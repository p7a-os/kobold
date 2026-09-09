use std::path::Path;
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use bytes::Bytes;
use fastwebsockets::{FragmentCollector, Frame, OpCode, Payload, WebSocketError};
use http_body_util::Empty;
use hyper::header::{AUTHORIZATION, CONNECTION, HOST, UPGRADE};
use hyper::Request;
use hyper_util::rt::{TokioExecutor, TokioIo};
use kobold_types::BackendError;
use rustls::{ClientConfig, RootCertStore};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

pub const HOSTNAME: &str = "api.openai.com";
pub const PATH: &str = "/v1/responses";

/// Abstract contract for WebSocket transports, enabling mock injection for deterministic testing.
#[async_trait]
pub trait WebSocketTransport: Send {
    async fn send(&mut self, text: &str) -> Result<(), BackendError>;
    async fn next_message(&mut self) -> Result<Option<String>, BackendError>;
    async fn close(&mut self) -> Result<(), BackendError>;
}

/// Cached TLS client configuration with parsed webpki roots to eliminate re-handshake latency.
fn tls_config() -> Arc<ClientConfig> {
    static CONFIG: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let roots = RootCertStore {
                roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
            };
            let mut config = ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth();
            config.alpn_protocols = vec![b"http/1.1".to_vec()];
            Arc::new(config)
        })
        .clone()
}

pub type LiveStream = FragmentCollector<TokioIo<hyper::upgrade::Upgraded>>;

/// Live WebSocket connection to OpenAI Responses API.
pub struct LiveOpenAiTransport {
    stream: LiveStream,
}

impl LiveOpenAiTransport {
    pub async fn connect(
        api_key: &str,
        egress_socket: Option<&Path>,
    ) -> Result<Self, BackendError> {
        let tcp: Box<dyn AsyncIoStream> = match egress_socket {
            Some(socket) => {
                let stream = tokio::net::UnixStream::connect(socket)
                    .await
                    .map_err(|e| BackendError::Connection(format!("Egress broker socket error: {e}")))?;
                Box::new(stream)
            }
            None => {
                let addrs = tokio::net::lookup_host((HOSTNAME, 443))
                    .await
                    .map_err(|e| BackendError::Connection(format!("DNS lookup for {HOSTNAME}:443 failed: {e}")))?;
                let mut last_err = None;
                let mut connected_tcp = None;
                for addr in addrs {
                    match TcpStream::connect(addr).await {
                        Ok(s) => {
                            let _ = s.set_nodelay(true);
                            connected_tcp = Some(s);
                            break;
                        }
                        Err(e) => last_err = Some(e),
                    }
                }
                let tcp = match connected_tcp {
                    Some(s) => s,
                    None => {
                        return Err(BackendError::Connection(format!(
                            "Failed connecting to {HOSTNAME}:443: {:?}",
                            last_err
                        )));
                    }
                };
                Box::new(tcp)
            }
        };

        let server_name = HOSTNAME
            .try_into()
            .map_err(|_| BackendError::Connection("Invalid static hostname".into()))?;

        let tls = TlsConnector::from(tls_config())
            .connect(server_name, tcp)
            .await
            .map_err(|e| BackendError::Connection(format!("TLS handshake failed: {e}")))?;

        let req = Request::builder()
            .method("GET")
            .uri(format!("https://{HOSTNAME}{PATH}"))
            .header(HOST, HOSTNAME)
            .header(UPGRADE, "websocket")
            .header(CONNECTION, "upgrade")
            .header("Sec-WebSocket-Key", fastwebsockets::handshake::generate_key())
            .header("Sec-WebSocket-Version", "13")
            .header(AUTHORIZATION, format!("Bearer {api_key}"))
            .body(Empty::<Bytes>::new())
            .map_err(|e| BackendError::Protocol(format!("HTTP request build error: {e}")))?;

        let (ws, _res) = fastwebsockets::handshake::client(&TokioExecutor::new(), req, tls)
            .await
            .map_err(|e| BackendError::Connection(format!("WebSocket upgrade handshake failed: {e}")))?;

        Ok(Self {
            stream: FragmentCollector::new(ws),
        })
    }
}

pub trait AsyncIoStream:
    tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static
{
}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static> AsyncIoStream for T {}

#[async_trait]
impl WebSocketTransport for LiveOpenAiTransport {
    async fn send(&mut self, text: &str) -> Result<(), BackendError> {
        self.stream
            .write_frame(Frame::text(Payload::Borrowed(text.as_bytes())))
            .await
            .map_err(|e| BackendError::Protocol(format!("WebSocket write error: {e}")))?;
        Ok(())
    }

    async fn next_message(&mut self) -> Result<Option<String>, BackendError> {
        match self.stream.read_frame().await {
            Ok(frame) => match frame.opcode {
                OpCode::Text => {
                    let s = String::from_utf8(frame.payload.to_vec())
                        .map_err(|e| BackendError::Protocol(format!("Invalid UTF-8 frame: {e}")))?;
                    Ok(Some(s))
                }
                OpCode::Close => Ok(None),
                _ => Ok(Some(String::new())),
            },
            Err(WebSocketError::ConnectionClosed) => Ok(None),
            Err(e) => Err(BackendError::Connection(format!("WebSocket read error: {e}"))),
        }
    }

    async fn close(&mut self) -> Result<(), BackendError> {
        let _ = self.stream.write_frame(Frame::close(1000, &[])).await;
        Ok(())
    }
}

/// In-memory mock transport for unit and integration testing without external networks.
pub struct MockWebSocketTransport {
    pub sent_messages: Arc<tokio::sync::Mutex<Vec<String>>>,
    pub incoming_messages: Arc<tokio::sync::Mutex<Vec<String>>>,
}

impl MockWebSocketTransport {
    pub fn new(incoming: Vec<String>) -> Self {
        Self {
            sent_messages: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            incoming_messages: Arc::new(tokio::sync::Mutex::new(incoming)),
        }
    }
}

#[async_trait]
impl WebSocketTransport for MockWebSocketTransport {
    async fn send(&mut self, text: &str) -> Result<(), BackendError> {
        self.sent_messages.lock().await.push(text.to_string());
        Ok(())
    }

    async fn next_message(&mut self) -> Result<Option<String>, BackendError> {
        let mut guard = self.incoming_messages.lock().await;
        if guard.is_empty() {
            Ok(None)
        } else {
            Ok(Some(guard.remove(0)))
        }
    }

    async fn close(&mut self) -> Result<(), BackendError> {
        Ok(())
    }
}
