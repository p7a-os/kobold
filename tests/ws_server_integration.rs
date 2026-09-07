//! Integration tests for Northbound WebSocket server, authentication, and dual-transport broadcast.

use std::future::Future;
use std::time::Duration;
use tempfile::tempdir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, watch};

use fastwebsockets::{FragmentCollector, OpCode, WebSocketError};
use hyper_util::rt::TokioIo;
use kobold_core::daemon::{Daemon, DaemonClient, DaemonConfig};
use kobold_core::ws::generate_token;
use kobold_proto::agui;
use kobold_proto::codec;
use kobold_proto::northbound::{ClientFrame, ClientServerFrame};
use kobold_proto::{Command, IncomingFrame};

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

async fn find_free_port() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind port 0");
    let port = listener.local_addr().expect("local addr").port();
    drop(listener);
    port
}

async fn wait_for_port(port: u16) -> tokio::net::TcpStream {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while tokio::time::Instant::now() < deadline {
        if let Ok(stream) = tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
            return stream;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("tcp connect timeout")
}

async fn connect_ws(
    port: u16,
    token: &str,
    use_bearer: bool,
) -> Result<FragmentCollector<TokioIo<hyper::upgrade::Upgraded>>, WebSocketError> {
    let stream = wait_for_port(port).await;

    let uri = if use_bearer {
        format!("http://127.0.0.1:{port}/ws")
    } else {
        format!("http://127.0.0.1:{port}/ws?token={token}")
    };

    let mut req_builder = hyper::Request::builder()
        .method("GET")
        .uri(uri)
        .header("Host", format!("127.0.0.1:{port}"))
        .header(hyper::header::UPGRADE, "websocket")
        .header(hyper::header::CONNECTION, "upgrade")
        .header(
            "Sec-WebSocket-Key",
            fastwebsockets::handshake::generate_key(),
        )
        .header("Sec-WebSocket-Version", "13");

    if use_bearer {
        req_builder = req_builder.header("Authorization", format!("Bearer {token}"));
    }

    let req = req_builder
        .body(http_body_util::Empty::<bytes::Bytes>::new())
        .expect("build request");

    let (ws, _) = fastwebsockets::handshake::client(&SpawnExecutor, req, stream).await?;
    Ok(FragmentCollector::new(ws))
}

#[tokio::test]
async fn test_ws_static_ui_serving() {
    let tmp = tempdir().expect("tempdir");
    let sock = tmp.path().join("ws_static.sock");
    let port = find_free_port().await;
    let token = generate_token();

    let config = DaemonConfig::new(&sock, tmp.path()).with_ws(port, &token);
    let daemon = Daemon::bind(config).expect("bind daemon");

    let (cmd_tx, _) = mpsc::unbounded_channel::<Command>();
    let (_, adapter_rx) = mpsc::channel::<IncomingFrame>(32);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let daemon_handle = tokio::spawn(async move {
        let _ = daemon.run(cmd_tx, adapter_rx, shutdown_rx).await;
    });

    // Make raw HTTP GET /
    let mut stream = wait_for_port(port).await;
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .await
        .expect("write request");

    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .await
        .expect("read response");

    assert!(response.starts_with("HTTP/1.1 200 OK"));
    assert!(response.contains("text/html; charset=utf-8"));
    assert!(response.contains("Kobold Web Companion"));
    assert!(response.contains("connectWS"));

    let _ = shutdown_tx.send(true);
    let _ = daemon_handle.await;
}

#[tokio::test]
async fn test_ws_authentication_enforcement() {
    let tmp = tempdir().expect("tempdir");
    let sock = tmp.path().join("ws_auth.sock");
    let port = find_free_port().await;
    let valid_token = generate_token();

    let config = DaemonConfig::new(&sock, tmp.path()).with_ws(port, &valid_token);
    let daemon = Daemon::bind(config).expect("bind daemon");

    let (cmd_tx, _) = mpsc::unbounded_channel::<Command>();
    let (_, adapter_rx) = mpsc::channel::<IncomingFrame>(32);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let daemon_handle = tokio::spawn(async move {
        let _ = daemon.run(cmd_tx, adapter_rx, shutdown_rx).await;
    });

    // 1. Missing token -> 401 Unauthorized
    let mut stream = wait_for_port(port).await;
    stream
        .write_all(b"GET /ws HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n")
        .await
        .expect("write");
    let mut buf = [0u8; 1024];
    let n = stream.read(&mut buf).await.expect("read");
    let resp = String::from_utf8_lossy(&buf[..n]);
    assert!(resp.starts_with("HTTP/1.1 401 Unauthorized"));
    assert!(resp.contains("invalid or missing token"));

    // 2. Invalid token -> 401 Unauthorized
    let mut stream = wait_for_port(port).await;
    stream
        .write_all(b"GET /ws?token=wrong-token-abc HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n")
        .await
        .expect("write");
    let mut buf = [0u8; 1024];
    let n = stream.read(&mut buf).await.expect("read");
    let resp = String::from_utf8_lossy(&buf[..n]);
    assert!(resp.starts_with("HTTP/1.1 401 Unauthorized"));
    assert!(resp.contains("invalid or missing token"));

    // 3. Valid token via Query Parameter -> 101 Switching Protocols
    let mut ws_query = connect_ws(port, &valid_token, false)
        .await
        .expect("ws handshake via query token");
    let initial_frame = ws_query.read_frame().await.expect("read snapshot");
    assert_eq!(initial_frame.opcode, OpCode::Text);

    // 4. Valid token via Bearer Header -> 101 Switching Protocols
    let mut ws_bearer = connect_ws(port, &valid_token, true)
        .await
        .expect("ws handshake via bearer token");
    let initial_frame = ws_bearer.read_frame().await.expect("read snapshot");
    assert_eq!(initial_frame.opcode, OpCode::Text);

    let _ = shutdown_tx.send(true);
    let _ = daemon_handle.await;
}

#[tokio::test]
async fn test_ws_and_uds_dual_broadcast_parity() {
    let tmp = tempdir().expect("tempdir");
    let sock = tmp.path().join("dual_bc.sock");
    let port = find_free_port().await;
    let token = generate_token();

    let config = DaemonConfig::new(&sock, tmp.path()).with_ws(port, &token);
    let daemon = Daemon::bind(config).expect("bind daemon");

    let (cmd_tx, _cmd_rx) = mpsc::unbounded_channel::<Command>();
    let (adapter_tx, adapter_rx) = mpsc::channel::<IncomingFrame>(32);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let daemon_handle = tokio::spawn(async move {
        let _ = daemon.run(cmd_tx, adapter_rx, shutdown_rx).await;
    });

    // 1. Connect Client 1 via UDS (gets RW lease)
    let uds_client = DaemonClient::connect(&sock).await.expect("uds connect");
    let (uds_tx, mut uds_rx) = uds_client.into_channels();

    // Drain initial snapshot on UDS
    let snap = uds_rx.recv().await.expect("uds snap");
    assert!(matches!(snap, ClientServerFrame::Snapshot { .. }));

    // 2. Connect Client 2 via WebSocket (becomes Follower / Read-Only)
    let mut ws_client = connect_ws(port, &token, false).await.expect("ws connect");

    // Drain initial snapshot on WS
    let ws_msg1 = ws_client.read_frame().await.expect("ws msg1");
    let ws_text1 = std::str::from_utf8(&ws_msg1.payload).expect("utf8");
    let snap_frame: ClientServerFrame = codec::decode(ws_text1).expect("decode ws snap");
    assert!(matches!(snap_frame, ClientServerFrame::Snapshot { .. }));

    // Drain Follower notice on WS
    let ws_msg2 = ws_client.read_frame().await.expect("ws msg2");
    let ws_text2 = std::str::from_utf8(&ws_msg2.payload).expect("utf8");
    let notice_frame: ClientServerFrame = codec::decode(ws_text2).expect("decode ws notice");
    match notice_frame {
        ClientServerFrame::Notice { text } => {
            assert!(text.contains("read-only mode"));
        }
        other => panic!("expected Notice, got {:?}", other),
    }

    // 3. Emit an AG-UI event from the adapter
    let event = agui::Incoming::TextMessageContent {
        base: agui::Base::default(),
        message_id: "test-delta-1".into(),
        delta: "dual-broadcast-verified".into(),
    };
    adapter_tx
        .send(IncomingFrame::Event {
            lane: "main".into(),
            event,
        })
        .await
        .expect("send adapter frame");

    // 4. Verify UDS client receives the event
    let uds_event = tokio::time::timeout(Duration::from_secs(2), uds_rx.recv())
        .await
        .expect("uds timeout")
        .expect("uds frame");
    match uds_event {
        ClientServerFrame::Event { event, .. } => match event {
            agui::Incoming::TextMessageContent { delta, .. } => {
                assert_eq!(delta, "dual-broadcast-verified");
            }
            other => panic!("expected TextMessageContent, got {:?}", other),
        },
        other => panic!("expected Event, got {:?}", other),
    }

    // 5. Verify WS client receives the identical event
    let ws_msg3 = tokio::time::timeout(Duration::from_secs(2), ws_client.read_frame())
        .await
        .expect("ws timeout")
        .expect("ws frame");
    let ws_text3 = std::str::from_utf8(&ws_msg3.payload).expect("utf8");
    let ws_event: ClientServerFrame = codec::decode(ws_text3).expect("decode ws event");
    match ws_event {
        ClientServerFrame::Event { event, .. } => match event {
            agui::Incoming::TextMessageContent { delta, .. } => {
                assert_eq!(delta, "dual-broadcast-verified");
            }
            other => panic!("expected TextMessageContent on ws, got {:?}", other),
        },
        other => panic!("expected Event on ws, got {:?}", other),
    }

    // 6. Shutdown
    let _ = uds_tx.send(ClientFrame::Detach);
    let _ = shutdown_tx.send(true);
    let _ = daemon_handle.await;
}
