//! Streamable HTTP transport: each outbound JSON-RPC message is one POST to
//! the server's single MCP endpoint; the reply is either one JSON object or a
//! `text/event-stream` carrying it as SSE data.
//!
//! Uses `hyper_util`'s pooled legacy client rather than `ws.rs`'s hand-rolled
//! connect-per-call, because Streamable HTTP is many short-lived requests
//! against one origin -- exactly what connection pooling is for, and TCP+TLS
//! setup on every message would dominate latency. `ws.rs` cannot pool because
//! a WebSocket upgrade consumes the connection for the app's lifetime; this
//! transport never upgrades, so a plain HTTP/1.1 client is the right tool.
//!
//! Session ids (`Mcp-Session-Id`, legacy-revision only -- see `version.rs`)
//! are opaque to this module: the caller reads it off the first response and
//! feeds it back in, rather than this transport tracking protocol state it
//! has no business knowing about.

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::header::{ACCEPT, CONTENT_TYPE};
use hyper::{Request, Response, Uri};
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;

pub const MCP_SESSION_ID: &str = "Mcp-Session-Id";
pub const MCP_PROTOCOL_VERSION: &str = "MCP-Protocol-Version";

#[derive(Debug)]
pub enum Error {
    Encode(sonic_rs::Error),
    Build(hyper::http::Error),
    Send(hyper_util::client::legacy::Error),
    Body(hyper::Error),
    /// Neither `application/json` nor `text/event-stream`, or absent.
    UnexpectedContentType(String),
    /// A `text/event-stream` body with no `data:` line, so there is no
    /// message to hand back at all.
    EmptyEventStream,
    Status(hyper::StatusCode),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Encode(e) => write!(f, "encode: {e}"),
            Error::Build(e) => write!(f, "request build: {e}"),
            Error::Send(e) => write!(f, "http: {e}"),
            Error::Body(e) => write!(f, "body: {e}"),
            Error::UnexpectedContentType(ct) => write!(f, "unexpected content-type {ct:?}"),
            Error::EmptyEventStream => write!(f, "event stream carried no data"),
            Error::Status(s) => write!(f, "server returned {s}"),
        }
    }
}

impl std::error::Error for Error {}

pub struct Transport {
    client: Client<HttpsConnector<HttpConnector>, Full<Bytes>>,
    endpoint: Uri,
    /// Set once a response carries `Mcp-Session-Id`; echoed on every request
    /// after. Absent under the 2026-07-28 revision, which has no sessions.
    session_id: Option<String>,
    protocol_version: Option<&'static str>,
}

impl Transport {
    pub fn new(endpoint: Uri) -> Self {
        let https = default_https_connector();
        Self {
            client: Client::builder(TokioExecutor::new()).build(https),
            endpoint,
            session_id: None,
            protocol_version: None,
        }
    }

    /// Sets the `MCP-Protocol-Version` header sent on every subsequent
    /// request. Only meaningful under 2026-07-28 (see `version.rs`); the
    /// legacy revision negotiates once via `initialize` and never repeats it.
    pub fn set_protocol_version(&mut self, version: &'static str) {
        self.protocol_version = Some(version);
    }

    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// POSTs one serialized JSON-RPC message and returns the single reply
    /// message's raw bytes, unwrapped from SSE framing if the server chose
    /// that content type. Notifications (no reply expected) are the caller's
    /// business to not wait on -- this always tries to read a reply, matching
    /// the spec's per-POST response.
    pub async fn send<T: serde::Serialize>(&mut self, message: &T) -> Result<Vec<u8>, Error> {
        let body = crate::json::to_string(message).map_err(Error::Encode)?;

        let mut builder = Request::post(&self.endpoint)
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json, text/event-stream");
        if let Some(sid) = &self.session_id {
            builder = builder.header(MCP_SESSION_ID, sid.as_str());
        }
        if let Some(v) = self.protocol_version {
            builder = builder.header(MCP_PROTOCOL_VERSION, v);
        }

        let req = builder
            .body(Full::new(Bytes::from(body)))
            .map_err(Error::Build)?;
        let res = self.client.request(req).await.map_err(Error::Send)?;

        if let Some(sid) = res.headers().get(MCP_SESSION_ID) {
            if let Ok(s) = sid.to_str() {
                self.session_id = Some(s.to_owned());
            }
        }

        if !res.status().is_success() {
            return Err(Error::Status(res.status()));
        }

        read_body(res).await
    }
}

async fn read_body(res: Response<Incoming>) -> Result<Vec<u8>, Error> {
    let content_type = res
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let bytes = res
        .into_body()
        .collect()
        .await
        .map_err(Error::Body)?
        .to_bytes();
    decode_by_content_type(&content_type, &bytes)
}

/// The content-type branching from `read_body`, pulled out as a pure
/// function so it is testable without a real HTTP connection -- hyper's
/// `Incoming` body has no public constructor outside one.
fn decode_by_content_type(content_type: &str, bytes: &[u8]) -> Result<Vec<u8>, Error> {
    if content_type.starts_with("application/json") {
        Ok(bytes.to_vec())
    } else if content_type.starts_with("text/event-stream") {
        first_sse_data(bytes).ok_or(Error::EmptyEventStream)
    } else {
        Err(Error::UnexpectedContentType(content_type.to_owned()))
    }
}

/// Pulls the `data` field out of the first complete SSE event. Minimal on
/// purpose: this client sends one request and wants one reply, never a
/// standing subscription, so multi-event streams, `id:`/`retry:` fields and
/// reconnection are out of scope -- the spec's resumability machinery
/// (`Last-Event-ID`) was removed outright in 2026-07-28 and was only ever
/// needed for the long-lived GET listen stream, not a POST reply.
fn first_sse_data(bytes: &[u8]) -> Option<Vec<u8>> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut data_lines = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
            data_lines.push(rest.strip_prefix(' ').unwrap_or(rest));
        } else if line.is_empty() && !data_lines.is_empty() {
            // Blank line ends the event.
            break;
        }
    }
    if data_lines.is_empty() {
        None
    } else {
        Some(data_lines.join("\n").into_bytes())
    }
}

fn default_https_connector() -> HttpsConnector<HttpConnector> {
    hyper_rustls::HttpsConnectorBuilder::new()
        .with_webpki_roots()
        .https_or_http()
        .enable_http1()
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_json_content_type_response_body_is_returned_verbatim() {
        let got = decode_by_content_type("application/json", br#"{"ok":true}"#).unwrap();
        assert_eq!(got, br#"{"ok":true}"#);
    }

    #[test]
    fn a_json_content_type_with_a_charset_suffix_is_still_recognised() {
        let got = decode_by_content_type("application/json; charset=utf-8", b"{}").unwrap();
        assert_eq!(got, b"{}");
    }

    #[test]
    fn the_first_sse_data_field_is_extracted_from_a_single_event_stream() {
        let raw = b"event: message\ndata: {\"ok\":true}\n\n";
        let got = first_sse_data(raw).unwrap();
        assert_eq!(got, br#"{"ok":true}"#);
    }

    #[test]
    fn a_multi_line_data_payload_is_joined_with_newlines_per_the_sse_spec() {
        let raw = b"data: line one\ndata: line two\n\n";
        let got = first_sse_data(raw).unwrap();
        assert_eq!(got, b"line one\nline two");
    }

    #[test]
    fn only_the_first_event_is_returned_when_the_stream_carries_more_than_one() {
        let raw = b"data: first\n\ndata: second\n\n";
        let got = first_sse_data(raw).unwrap();
        assert_eq!(got, b"first");
    }

    #[test]
    fn a_stream_with_no_data_field_at_all_yields_nothing() {
        let raw = b": just a comment\n\n";
        assert_eq!(first_sse_data(raw), None);
    }

    #[test]
    fn a_non_json_non_event_stream_content_type_is_reported_as_unexpected() {
        let err = decode_by_content_type("text/plain", b"nope").unwrap_err();
        assert!(matches!(err, Error::UnexpectedContentType(ct) if ct == "text/plain"));
    }

    #[test]
    fn a_missing_content_type_is_also_reported_as_unexpected_rather_than_guessed() {
        let err = decode_by_content_type("", b"{}").unwrap_err();
        assert!(matches!(err, Error::UnexpectedContentType(ct) if ct.is_empty()));
    }

    #[test]
    fn an_event_stream_with_no_data_at_all_is_reported_as_empty_not_as_ok_with_nothing() {
        let err = decode_by_content_type("text/event-stream", b": comment only\n\n").unwrap_err();
        assert!(matches!(err, Error::EmptyEventStream));
    }

    #[test]
    fn a_leading_blank_line_before_any_data_does_not_end_the_event_early() {
        // Guards the `&&` in the blank-line check: with `||` this would stop
        // at the very first (pre-data) blank line and miss the data below it.
        let got = first_sse_data(b"\ndata: hello\n\n").unwrap();
        assert_eq!(got, b"hello");
    }

    #[test]
    fn a_send_or_status_error_displays_a_message_naming_the_failure_kind() {
        assert!(Error::EmptyEventStream.to_string().contains("event stream"));
        assert!(Error::UnexpectedContentType("x/y".into())
            .to_string()
            .contains("x/y"));
    }

    /// A minimal loopback HTTP/1.1 server: reads one request, replies with a
    /// fixed body and content-type. Enough to exercise `Transport::send`
    /// end-to-end (headers set, body posted, session id captured, reply
    /// decoded) without reaching the real network -- a bound `127.0.0.1:0`
    /// listener, per the house rule that transport tests use a local
    /// listener rather than a live MCP server.
    ///
    /// Every reply sets `Connection: close` deliberately. Without it, the
    /// first version of this test hung forever: `Transport`'s client pools
    /// connections (keep-alive), so it never closes its end after reading
    /// the response, and `serve_connection` waits for the client to close
    /// before returning -- a pooled client plus an await-until-closed server
    /// is a deadlock, not a slow test. If you write another in-process
    /// client/server test pair against this client, keep this header.
    async fn serve_one(
        listener: tokio::net::TcpListener,
        status: hyper::StatusCode,
        content_type: &'static str,
        body: &'static str,
        session_id: Option<&'static str>,
    ) {
        let (stream, _) = listener.accept().await.unwrap();
        let io = hyper_util::rt::TokioIo::new(stream);
        let service = hyper::service::service_fn(move |_req: Request<Incoming>| {
            let mut res = Response::builder()
                .status(status)
                .header(CONTENT_TYPE, content_type)
                .header(hyper::header::CONNECTION, "close");
            if let Some(sid) = session_id {
                res = res.header(MCP_SESSION_ID, sid);
            }
            let res = res.body(Full::new(Bytes::from(body))).unwrap();
            async move { Ok::<_, std::convert::Infallible>(res) }
        });
        let _ = hyper::server::conn::http1::Builder::new()
            .serve_connection(io, service)
            .await;
    }

    #[tokio::test]
    async fn a_json_reply_from_a_real_connection_round_trips_through_send() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(serve_one(
            listener,
            hyper::StatusCode::OK,
            "application/json",
            r#"{"jsonrpc":"2.0","id":1,"result":{}}"#,
            Some("sess-abc"),
        ));

        let mut t = Transport::new(format!("http://{addr}/mcp").parse().unwrap());
        let req = crate::mcp::jsonrpc::Request::<()>::new(1.into(), "ping", None);
        // Bounded rather than a bare `.await`: a `send` that stubbed out and
        // never touched the network would otherwise hang here instead of
        // failing, since the loopback server would then have no request to
        // answer.
        let reply = with_timeout(t.send(&req)).await.unwrap();

        assert_eq!(reply, br#"{"jsonrpc":"2.0","id":1,"result":{}}"#);
        assert_eq!(t.session_id(), Some("sess-abc"));
        with_timeout(server).await.unwrap();
    }

    #[tokio::test]
    async fn a_non_success_status_is_reported_as_an_error_not_as_the_body() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(serve_one(
            listener,
            hyper::StatusCode::INTERNAL_SERVER_ERROR,
            "application/json",
            r#"{"oops":true}"#,
            None,
        ));

        let mut t = Transport::new(format!("http://{addr}/mcp").parse().unwrap());
        let req = crate::mcp::jsonrpc::Request::<()>::new(1.into(), "ping", None);
        let err = with_timeout(t.send(&req)).await.unwrap_err();

        assert!(matches!(err, Error::Status(s) if s == hyper::StatusCode::INTERNAL_SERVER_ERROR));
        with_timeout(server).await.unwrap();
    }

    #[tokio::test]
    async fn set_protocol_version_puts_the_header_on_the_next_request() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (seen_tx, seen_rx) = tokio::sync::oneshot::channel();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let io = hyper_util::rt::TokioIo::new(stream);
            let seen_tx = std::sync::Mutex::new(Some(seen_tx));
            let service = hyper::service::service_fn(move |req: Request<Incoming>| {
                if let Some(tx) = seen_tx.lock().unwrap().take() {
                    let header = req
                        .headers()
                        .get(MCP_PROTOCOL_VERSION)
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_owned);
                    let _ = tx.send(header);
                }
                let res = Response::builder()
                    .header(CONTENT_TYPE, "application/json")
                    .header(hyper::header::CONNECTION, "close")
                    .body(Full::new(Bytes::from(
                        r#"{"jsonrpc":"2.0","id":1,"result":{}}"#,
                    )))
                    .unwrap();
                async move { Ok::<_, std::convert::Infallible>(res) }
            });
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(io, service)
                .await;
        });

        let mut t = Transport::new(format!("http://{addr}/mcp").parse().unwrap());
        t.set_protocol_version(crate::mcp::version::V2026_07_28);
        let req = crate::mcp::jsonrpc::Request::<()>::new(1.into(), "ping", None);
        with_timeout(t.send(&req)).await.unwrap();

        let seen = with_timeout(seen_rx).await.unwrap();
        assert_eq!(seen.as_deref(), Some(crate::mcp::version::V2026_07_28));
        with_timeout(server).await.unwrap();
    }

    /// Bounds a future to well under `cargo-mutants`' default 20s-per-mutant
    /// budget, so a mutant that turns a network call into a no-op stub fails
    /// fast and deterministically instead of running out the clock as a
    /// timeout -- the report categorizes those separately from "caught", and
    /// a stub that returns success unnoticed is exactly the bug this file
    /// exists to prevent.
    async fn with_timeout<F: std::future::Future>(f: F) -> F::Output {
        tokio::time::timeout(std::time::Duration::from_secs(2), f)
            .await
            .expect("operation should complete well within the mutant test budget")
    }
}
