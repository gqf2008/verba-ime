//! IPC 回环集成测试：验证 client/server 帧传输与流式事件。

use std::sync::Arc;
use std::time::Duration;

use verba_ipc::server::{serve, Outbound, RequestHandler};
use verba_ipc::{ConnectWait, VerbaClient};
use verba_protos::{
    request, response, stream_event, Chunk, Error as ProtoError, Final, LlmGenerate, Ok as OkMsg,
    Pong, Request, Response, StreamEvent,
};

fn unique_name(tag: &str) -> String {
    // 短后缀（进程 pid + 原子计数）：避免线程名过长导致 sun_path 超限
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let base = format!("verba-test-{tag}-{}-{n}", std::process::id());
    // Unix 用文件系统 socket（完整路径，测试临时目录）；Windows 用 `\\.\pipe\` 管道名。
    #[cfg(unix)]
    {
        std::env::temp_dir().join(base).display().to_string()
    }
    #[cfg(windows)]
    {
        format!(r"\\.\pipe\{base}")
    }
}

/// 带整体时限的连接重试（Windows 命名管道对不存在目标立即报错）。
fn connect_with_retry(name: &str, deadline: Duration) -> VerbaClient {
    let start = std::time::Instant::now();
    loop {
        match VerbaClient::connect_named(name, ConnectWait::Nonblocking) {
            Ok(c) => return c,
            Err(_) if start.elapsed() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => panic!("连接失败: {e}"),
        }
    }
}

struct TestHandler;

#[async_trait::async_trait]
impl RequestHandler for TestHandler {
    async fn handle(&self, _conn_id: u64, req: Request, out: Outbound) {
        let id = req.id;
        match req.kind {
            Some(request::Kind::Ping(_)) => {
                let _ = out
                    .response(&Response {
                        id,
                        kind: Some(response::Kind::Pong(Pong {
                            version: "test".into(),
                        })),
                    })
                    .await;
            }
            Some(request::Kind::LlmGenerate(_)) => {
                let _ = out
                    .response(&Response {
                        id,
                        kind: Some(response::Kind::Ok(OkMsg {})),
                    })
                    .await;
                for part in ["你", "好", "世界"] {
                    let _ = out
                        .event(&StreamEvent {
                            id,
                            kind: Some(stream_event::Kind::Chunk(Chunk { text: part.into() })),
                        })
                        .await;
                }
                let _ = out
                    .event(&StreamEvent {
                        id,
                        kind: Some(stream_event::Kind::Final(Final {
                            text: "你好世界".into(),
                        })),
                    })
                    .await;
            }
            Some(request::Kind::LlmCancel(_)) => {
                let _ = out
                    .response(&Response {
                        id,
                        kind: Some(response::Kind::Ok(OkMsg {})),
                    })
                    .await;
            }
            Some(request::Kind::ApiKeySet(_)) => {
                let _ = out
                    .response(&Response {
                        id,
                        kind: Some(response::Kind::Ok(OkMsg {})),
                    })
                    .await;
            }
            _ => {
                let _ = out
                    .response(&Response {
                        id,
                        kind: Some(response::Kind::Error(ProtoError {
                            code: 1,
                            message: "not implemented".into(),
                        })),
                    })
                    .await;
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn ping_roundtrip() {
    let name = unique_name("ping");
    let handler = Arc::new(TestHandler);
    let server_name = name.clone();
    let server = tokio::spawn(async move {
        let _ = serve(&server_name, handler).await;
    });

    let mut client = connect_with_retry(&name, Duration::from_secs(5));
    let version = client.ping().expect("ping 成功");
    assert_eq!(version, "test");
    server.abort();
    let _ = server.await;
}

#[tokio::test(flavor = "multi_thread")]
async fn llm_stream_roundtrip() {
    let name = unique_name("llm");
    let handler = Arc::new(TestHandler);
    let server_name = name.clone();
    let server = tokio::spawn(async move {
        let _ = serve(&server_name, handler).await;
    });

    let mut client = connect_with_retry(&name, Duration::from_secs(5));
    let id = client
        .llm_start("你好", None, None, None, None, 0)
        .expect("llm_start");
    let mut parts = Vec::new();
    loop {
        let evt = client.next_event(id).expect("next_event");
        match evt.kind {
            Some(stream_event::Kind::Chunk(c)) => parts.push(c.text),
            Some(stream_event::Kind::Final(_)) => break,
            Some(stream_event::Kind::Error(e)) => panic!("服务端错误: {e:?}"),
            Some(stream_event::Kind::Candidates(_)) => panic!("LLM 流不应出现候选事件"),
            None => panic!("空事件"),
        }
    }
    assert_eq!(parts, ["你", "好", "世界"]);
    server.abort();
    let _ = server.await;
}

#[tokio::test(flavor = "multi_thread")]
async fn api_key_set_roundtrip() {
    let name = unique_name("apikey");
    let handler = Arc::new(TestHandler);
    let server_name = name.clone();
    let server = tokio::spawn(async move {
        let _ = serve(&server_name, handler).await;
    });

    let mut client = connect_with_retry(&name, Duration::from_secs(5));
    client.set_api_key("sk-test").expect("set_api_key 成功");
    client.set_api_key("").expect("set_api_key 清空成功");
    server.abort();
    let _ = server.await;
}
/// 捕获 LLM 生成请求的 handler（验证多模态 image 字段回环）。
struct CapturingHandler {
    captured: std::sync::Arc<std::sync::Mutex<Option<LlmGenerate>>>,
}

#[async_trait::async_trait]
impl RequestHandler for CapturingHandler {
    async fn handle(&self, _conn_id: u64, req: Request, out: Outbound) {
        match req.kind {
            Some(request::Kind::LlmGenerate(g)) => {
                *self.captured.lock().unwrap() = Some(g.clone());
                let _ = out
                    .response(&Response {
                        id: req.id,
                        kind: Some(response::Kind::Ok(OkMsg {})),
                    })
                    .await;
                let _ = out
                    .event(&StreamEvent {
                        id: req.id,
                        kind: Some(stream_event::Kind::Final(Final { text: "ok".into() })),
                    })
                    .await;
            }
            _ => {
                let _ = out
                    .response(&Response {
                        id: req.id,
                        kind: Some(response::Kind::Error(ProtoError {
                            code: 1,
                            message: "not implemented".into(),
                        })),
                    })
                    .await;
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn llm_vision_image_roundtrip() {
    let name = unique_name("vision");
    let captured = std::sync::Arc::new(std::sync::Mutex::new(None::<LlmGenerate>));
    let handler = Arc::new(CapturingHandler {
        captured: captured.clone(),
    });
    let server_name = name.clone();
    let server = tokio::spawn(async move {
        let _ = serve(&server_name, handler).await;
    });

    let mut client = connect_with_retry(&name, Duration::from_secs(5));
    let img = b"\x89PNG fake vision bytes".to_vec();
    let id = client
        .llm_start("看图", None, None, None, Some(("image/png", &img)), 0)
        .expect("llm_start image");
    loop {
        let evt = client.next_event(id).expect("next_event");
        if matches!(evt.kind, Some(stream_event::Kind::Final(_))) {
            break;
        }
    }
    server.abort();
    let _ = server.await;

    let got = captured
        .lock()
        .unwrap()
        .clone()
        .expect("handler 应收到 LlmGenerate");
    assert_eq!(got.image.as_deref(), Some(img.as_slice()));
    assert_eq!(got.image_mime.as_deref(), Some("image/png"));
}

/// 捕获 LlmCancel 请求 id 的 handler：验证取消请求命中原始目标 id。
struct CancelCapturingHandler {
    captured: std::sync::Arc<std::sync::Mutex<Option<u64>>>,
}

#[async_trait::async_trait]
impl RequestHandler for CancelCapturingHandler {
    async fn handle(&self, _conn_id: u64, req: Request, out: Outbound) {
        match req.kind {
            Some(request::Kind::LlmGenerate(_)) => {
                let _ = out
                    .response(&Response {
                        id: req.id,
                        kind: Some(response::Kind::Ok(OkMsg {})),
                    })
                    .await;
            }
            Some(request::Kind::LlmCancel(_)) => {
                // daemon 端正是用 (conn_id, req.id) 从 cancels 表移除 token。
                *self.captured.lock().unwrap() = Some(req.id);
                let _ = out
                    .response(&Response {
                        id: req.id,
                        kind: Some(response::Kind::Ok(OkMsg {})),
                    })
                    .await;
            }
            _ => {
                let _ = out
                    .response(&Response {
                        id: req.id,
                        kind: Some(response::Kind::Error(ProtoError {
                            code: 1,
                            message: "not implemented".into(),
                        })),
                    })
                    .await;
            }
        }
    }
}

/// 回归测试：llm_cancel 必须用目标请求 id 发送，daemon 才能命中取消 token。
#[tokio::test(flavor = "multi_thread")]
async fn llm_cancel_uses_target_request_id() {
    let name = unique_name("cancel");
    let captured = Arc::new(std::sync::Mutex::new(None));
    let handler = Arc::new(CancelCapturingHandler {
        captured: captured.clone(),
    });
    let server_name = name.clone();
    let server = tokio::spawn(async move {
        let _ = serve(&server_name, handler).await;
    });

    let mut client = connect_with_retry(&name, Duration::from_secs(5));
    let target = client
        .llm_start("你好", None, None, None, None, 0)
        .expect("llm_start");
    client.llm_cancel(target).expect("llm_cancel 成功");

    let seen = captured.lock().unwrap().expect("收到 LlmCancel 请求");
    assert_eq!(
        seen, target,
        "LlmCancel 必须携带目标请求 id（修复前为自增新 id，取消静默失效）"
    );
    server.abort();
    let _ = server.await;
}
