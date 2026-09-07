//! End-to-end tests for `koboldd` Northbound WebSocket server and Web Companion.

use std::future::Future;
use std::path::PathBuf;
use std::process::Command as StdCommand;
use std::time::Duration;
use tempfile::tempdir;

use fastwebsockets::{FragmentCollector, Frame, OpCode, Payload, WebSocketError};
use hyper_util::rt::TokioIo;
use kobold_core::daemon::default_socket_path;
use kobold_core::ws::auth_file_path;
use kobold_proto::codec;
use kobold_proto::northbound::{ClientFrame, ClientServerFrame};

struct SpawnExecutor;
impl<Fut> hyper::rt::Executor<Fut> for SpawnExecutor
where
    Fut: Future + Send + 'static,
    Fut::Output: Send + 'static,
{
    fn execute(&self, fut: Fut) {
        tokio::task::spawn(fut);
    }
}

fn find_binary(name: &str) -> PathBuf {
    let mut path = std::env::current_exe().expect("current test exe");
    path.pop(); // deps
    path.pop(); // debug
    let candidate = path.join(name);
    if candidate.exists() {
        return candidate;
    }
    let debug_path = PathBuf::from("target/debug").join(name);
    if debug_path.exists() {
        return debug_path;
    }
    let _ = StdCommand::new("cargo")
        .args(["build", "--bin", name])
        .output();
    if candidate.exists() {
        candidate
    } else {
        debug_path
    }
}

async fn find_free_port() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind port 0");
    let port = listener.local_addr().expect("local addr").port();
    drop(listener);
    port
}

async fn wait_for_port(port: u16) -> tokio::net::TcpStream {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        if let Ok(stream) = tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
            return stream;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("tcp connect timeout")
}

async fn connect_ws(
    port: u16,
    token: &str,
) -> Result<FragmentCollector<TokioIo<hyper::upgrade::Upgraded>>, WebSocketError> {
    let stream = wait_for_port(port).await;
    let uri = format!("http://127.0.0.1:{port}/ws?token={token}");

    let req = hyper::Request::builder()
        .method("GET")
        .uri(uri)
        .header("Host", format!("127.0.0.1:{port}"))
        .header(hyper::header::UPGRADE, "websocket")
        .header(hyper::header::CONNECTION, "upgrade")
        .header(
            "Sec-WebSocket-Key",
            fastwebsockets::handshake::generate_key(),
        )
        .header("Sec-WebSocket-Version", "13")
        .body(http_body_util::Empty::<bytes::Bytes>::new())
        .expect("build request");

    let (ws, _) = fastwebsockets::handshake::client(&SpawnExecutor, req, stream).await?;
    Ok(FragmentCollector::new(ws))
}

#[tokio::test]
async fn test_e2e_web_companion_flow() {
    let koboldd_bin = find_binary("koboldd");
    assert!(
        koboldd_bin.exists(),
        "koboldd binary must exist at {koboldd_bin:?}"
    );

    let tmp = tempdir().expect("tempdir");
    let session_id = format!("e2e-ws-{}", uuid::Uuid::now_v7());
    let socket = default_socket_path(&session_id);
    let port = find_free_port().await;

    // 1. Spawn koboldd with --ws-port and --mock
    let mut child = StdCommand::new(&koboldd_bin)
        .arg("--socket")
        .arg(&socket)
        .arg("--session")
        .arg(&session_id)
        .arg("--workdir")
        .arg(tmp.path())
        .arg("--ws-port")
        .arg(port.to_string())
        .arg("--mock")
        .spawn()
        .expect("spawn koboldd");

    // 2. Wait for auth token file to be created
    let auth_path = auth_file_path(&session_id);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut token = None;
    while tokio::time::Instant::now() < deadline {
        if auth_path.exists() {
            if let Ok(t) = std::fs::read_to_string(&auth_path) {
                if !t.trim().is_empty() {
                    token = Some(t.trim().to_string());
                    break;
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let token = token.expect("auth token must be written by koboldd");

    // 3. Connect WebSocket client
    let mut ws = connect_ws(port, &token)
        .await
        .expect("connect to websocket companion");

    // 4. Verify initial snapshot received over WebSocket
    let msg = ws.read_frame().await.expect("recv initial snapshot frame");
    assert_eq!(msg.opcode, OpCode::Text);
    let text = std::str::from_utf8(&msg.payload).expect("utf8 payload");
    let snap_frame: ClientServerFrame = codec::decode(text).expect("decode snapshot");
    assert!(matches!(snap_frame, ClientServerFrame::Snapshot { .. }));

    // 5. Send Prompt frame over WebSocket
    let prompt_frame = ClientFrame::Prompt {
        lane: "main".into(),
        text: "hello e2e web companion".into(),
    };
    let encoded_prompt = codec::encode(&prompt_frame).expect("encode prompt");
    let ws_out_frame = Frame::text(Payload::Owned(encoded_prompt.into_bytes()));
    ws.write_frame(ws_out_frame)
        .await
        .expect("write prompt frame to websocket");

    // 6. Send Detach and clean up
    let detach_frame = ClientFrame::Detach;
    let encoded_detach = codec::encode(&detach_frame).expect("encode detach");
    let ws_detach_frame = Frame::text(Payload::Owned(encoded_detach.into_bytes()));
    let _ = ws.write_frame(ws_detach_frame).await;

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_file(&socket);
    let _ = std::fs::remove_file(&auth_path);
}
