//! 阻塞式 IPC 客户端（单句柄、单线程顺序读写）。
//!
//! 设计约束（Windows 命名管道实测）：
//!
//! 1. `set_nonblocking`（PIPE_NOWAIT）会把「无数据」与「对端关闭」混淆，不用。
//! 2. `try_clone` 出的第二个句柄在读取分帧数据时会出现假 EOF，不用。
//!
//! 因此每个 `VerbaClient` 独占一个连接、同一线程顺序「写请求→读响应」。
//! 需要流式并行时（如 TSF 前端），另起一个线程持有独立连接即可，
//! daemon 的取消按全局请求 id 生效，不依赖连接。
//!
//! 超时策略：服务端协议保证每个请求必有响应（Ping/Config/Ok/Error），
//! LLM 流必有 `Final` 或 `Error`；daemon 崩溃则管道关闭→`ConnectionClosed`。
//! 因此阻塞读是安全的。

use std::collections::VecDeque;
use std::io::Write;

use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{ConnectOptions, GenericNamespaced, Stream as LocalSocketStream};
use interprocess::ConnectWaitMode;
use prost::Message as _;
use verba_protos::{
    request, response, stream_event, AsrTranscribe, LlmCancel, LlmCandidates, LlmGenerate,
    OcrRecognize, Ping, Request, Response, RimeCandidates, StreamEvent, TtsSynthesize,
};

use crate::codec::{encode_frame, read_frame};
use crate::error::IpcError;

/// 默认套接字名（Windows 映射为命名管道 `\\.\pipe\verba-ime`）。
pub const DEFAULT_SOCKET_NAME: &str = "verba-ime";

/// 连接等待策略。
///
/// 注意：Windows 命名管道对「目标管道不存在」总是立即报错，
/// 调用方应在失败时自行重试（如 daemon 启动期间的退避重试）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectWait {
    /// 语义上的非阻塞连接：失败由调用方重试。
    Nonblocking,
    /// 阻塞等待服务端就绪（目标管道不存在时仍立即报错）。
    Block,
}

/// 阻塞式 IPC 客户端（单句柄）。
#[derive(Debug)]
pub struct VerbaClient {
    stream: LocalSocketStream,
    next_id: u64,
    /// 读取到但尚未被 `next_event` 消费的事件。
    pending_events: VecDeque<StreamEvent>,
}

impl VerbaClient {
    /// 连接默认套接字。
    pub fn connect() -> Result<Self, IpcError> {
        Self::connect_named(DEFAULT_SOCKET_NAME, ConnectWait::Nonblocking)
    }

    /// 连接指定套接字名。
    pub fn connect_named(name: &str, wait: ConnectWait) -> Result<Self, IpcError> {
        let name = name.to_ns_name::<GenericNamespaced>()?;
        let wait_mode = match wait {
            ConnectWait::Nonblocking | ConnectWait::Block => ConnectWaitMode::Unbounded,
        };
        let stream = ConnectOptions::new()
            .name(name)
            .wait_mode(wait_mode)
            .connect_sync()?;
        Ok(Self {
            stream,
            next_id: 1,
            pending_events: VecDeque::new(),
        })
    }

    /// 健康检查，返回服务端版本。
    pub fn ping(&mut self) -> Result<String, IpcError> {
        let id = self.new_id();
        let req = Request {
            id,
            kind: Some(request::Kind::Ping(Ping {})),
        };
        let resp = self.request(req)?;
        match resp.kind {
            Some(response::Kind::Pong(p)) => Ok(p.version),
            _ => Err(IpcError::Protocol("期望 Pong 响应".into())),
        }
    }

    /// 发起 LLM 流式生成，返回请求 id；服务端以 Ok 确认后开始推送事件。
    pub fn llm_start(
        &mut self,
        prompt: &str,
        system: Option<&str>,
        temperature: Option<f32>,
        max_tokens: Option<i32>,
    ) -> Result<u64, IpcError> {
        let id = self.new_id();
        let req = Request {
            id,
            kind: Some(request::Kind::LlmGenerate(LlmGenerate {
                prompt: prompt.to_owned(),
                system: system.map(str::to_owned),
                temperature,
                max_tokens,
                stream: true,
            })),
        };
        self.write_request(&req)?;
        loop {
            let frame = self.read_frame_blocking()?;
            // 注意：protobuf 解码宽容，须先按 Response 解码（事件帧解码为 Response 会失败）。
            if let Ok(resp) = Response::decode(frame.as_slice()) {
                if resp.id != id {
                    return Err(IpcError::Protocol("响应 id 不匹配".into()));
                }
                match resp.kind {
                    Some(response::Kind::Ok(_)) => return Ok(id),
                    Some(response::Kind::Error(e)) => {
                        return Err(IpcError::Server {
                            code: e.code,
                            message: e.message,
                        });
                    }
                    _ => return Err(IpcError::Protocol("期望 Ok 响应".into())),
                }
            }
            if let Ok(evt) = StreamEvent::decode(frame.as_slice()) {
                if evt.id == id {
                    self.pending_events.push_back(evt);
                }
                continue;
            }
            return Err(IpcError::Protocol("无法解码响应".into()));
        }
    }

    /// 发起候选融合：请求 LLM 为拼音补充候选，返回请求 id；
    /// 服务端以 Ok 确认后开始推送 `Candidates` 事件（done=true 结束）。
    pub fn llm_candidates_start(
        &mut self,
        pinyin: &str,
        dictionary: &[String],
        max_candidates: i32,
    ) -> Result<u64, IpcError> {
        let id = self.new_id();
        let req = Request {
            id,
            kind: Some(request::Kind::LlmCandidates(LlmCandidates {
                pinyin: pinyin.to_owned(),
                dictionary: dictionary.to_vec(),
                max_candidates,
            })),
        };
        self.write_request(&req)?;
        loop {
            let frame = self.read_frame_blocking()?;
            if let Ok(resp) = Response::decode(frame.as_slice()) {
                if resp.id != id {
                    return Err(IpcError::Protocol("响应 id 不匹配".into()));
                }
                match resp.kind {
                    Some(response::Kind::Ok(_)) => return Ok(id),
                    Some(response::Kind::Error(e)) => {
                        return Err(IpcError::Server {
                            code: e.code,
                            message: e.message,
                        });
                    }
                    _ => return Err(IpcError::Protocol("期望 Ok 响应".into())),
                }
            }
            if let Ok(evt) = StreamEvent::decode(frame.as_slice()) {
                if evt.id == id {
                    self.pending_events.push_back(evt);
                }
                continue;
            }
            return Err(IpcError::Protocol("无法解码响应".into()));
        }
    }

    /// 查询 Rime 引擎候选（config 引擎=rime）：同步返回候选列表（一个
    /// `Candidates` 事件，`done=true` 结束）。
    pub fn rime_candidates(
        &mut self,
        input: &str,
        schema: &str,
        max_candidates: i32,
    ) -> Result<Vec<String>, IpcError> {
        let id = self.new_id();
        let req = Request {
            id,
            kind: Some(request::Kind::RimeCandidates(RimeCandidates {
                input: input.to_owned(),
                schema: schema.to_owned(),
                max_candidates,
            })),
        };
        self.write_request(&req)?;
        let mut out = Vec::new();
        loop {
            let frame = self.read_frame_blocking()?;
            // 事件帧优先：protobuf 宽松解码下，Candidates 事件可能被 Response::decode
            // 误判为 field 5 的 Text（同为 length-delimited message）。
            if let Ok(evt) = StreamEvent::decode(frame.as_slice()) {
                if evt.id != id {
                    self.pending_events.push_back(evt);
                    continue;
                }
                match evt.kind {
                    Some(stream_event::Kind::Candidates(c)) => {
                        out.extend(c.candidates);
                        if c.done {
                            return Ok(out);
                        }
                    }
                    Some(stream_event::Kind::Error(e)) => {
                        return Err(IpcError::Server {
                            code: e.code,
                            message: e.message,
                        });
                    }
                    // Ok 响应在 StreamEvent 宽松解码下表现为空 Final，忽略继续读。
                    _ => continue,
                }
            }
            if let Ok(resp) = Response::decode(frame.as_slice()) {
                if resp.id != id {
                    return Err(IpcError::Protocol("响应 id 不匹配".into()));
                }
                match resp.kind {
                    Some(response::Kind::Ok(_)) => continue,
                    Some(response::Kind::Error(e)) => {
                        return Err(IpcError::Server {
                            code: e.code,
                            message: e.message,
                        });
                    }
                    _ => return Err(IpcError::Protocol("期望 Ok 响应".into())),
                }
            }
            return Err(IpcError::Protocol("无法解码响应".into()));
        }
    }

    /// 取消指定请求的流式生成（daemon 按全局请求 id 取消）。
    pub fn llm_cancel(&mut self, request_id: u64) -> Result<(), IpcError> {
        log::debug!("取消请求 {request_id}");
        let id = self.new_id();
        let req = Request {
            id,
            kind: Some(request::Kind::LlmCancel(LlmCancel {})),
        };
        self.write_request(&req)?;
        loop {
            let frame = self.read_frame_blocking()?;
            if let Ok(resp) = Response::decode(frame.as_slice()) {
                if resp.id == id {
                    return Ok(());
                }
            }
        }
    }

    /// 拉取指定请求的下一个流式事件（阻塞）。
    ///
    /// 依赖服务端协议保证：LLM 流必有 `Final` 或 `Error` 收尾。
    pub fn next_event(&mut self, request_id: u64) -> Result<StreamEvent, IpcError> {
        if let Some(pos) = self.pending_events.iter().position(|e| e.id == request_id) {
            return Ok(self.pending_events.remove(pos).expect("存在"));
        }
        loop {
            let frame = self.read_frame_blocking()?;
            if let Ok(evt) = StreamEvent::decode(frame.as_slice()) {
                if evt.id == request_id {
                    return Ok(evt);
                }
                self.pending_events.push_back(evt);
            }
        }
    }

    /// 发送请求并读取匹配的响应（阻塞）。
    fn request(&mut self, req: Request) -> Result<Response, IpcError> {
        self.write_request(&req)?;
        loop {
            let frame = self.read_frame_blocking()?;
            if let Ok(resp) = Response::decode(frame.as_slice()) {
                if resp.id == req.id {
                    return Ok(resp);
                }
                log::warn!("忽略不匹配响应 id={}", resp.id);
                continue;
            }
            if let Ok(evt) = StreamEvent::decode(frame.as_slice()) {
                self.pending_events.push_back(evt);
                continue;
            }
            return Err(IpcError::Protocol("无法解码响应".into()));
        }
    }

    fn read_frame_blocking(&mut self) -> Result<Vec<u8>, IpcError> {
        read_frame(&mut self.stream).map_err(Into::into)
    }

    fn write_request(&mut self, req: &Request) -> Result<(), IpcError> {
        let mut payload = Vec::new();
        req.encode(&mut payload)?;
        let frame = encode_frame(&payload)?;
        self.stream.write_all(&frame)?;
        Ok(())
    }

    fn new_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }
}

impl VerbaClient {
    /// 读取配置（键值表）。
    pub fn get_config(&mut self) -> Result<std::collections::HashMap<String, String>, IpcError> {
        let id = self.new_id();
        let req = Request {
            id,
            kind: Some(request::Kind::GetConfig(verba_protos::GetConfig {})),
        };
        let resp = self.request(req)?;
        match resp.kind {
            Some(response::Kind::Config(c)) => Ok(c.values),
            _ => Err(IpcError::Protocol("期望 Config 响应".into())),
        }
    }

    /// 写入配置（键值表）。
    pub fn set_config(
        &mut self,
        values: std::collections::HashMap<String, String>,
    ) -> Result<(), IpcError> {
        let id = self.new_id();
        let req = Request {
            id,
            kind: Some(request::Kind::SetConfig(verba_protos::SetConfig { values })),
        };
        let resp = self.request(req)?;
        match resp.kind {
            Some(response::Kind::Ok(_)) => Ok(()),
            Some(response::Kind::Error(e)) => Err(IpcError::Server {
                code: e.code,
                message: e.message,
            }),
            _ => Err(IpcError::Protocol("期望 Ok 响应".into())),
        }
    }

    /// 请求 TTS 合成，返回（音频格式, 音频字节）。
    pub fn tts_synthesize(
        &mut self,
        text: &str,
        voice: Option<&str>,
    ) -> Result<(String, Vec<u8>), IpcError> {
        let id = self.new_id();
        let req = Request {
            id,
            kind: Some(request::Kind::TtsSynthesize(TtsSynthesize {
                text: text.to_owned(),
                voice: voice.map(str::to_owned),
            })),
        };
        let resp = self.request(req)?;
        match resp.kind {
            Some(response::Kind::Audio(a)) => Ok((a.format, a.data)),
            Some(response::Kind::Error(e)) => Err(IpcError::Server {
                code: e.code,
                message: e.message,
            }),
            _ => Err(IpcError::Protocol("期望 Audio 响应".into())),
        }
    }

    /// 请求 OCR 识别，返回识别文字。
    pub fn ocr_recognize(&mut self, image: &[u8]) -> Result<String, IpcError> {
        let id = self.new_id();
        let req = Request {
            id,
            kind: Some(request::Kind::OcrRecognize(OcrRecognize {
                image: image.to_vec(),
            })),
        };
        let resp = self.request(req)?;
        match resp.kind {
            Some(response::Kind::Text(t)) => Ok(t.text),
            Some(response::Kind::Error(e)) => Err(IpcError::Server {
                code: e.code,
                message: e.message,
            }),
            _ => Err(IpcError::Protocol("期望 Text 响应".into())),
        }
    }

    /// 请求 ASR 转写，返回识别文字。
    pub fn asr_transcribe(&mut self, audio: &[u8]) -> Result<String, IpcError> {
        let id = self.new_id();
        let req = Request {
            id,
            kind: Some(request::Kind::AsrTranscribe(AsrTranscribe {
                audio: audio.to_vec(),
            })),
        };
        let resp = self.request(req)?;
        match resp.kind {
            Some(response::Kind::Text(t)) => Ok(t.text),
            Some(response::Kind::Error(e)) => Err(IpcError::Server {
                code: e.code,
                message: e.message,
            }),
            _ => Err(IpcError::Protocol("期望 Text 响应".into())),
        }
    }

    /// 切换输入法模式。
    pub fn set_mode(&mut self, mode: &str) -> Result<(), IpcError> {
        let id = self.new_id();
        let req = Request {
            id,
            kind: Some(request::Kind::SetMode(verba_protos::SetMode {
                mode: mode.to_owned(),
            })),
        };
        let resp = self.request(req)?;
        match resp.kind {
            Some(response::Kind::Ok(_)) => Ok(()),
            Some(response::Kind::Error(e)) => Err(IpcError::Server {
                code: e.code,
                message: e.message,
            }),
            _ => Err(IpcError::Protocol("期望 Ok 响应".into())),
        }
    }
}
