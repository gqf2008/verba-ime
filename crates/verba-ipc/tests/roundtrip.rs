//! IPC 回环集成测试：验证 client/server 帧传输与流式事件。

use std::sync::Arc;
use std::time::Duration;

use verba_ipc::server::{serve, Outbound, RequestHandler};
use verba_ipc::{ConnectWait, VerbaClient};
use verba_protos::{
    request, response, stream_event, Chunk, Error as ProtoError, Final, Ok as OkMsg, Pong, Request,
    Response, StreamEvent,
};

fn unique_name(tag: &str) -> String {
    format!(
        "verba-test-{tag}-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("t")
    )
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
    async fn handle(&self, req: Request, out: Outbound) {
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
        .llm_start("你好", None, None, None)
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
