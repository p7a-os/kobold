//! Authenticated Northbound WebSocket & Web Companion Server.
//!
//! Exposes the Kobold Kernel over WebSocket for browser and remote clients,
//! serving a modern standalone Web Companion UI and duplex AG-UI streaming frames.

use std::convert::Infallible;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use fastwebsockets::{upgrade, FragmentCollectorRead, Frame, OpCode, Payload, WebSocketError};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc, watch};

use kobold_proto::codec;
use kobold_proto::northbound::{ClientFrame, ServerFrame};

use crate::daemon::DaemonMsg;

/// Default port for the WebSocket companion server.
pub const DEFAULT_WS_PORT: u16 = 8765;

/// Bundled standalone Web Companion UI HTML.
pub const WEB_COMPANION_HTML: &str = include_str!("web/index.html");

/// Monotonically increasing client counter for WebSocket connections (high-bit tagged).
static NEXT_WS_CLIENT_ID: AtomicUsize = AtomicUsize::new(0x8000_0000);

/// Generates a high-entropy 256-bit authentication token (64 hex characters).
pub fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    for b in &mut bytes {
        *b = fastrand::u8(..);
    }
    let mut s = String::with_capacity(64);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Validates an incoming token against the expected token in constant time.
pub fn validate_token(expected: &str, candidate: &str) -> bool {
    if expected.is_empty() || expected.len() != candidate.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in expected.as_bytes().iter().zip(candidate.as_bytes()) {
        diff |= a ^ b;
    }
    diff == 0
}

/// Returns the path to the auth token file: `/tmp/kobold-{uid}/{session_id}.auth`.
pub fn auth_file_path(session_id: &str) -> PathBuf {
    crate::session::session_runtime_dir().join(format!("{session_id}.auth"))
}

/// Saves the authentication token to `/tmp/kobold-{uid}/{session_id}.auth` with private 0600 permissions.
pub fn save_auth_token(session_id: &str, token: &str) -> io::Result<PathBuf> {
    let path = auth_file_path(session_id);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, token)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o600);
        let _ = fs::set_permissions(&path, perms);
    }

    Ok(path)
}

/// Extracts auth token from `Authorization: Bearer <TOKEN>` or `?token=<TOKEN>`.
fn extract_token<B>(req: &Request<B>) -> Option<String> {
    if let Some(auth) = req.headers().get("authorization") {
        if let Ok(s) = auth.to_str() {
            if let Some(token) = s.strip_prefix("Bearer ") {
                return Some(token.trim().to_string());
            }
        }
    }
    if let Some(query) = req.uri().query() {
        for pair in query.split('&') {
            if let Some(token) = pair.strip_prefix("token=") {
                return Some(token.to_string());
            }
        }
    }
    None
}

type ResponseBox = Response<BoxBody<Bytes, Infallible>>;

fn full_response(status: StatusCode, content_type: &str, body: impl Into<Bytes>) -> ResponseBox {
    let mut builder = Response::builder()
        .status(status)
        .header("content-type", content_type);
    if status.is_client_error() || status.is_server_error() {
        builder = builder.header("connection", "close");
    }
    builder.body(Full::new(body.into()).boxed()).unwrap()
}

/// Runs the Northbound WebSocket and Web UI server until shutdown.
pub(crate) async fn run_ws_server(
    listener: TcpListener,
    token: Arc<String>,
    msg_tx: mpsc::Sender<DaemonMsg>,
    broadcast_tx: broadcast::Sender<ServerFrame>,
    mut shutdown_rx: watch::Receiver<bool>,
) -> Result<(), io::Error> {
    loop {
        tokio::select! {
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    break;
                }
            }
            res = listener.accept() => {
                let (stream, _) = match res {
                    Ok(pair) => pair,
                    Err(_) => break,
                };

                let io = TokioIo::new(stream);
                let expected_token = Arc::clone(&token);
                let server_msg_tx = msg_tx.clone();
                let server_broadcast_tx = broadcast_tx.clone();

                tokio::spawn(async move {
                    let service = service_fn(move |mut req: Request<Incoming>| {
                        let expected = Arc::clone(&expected_token);
                        let sub_msg_tx = server_msg_tx.clone();
                        let sub_broadcast_tx = server_broadcast_tx.clone();

                        async move {
                            // 1. Static asset: Web Companion UI
                            let path = req.uri().path();
                            if path == "/" || path == "/index.html" {
                                return Ok::<ResponseBox, Infallible>(full_response(
                                    StatusCode::OK,
                                    "text/html; charset=utf-8",
                                    WEB_COMPANION_HTML,
                                ));
                            }

                            // 2. WebSocket upgrade endpoint
                            if upgrade::is_upgrade_request(&req) {
                                let candidate_token = extract_token(&req);
                                let is_authorized = match candidate_token {
                                    Some(cand) => validate_token(&expected, &cand),
                                    None => false,
                                };

                                if !is_authorized {
                                    return Ok(full_response(
                                        StatusCode::UNAUTHORIZED,
                                        "application/json",
                                        r#"{"error":"Unauthorized: invalid or missing token"}"#,
                                    ));
                                }

                                match upgrade::upgrade(&mut req) {
                                    Ok((response, fut)) => {
                                        let client_id = NEXT_WS_CLIENT_ID.fetch_add(1, Ordering::Relaxed);
                                        tokio::spawn(async move {
                                            match fut.await {
                                                Ok(ws) => {
                                                    handle_ws_connection(ws, client_id, sub_msg_tx, sub_broadcast_tx).await;
                                                }
                                                Err(e) => {
                                                    eprintln!("websocket handshake error: {e}");
                                                }
                                            }
                                        });

                                        let boxed_resp = response.map(|b| {
                                            b.map_err(|never| match never {}).boxed()
                                        });
                                        return Ok(boxed_resp);
                                    }
                                    Err(e) => {
                                        return Ok(full_response(
                                            StatusCode::BAD_REQUEST,
                                            "text/plain",
                                            format!("WebSocket upgrade error: {e}"),
                                        ));
                                    }
                                }
                            }

                            // 3. Fallback: 404
                            Ok(full_response(StatusCode::NOT_FOUND, "text/plain", "Not Found"))
                        }
                    });

                    let conn = http1::Builder::new()
                        .serve_connection(io, service)
                        .with_upgrades();

                    let _ = conn.await;
                });
            }
        }
    }

    Ok(())
}

/// Drives an active WebSocket client connection.
async fn handle_ws_connection(
    ws: fastwebsockets::WebSocket<TokioIo<hyper::upgrade::Upgraded>>,
    client_id: usize,
    msg_tx: mpsc::Sender<DaemonMsg>,
    broadcast_tx: broadcast::Sender<ServerFrame>,
) {
    let (rx, mut tx) = ws.split(tokio::io::split);
    let mut rx = FragmentCollectorRead::new(rx);

    let (initial_tx, mut initial_rx) = mpsc::channel::<ServerFrame>(32);
    let (obligated_tx, mut obligated_rx) = mpsc::channel::<Frame<'static>>(16);
    let mut broadcast_rx = broadcast_tx.subscribe();

    // 1. Announce client to daemon for hydration
    let _ = msg_tx
        .send(DaemonMsg::NewClient {
            client_id,
            reply: initial_tx,
        })
        .await;

    // 2. Writer loop
    let writer_handle = tokio::spawn(async move {
        // Drain hydration snapshot frames first
        while let Some(frame) = initial_rx.recv().await {
            if let Ok(line) = codec::encode(&frame) {
                let out = Frame::text(Payload::Owned(line.into_bytes()));
                if tx.write_frame(out).await.is_err() {
                    return;
                }
            }
        }

        // Multiplex broadcast frames and obligated protocol frames (like Pong)
        loop {
            tokio::select! {
                Some(ob_frame) = obligated_rx.recv() => {
                    if tx.write_frame(ob_frame).await.is_err() {
                        break;
                    }
                }
                b_res = broadcast_rx.recv() => {
                    match b_res {
                        Ok(frame) => {
                            if let Ok(line) = codec::encode(&frame) {
                                let out = Frame::text(Payload::Owned(line.into_bytes()));
                                if tx.write_frame(out).await.is_err() {
                                    break;
                                }
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }
    });

    // 3. Reader loop
    let reader_msg_tx = msg_tx.clone();
    let reader_ob_tx = obligated_tx;

    loop {
        let frame_res = rx
            .read_frame::<_, WebSocketError>(&mut |frame| {
                let tx = reader_ob_tx.clone();
                async move {
                    let static_payload = frame.payload.to_vec();
                    let static_frame =
                        Frame::new(true, frame.opcode, None, Payload::Owned(static_payload));
                    let _ = tx.send(static_frame).await;
                    Ok(())
                }
            })
            .await;

        match frame_res {
            Ok(frame) => match frame.opcode {
                OpCode::Close => break,
                OpCode::Text => {
                    if let Ok(text) = std::str::from_utf8(&frame.payload) {
                        if let Ok(client_frame) = codec::decode::<ClientFrame>(text) {
                            let is_detach = matches!(client_frame, ClientFrame::Detach);
                            let _ = reader_msg_tx
                                .send(DaemonMsg::ClientFrame {
                                    client_id,
                                    frame: client_frame,
                                })
                                .await;
                            if is_detach {
                                break;
                            }
                        }
                    }
                }
                _ => {}
            },
            Err(_) => break,
        }
    }

    writer_handle.abort();
    let _ = msg_tx.send(DaemonMsg::ClientDisconnected(client_id)).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_generation_and_validation() {
        let token1 = generate_token();
        let token2 = generate_token();

        assert_eq!(token1.len(), 64);
        assert_eq!(token2.len(), 64);
        assert_ne!(token1, token2);

        assert!(validate_token(&token1, &token1));
        assert!(!validate_token(&token1, &token2));
        assert!(!validate_token(&token1, ""));
        assert!(!validate_token(&token1, "short"));
    }

    #[test]
    fn test_auth_token_file_save_and_path() {
        let session_id = format!("test-sess-{}", uuid::Uuid::now_v7());
        let token = generate_token();
        let path = save_auth_token(&session_id, &token).expect("save auth token");

        assert!(path.exists());
        let read_back = fs::read_to_string(&path).expect("read auth token");
        assert_eq!(read_back, token);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn test_web_companion_html_embedded() {
        assert!(WEB_COMPANION_HTML.contains("Kobold Web Companion"));
        assert!(WEB_COMPANION_HTML.contains("connectWS"));
        assert!(WEB_COMPANION_HTML.contains("tailwind"));
    }
}
