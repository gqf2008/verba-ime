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
use interprocess::local_socket::{ConnectOptions, GenericFilePath, Stream as LocalSocketStream};
use interprocess::ConnectWaitMode;
use prost::Message as _;
use verba_protos::{
    request, response, stream_event, ApiKeySet, AsrTranscribe, LlmCancel, LlmCandidates,
    LlmGenerate, OcrRecognize, Ping, Request, Response, RimeCandidates, StreamEvent, TtsSynthesize,
};

use crate::codec::{encode_frame, read_frame};
use crate::error::IpcError;
use crate::name::default_socket_spec;

/// 默认套接字名（旧值，保持兼容导出；实际默认见 [`default_socket_spec`]）。
pub const DEFAULT_SOCKET_NAME: &str = "verba-ime";

/// `connect_verified` 验活握手的读超时：仅作用于握手期间（防对端接受连接但
/// 永不应答时把前端 UI 线程一起挂起）。正常 daemon 本地回 Pong 为微秒级，
/// 取宽裕的 5s 仅为兜底卡死场景。仅 Unix 使用（Windows 命名管道不支持 I/O 超时）。
#[cfg(unix)]
const HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

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
    /// 连接默认套接字（Unix 为用户数据目录全路径 / Windows per-user 管道）。
    pub fn connect() -> Result<Self, IpcError> {
        Self::connect_named(&default_socket_spec(), ConnectWait::Nonblocking)
    }

    /// 连接默认套接字并做验活握手（架构审查 P0-1）：连接成功不代表对端是
    /// 真实 daemon，能回 Pong 的才信任（防冒充者窃取 `key set` 的密钥、
    /// 提示词、截图、录音）。**发送敏感数据的所有调用方都应使用本构造函数**，
    /// 而不是裸 [`VerbaClient::connect`]（socket 目录 0700 / per-user 管道
    /// 之上，握手是纵深防御）。
    ///
    /// 握手在 **Unix** 带**有界读超时**（`HANDSHAKE_TIMEOUT`）：本函数跑在前端 UI
    /// 线程（TSF/IMK 回调），对端「接受连接但永不应答」（daemon 卡死但进程/socket
    /// 仍在）时，无超时的阻塞 `ping` 会把宿主应用一起挂起。超时即放弃并返回
    /// 错误（上层 `ensure_daemon` 重启 daemon / 重试）。超时仅在握手期间设置，
    /// 完成后清除，避免影响后续流式读取。**Windows 命名管道不支持 I/O 超时**，
    /// 故仅做 Pong 验活、无有界超时（阻塞语义同裸 [`VerbaClient::connect`]）。
    pub fn connect_verified() -> Result<Self, IpcError> {
        let mut client = Self::connect()?;
        // 读超时仅在 Unix 设置：interprocess 的 Windows 命名管道不支持 I/O 超时
        // （set_recv_timeout 恒返回 Unsupported，复审 P0——若在 Windows 传播该错，
        // 握手在发 Ping 前就失败，整个前端/settings 连不上 daemon）。Windows 下
        // 握手仍做 Pong 验活，仅退化为无有界超时（与裸 connect 一致的阻塞语义）。
        #[cfg(unix)]
        {
            client.stream.set_recv_timeout(Some(HANDSHAKE_TIMEOUT))?;
        }
        let ping_result = client.ping();
        // 无论握手成败都清除超时，恢复后续流式读的阻塞语义。
        #[cfg(unix)]
        {
            let _ = client.stream.set_recv_timeout(None);
        }
        ping_result?;
        Ok(client)
    }

    /// 连接指定套接字名（Unix 为文件系统路径；Windows 为 `\\.\pipe\...` 管道名）。
    pub fn connect_named(name: &str, wait: ConnectWait) -> Result<Self, IpcError> {
        let wait_mode = match wait {
            ConnectWait::Nonblocking | ConnectWait::Block => ConnectWaitMode::Unbounded,
        };
        // GenericFilePath：Unix 原样作为 UDS 路径（权限受用户数据目录保护）；
        // Windows 把 `\\.\pipe\` 前缀映射为命名管道（per-user 名称隔离）。
        let name = name.to_fs_name::<GenericFilePath>()?;
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
    ///
    /// `session_id`：多轮上下文会话标识（每控制器/前端生成唯一值；0 = 旧客户端
    /// 默认共享槽）。服务端按此分组 AI 历史，实现多会话上下文隔离（架构审查会话维度）。
    pub fn llm_start(
        &mut self,
        prompt: &str,
        system: Option<&str>,
        temperature: Option<f32>,
        max_tokens: Option<i32>,
        image: Option<(&str, &[u8])>,
        session_id: u64,
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
                image: image.map(|(_, data)| data.to_vec()),
                image_mime: image.map(|(mime, _)| mime.to_owned()),
                session_id,
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
        // 注意：Request.id 必须等于目标请求 id——daemon 端按 (conn_id, req.id)
        // 从 cancels 表移除对应的 CancellationToken（见 verba-daemon handler 的
        // LlmCancel 分支；conn_id 由服务端按连接分配）。
        // 若用新自增 id，remove 永远取不到 token，取消静默失效。
        let req = Request {
            id: request_id,
            kind: Some(request::Kind::LlmCancel(LlmCancel {})),
        };
        self.write_request(&req)?;
        loop {
            let frame = self.read_frame_blocking()?;
            if let Ok(resp) = Response::decode(frame.as_slice()) {
                if resp.id == request_id {
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

    /// 设置/清除 API Key（空字符串 = 删除）：写系统密钥库并热更新 daemon 内存，
    /// 无需重启 daemon 即可让 LLM / 在线 ASR / 在线 TTS 生效。
    pub fn set_api_key(&mut self, key: &str) -> Result<(), IpcError> {
        let id = self.new_id();
        let req = Request {
            id,
            kind: Some(request::Kind::ApiKeySet(ApiKeySet {
                key: key.to_owned(),
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
