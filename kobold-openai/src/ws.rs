//! WebSocket transport to `wss://api.openai.com/v1/responses`.

use std::borrow::Cow;
use std::sync::{Arc, OnceLock};

use bytes::Bytes;
use fastwebsockets::{FragmentCollector, Frame, OpCode, Payload, WebSocketError};
use http_body_util::Empty;
use hyper::header::{AUTHORIZATION, CONNECTION, HOST, UPGRADE};
use hyper::Request;
use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio::net::TcpStream;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};
use tokio_rustls::TlsConnector;

pub const HOSTNAME: &str = "api.openai.com";
pub const PATH: &str = "/v1/responses";

/// Connections are capped at 60 minutes server-side. Rotate before that during
/// an idle gap; being cut off mid-loop with `store: false` means replaying the
/// whole context.
pub const ROTATE_AFTER: std::time::Duration = std::time::Duration::from_secs(55 * 60);

pub type Stream = FragmentCollector<TokioIo<hyper::upgrade::Upgraded>>;

#[derive(Debug)]
pub enum ConnectError {
    Io(std::io::Error),
    Tls(std::io::Error),
    Http(hyper::http::Error),
    Ws(WebSocketError),
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectError::Io(e) => write!(f, "tcp: {e}"),
            ConnectError::Tls(e) => write!(f, "tls: {e}"),
            ConnectError::Http(e) => write!(f, "http: {e}"),
            ConnectError::Ws(e) => write!(f, "websocket: {e}"),
        }
    }
}

impl std::error::Error for ConnectError {}

/// Built once. Parsing ~150 webpki roots into a `RootCertStore` is milliseconds
/// of pure CPU, and reconnects (60-minute cap, transient drops) would otherwise
/// pay it every time.
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
            // We only ever speak HTTP/1.1 here (WebSocket is an h1 upgrade).
            // Advertising it avoids any chance of an h2 negotiation we would
            // then have to unwind.
            config.alpn_protocols = vec![b"http/1.1".to_vec()];
            Arc::new(config)
        })
        .clone()
}

/// TLS handshake only. Exposed so the probe can time it in isolation.
pub async fn tls_handshake(
    tcp: TcpStream,
) -> Result<tokio_rustls::client::TlsStream<TcpStream>, ConnectError> {
    let server_name = HOSTNAME
        .try_into()
        .expect("static hostname is a valid DNS name");
    TlsConnector::from(tls_config())
        .connect(server_name, tcp)
        .await
        .map_err(ConnectError::Tls)
}

/// Open one multiplexed connection. `api_key` comes from `LLM_API_KEY`.
pub async fn connect(api_key: &str) -> Result<Stream, ConnectError> {
    Ok(connect_inner(api_key, None, None).await?.0)
}

/// Connect through Kobold's egress broker at `socket`.
///
/// The adapter has no network of its own -- `TcpStream::connect` inside the
/// sandbox fails with "Network is unreachable" and DNS cannot resolve
/// anything -- so this is the only way out, and it goes through a host Kobold
/// has already agreed to.
///
/// **TLS is still established here, over the spliced socket.** Kobold carries
/// bytes it cannot read and never sees the credential a second time.
pub async fn connect_via(api_key: &str, socket: &str) -> Result<Stream, ConnectError> {
    Ok(connect_inner(api_key, None, Some(socket)).await?.0)
}

/// Same handshake, but returns the server's response so a caller can inspect
/// negotiated headers. `extensions` fills `Sec-WebSocket-Extensions`.
pub async fn connect_inspect(
    api_key: &str,
    extensions: Option<&str>,
) -> Result<(Stream, hyper::Response<hyper::body::Incoming>), ConnectError> {
    connect_inner(api_key, extensions, None).await
}

/// Whatever the TLS session is running over.
///
/// Boxed because there are two of them and they are different types: a
/// `TcpStream` when Kobold allows direct egress, and a `UnixStream` to the
/// broker when it does not. One allocation, once per connection.
type Wire = Box<dyn Transport2>;

/// The bound `Wire` needs. A local trait purely so it can be named in one
/// place; blanket-implemented, so nothing has to opt in.
pub trait Transport2:
    tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static
{
}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static> Transport2 for T {}

/// Ask the broker for `HOSTNAME:443` and hand back the socket once it agrees.
///
/// One line out, one line back, then the socket is raw. A refusal is an error
/// carrying the broker's reason, because "Kobold's allowlist does not include
/// this host" is a configuration problem the user can act on and must not be
/// reported as a network failure that a reconnect would retry forever.
async fn via_broker(socket: &str) -> Result<Wire, ConnectError> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let stream = tokio::net::UnixStream::connect(socket)
        .await
        .map_err(ConnectError::Io)?;
    let mut stream = BufReader::new(stream);
    stream
        .get_mut()
        .write_all(format!("CONNECT {HOSTNAME}:443\n").as_bytes())
        .await
        .map_err(ConnectError::Io)?;
    // `read_line` grows until it finds a newline, which is the unbounded
    // shape the broker itself guards against with `MAX_REQUEST` -- and it is
    // right here and wrong there, because the trust runs one way. Kobold owns
    // the broker and spawned this process; the untrusted party on this socket
    // is the adapter, not the broker. Said explicitly so the next reader does
    // not spend an afternoon deciding whether it is a hole.
    let mut answer = String::new();
    stream
        .read_line(&mut answer)
        .await
        .map_err(ConnectError::Io)?;
    if answer.trim_end() != "OK" {
        return Err(ConnectError::Io(std::io::Error::other(format!(
            "egress broker refused: {}",
            answer.trim_end()
        ))));
    }
    // `into_inner` is safe to the byte here: the broker sends exactly the
    // answer line and then stays quiet until we speak, so the reader's buffer
    // is empty. Anything it had read ahead would be discarded, which would be
    // a torn ServerHello -- worth stating, because the symptom would surface
    // as a TLS error a long way from this line.
    Ok(Box::new(stream.into_inner()))
}

async fn connect_inner(
    api_key: &str,
    extensions: Option<&str>,
    broker: Option<&str>,
) -> Result<(Stream, hyper::Response<hyper::body::Incoming>), ConnectError> {
    let tcp: Wire = match broker {
        Some(socket) => via_broker(socket).await?,
        None => {
            let tcp = TcpStream::connect((HOSTNAME, 443))
                .await
                .map_err(ConnectError::Io)?;

            // Measured A/B on this endpoint: no detectable effect. Nagle only delays
            // when there is unacknowledged data in flight, and a turn sends one small
            // frame then waits ~1s for the model, so nothing is ever pending. Kept
            // anyway as cheap insurance for the case that does bite -- a frame written
            // as separate header and payload writes, and tool outputs sent in several
            // frames back to back -- where the cost is bounded by the 1.3ms RTT to the
            // edge, not the textbook 40ms. KOBOLD_NAGLE=1 re-runs the A/B.
            let nodelay = std::env::var_os("KOBOLD_NAGLE").is_none();
            tcp.set_nodelay(nodelay).map_err(ConnectError::Io)?;
            Box::new(tcp)
        }
    };

    let server_name = HOSTNAME
        .try_into()
        .expect("static hostname is a valid DNS name");
    let tls = TlsConnector::from(tls_config())
        .connect(server_name, tcp)
        .await
        .map_err(ConnectError::Tls)?;

    let mut builder = Request::builder()
        .method("GET")
        .uri(format!("https://{HOSTNAME}{PATH}"))
        .header(HOST, HOSTNAME)
        .header(UPGRADE, "websocket")
        .header(CONNECTION, "upgrade")
        .header(
            "Sec-WebSocket-Key",
            fastwebsockets::handshake::generate_key(),
        )
        .header("Sec-WebSocket-Version", "13")
        .header(AUTHORIZATION, format!("Bearer {api_key}"));

    if let Some(ext) = extensions {
        builder = builder.header("Sec-WebSocket-Extensions", ext);
    }

    let req = builder
        .body(Empty::<Bytes>::new())
        .map_err(ConnectError::Http)?;

    let (ws, res) = fastwebsockets::handshake::client(&TokioExecutor::new(), req, tls)
        .await
        .map_err(ConnectError::Ws)?;

    // Events are JSON documents, so we need whole messages, not raw frames.
    Ok((FragmentCollector::new(ws), res))
}

pub async fn send_text(ws: &mut Stream, text: &str) -> Result<(), WebSocketError> {
    ws.write_frame(Frame::text(Payload::Borrowed(text.as_bytes())))
        .await
}

/// What `net::run` needs of a socket, and nothing more.
///
/// Two methods, because that is all the loop uses. The point is that the
/// provider loop can be driven by something other than a live WebSocket to
/// api.openai.com -- there is no other way to test what it does with a
/// close that carries no terminal event, or a server that simply stops.
///
/// A trait rather than a local listener deliberately. `connect` speaks TLS to
/// a fixed host, so a listener would need either certificate machinery in the
/// test or a plaintext branch in production connect code -- a second path to
/// the same thing, which is the shape this project has removed twice.
///
/// Generic rather than `dyn`, because `read` hands back a borrow of the read
/// buffer and that zero-copy property is why the event types can borrow at
/// all. Static dispatch keeps it and costs nothing.
pub trait Transport {
    type Error: std::fmt::Display;

    fn send_text(
        &mut self,
        text: &str,
    ) -> impl std::future::Future<Output = Result<(), Self::Error>> + Send;

    fn read(
        &mut self,
    ) -> impl std::future::Future<Output = Result<Incoming<'_>, Self::Error>> + Send;
}

impl Transport for Stream {
    type Error = WebSocketError;

    async fn send_text(&mut self, text: &str) -> Result<(), WebSocketError> {
        send_text(self, text).await
    }

    async fn read(&mut self) -> Result<Incoming<'_>, WebSocketError> {
        read(self).await
    }
}

/// One inbound message, borrowed straight out of the read buffer.
///
/// Borrowed rather than `Vec<u8>` on purpose: an agentic run takes thousands of
/// delta events, and copying each one only to parse it into borrowed `&str`
/// fields would undo the point of the zero-copy event types. The borrow ends
/// when the caller finishes with the event, so anything that outlives the turn
/// (a tool call's arguments) must copy explicitly.
pub enum Incoming<'a> {
    /// Borrowed for the common case; owned only when the collector had to
    /// stitch fragments, in which case the buffer is moved, never copied.
    Text(Cow<'a, [u8]>),
    Closed,
    Other,
}

pub async fn read(ws: &mut Stream) -> Result<Incoming<'_>, WebSocketError> {
    Ok(classify(ws.read_frame().await?))
}

/// One frame to one `Incoming`.
///
/// Split from the read so it can be tested without a socket. It is small but
/// it is not trivial: a `Close` mistaken for `Other` hangs the read loop
/// forever waiting for a terminal event that already went past, and a `Text`
/// mistaken for `Other` drops a delta silently. Both are the failures this
/// codebase keeps finding, and neither shows up as an error.
fn classify(frame: Frame<'_>) -> Incoming<'_> {
    match frame.opcode {
        OpCode::Text | OpCode::Binary => Incoming::Text(match frame.payload {
            Payload::Borrowed(b) => Cow::Borrowed(b),
            Payload::Owned(v) => Cow::Owned(v),
            other => Cow::Owned(other.to_vec()),
        }),
        OpCode::Close => Incoming::Closed,
        _ => Incoming::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(incoming: &Incoming<'_>) -> Option<String> {
        match incoming {
            Incoming::Text(b) => Some(String::from_utf8_lossy(b).into_owned()),
            _ => None,
        }
    }

    #[test]
    fn a_text_frame_becomes_text_and_keeps_its_bytes() {
        let got = classify(Frame::text(Payload::Borrowed(b"{\"type\":\"x\"}")));
        assert_eq!(text_of(&got).as_deref(), Some("{\"type\":\"x\"}"));
    }

    #[test]
    fn a_binary_frame_is_read_as_text_too() {
        // The API sends JSON, but a server that framed it as binary would
        // otherwise have every message silently discarded.
        let got = classify(Frame::binary(Payload::Borrowed(b"{}")));
        assert_eq!(text_of(&got).as_deref(), Some("{}"));
    }

    #[test]
    fn an_owned_payload_is_moved_rather_than_lost() {
        // The fragment collector hands back owned bytes when it had to
        // stitch a message together. Dropping that case would lose exactly
        // the large messages -- a long tool-call argument list.
        let got = classify(Frame::text(Payload::Owned(b"stitched".to_vec())));
        assert_eq!(text_of(&got).as_deref(), Some("stitched"));
    }

    #[test]
    fn a_close_frame_is_closed_and_not_merely_other() {
        // The distinction the read loop turns on. `Other` is ignored and the
        // loop reads again, so a close read as other waits forever for a
        // terminal event that has already gone past.
        assert!(matches!(
            classify(Frame::close(1000, b"bye")),
            Incoming::Closed
        ));
        assert!(matches!(
            classify(Frame::close_raw(Payload::Borrowed(&[]))),
            Incoming::Closed
        ));
    }

    #[test]
    fn control_frames_are_other_and_carry_nothing() {
        // Ping and pong are handled beneath us and mean nothing here. Read
        // as text they would be parsed as events and fail; read as close
        // they would end the turn.
        for frame in [
            Frame::new(true, OpCode::Ping, None, Payload::Borrowed(b"")),
            Frame::new(true, OpCode::Pong, None, Payload::Borrowed(b"")),
        ] {
            assert!(matches!(classify(frame), Incoming::Other));
        }
    }

    /// The other half of the allowlist coupling.
    ///
    /// `kobold` cannot import this constant -- an adapter is a separate
    /// process and the crates deliberately do not depend on each other -- so
    /// the two ends are pinned separately: this says what the adapter dials,
    /// and `main.rs`'s allowlist test says the default permits that string.
    /// Change one without the other and a shipped Kobold cannot reach any
    /// provider, which is a green suite and a dead product.
    #[test]
    fn the_hostname_this_adapter_dials_is_the_one_kobolds_default_allowlist_names() {
        assert_eq!(HOSTNAME, "api.openai.com");
    }

    /// **The broker's verdict, read the right way round.**
    ///
    /// Inverted, `OK` becomes a refusal and `DENY` becomes success -- so the
    /// adapter would establish TLS over a socket the broker had just declined
    /// and reject the connections it allowed. Both spellings, against a fake
    /// broker that answers a fixed line, because a test of one direction is
    /// satisfied by a comparison stuck on that answer.
    #[tokio::test]
    async fn the_brokers_answer_decides_and_a_refusal_carries_its_reason() {
        async fn broker_saying(answer: &'static str) -> (tempdir::Dir, String) {
            let dir = tempdir::Dir::new();
            let path = dir
                .path()
                .join("egress.sock")
                .to_string_lossy()
                .into_owned();
            let listener = tokio::net::UnixListener::bind(&path).expect("bind");
            tokio::spawn(async move {
                while let Ok((mut sock, _)) = listener.accept().await {
                    tokio::spawn(async move {
                        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
                        // Read the request first: answering before the client
                        // has spoken would test a protocol nobody implements.
                        let mut r = BufReader::new(&mut sock);
                        let mut asked = String::new();
                        let _ = r.read_line(&mut asked).await;
                        let _ = sock.write_all(answer.as_bytes()).await;
                        // Held open, so a refusal is distinguished from the
                        // socket simply closing.
                        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                    });
                }
            });
            (dir, path)
        }

        let (_d, path) = broker_saying("OK\n").await;
        assert!(
            via_broker(&path).await.is_ok(),
            "the broker allowed it and the adapter refused"
        );

        let (_d, path) = broker_saying("DENY api.openai.com:443 is not allowed\n").await;
        let err = via_broker(&path)
            .await
            .err()
            .expect("a refusal must not read as success");
        let shown = err.to_string();
        // The reason travels, because "your allowlist does not include this
        // host" is a configuration problem the user can fix and must not be
        // reported as a network failure a reconnect would retry forever.
        assert!(
            shown.contains("is not allowed"),
            "the broker's reason was dropped: {shown}"
        );
    }

    /// A directory that removes itself, so these tests leave nothing in
    /// `/tmp`. Local to this module rather than shared: `kobold::broker` has
    /// its own and this crate cannot see it.
    mod tempdir {
        use std::path::{Path, PathBuf};
        pub struct Dir(PathBuf);
        impl Dir {
            pub fn new() -> Self {
                use std::sync::atomic::{AtomicU32, Ordering};
                static NTH: AtomicU32 = AtomicU32::new(0);
                let d = std::env::temp_dir().join(format!(
                    "kobold-ws-test-{}-{}",
                    std::process::id(),
                    NTH.fetch_add(1, Ordering::Relaxed)
                ));
                std::fs::create_dir_all(&d).expect("temp dir");
                Dir(d)
            }
            pub fn path(&self) -> &Path {
                &self.0
            }
        }
        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }
}
