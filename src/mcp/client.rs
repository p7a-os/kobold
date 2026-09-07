//! Wires a connected MCP server into `tools::McpTools`, so `tools::run` can
//! reach it without knowing a socket or a subprocess exists on the other end.
//!
//! `tools/list` + `tools/call` over stdio, one server per `Client`, with era
//! detection (`server/discover`, falling back to the legacy `initialize`
//! handshake) and the `-32022` version-retry loop. HTTP and elicitation are
//! not wired here yet.
//!
//! Known gap, stated rather than hidden: 2026-07-28 requires every request to
//! declare its version via `_meta`, and this client does not yet stamp that
//! on `tools/list`/`tools/call`. The retry loop (`version::retry_version`) is
//! implemented and tested against a synthetic `-32022` response, but will not
//! be exercised against a strict modern server that actually enforces
//! per-request version declaration until `_meta` stamping lands.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};

use crate::mcp::jsonrpc::{Id, Incoming, Notification, Request};
use crate::mcp::version::{Revision, V2025_11_25};
use crate::mcp::{era, stdio};
use crate::tools::{Call, Outcome};

#[derive(Debug)]
pub enum Error {
    Transport(stdio::Error),
    Decode(sonic_rs::Error),
    /// The reply parsed as JSON but not as a valid protocol result -- a
    /// missing or wrongly-typed field, not a syntax error, so it is distinct
    /// from `Decode`.
    Protocol(String),
    /// The server answered, but not to the request we asked -- notably not
    /// yet handled here, since MRTR (2026-07-28) and server-initiated
    /// requests (2025-11-25) both produce messages that are not our reply.
    UnexpectedMessage,
    /// A server name containing the namespace separator, which could forge
    /// another server's tool namespace. See `Client::spawn`.
    NameContainsSeparator(String),
    Rpc(crate::mcp::jsonrpc::RpcError),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Transport(e) => write!(f, "transport: {e}"),
            Error::Decode(e) => write!(f, "decode: {e}"),
            Error::Protocol(msg) => write!(f, "protocol: {msg}"),
            Error::UnexpectedMessage => write!(f, "server sent a message that was not our reply"),
            Error::NameContainsSeparator(n) => {
                write!(f, "server name '{n}' contains '__', which could forge another server's tool namespace")
            }
            Error::Rpc(e) => write!(f, "server error: {e}"),
        }
    }
}

impl std::error::Error for Error {}

/// Prefix under which every tool this server offers is namespaced, so a
/// server cannot shadow a local tool or another server's tool -- see
/// `tools.rs`'s `run`, which checks local names before ever asking whether an
/// MCP source owns a name.
const SEP: &str = "__";

struct ToolInfo {
    /// The name as the server itself knows it, unnamespaced -- what goes into
    /// the `tools/call` request.
    remote_name: String,
    description: String,
    input_schema: String,
}

pub struct Client {
    transport: tokio::sync::Mutex<stdio::Transport>,
    next_id: AtomicI64,
    /// Namespaced name (`server__tool`) -> what the server calls it.
    tools: HashMap<String, ToolInfo>,
    prefix: String,
    /// Set once, by `detect_era`, and read-only after: era is a property of
    /// the server for the connection's lifetime, not of a single request.
    era: Revision,
    /// The version we believe is negotiated. Mutable after construction only
    /// because the `-32022` retry loop in `request` updates it when a server
    /// rejects our declared version mid-session.
    protocol_version: std::sync::Mutex<String>,
}

impl Client {
    /// Spawns `command`, detects the server's era, and does `tools/list`,
    /// namespacing every tool it offers under `server_name`.
    pub async fn spawn(
        server_name: &str,
        command: &str,
        args: &[String],
        env: &[(String, String)],
    ) -> Result<Self, Error> {
        if server_name.contains(SEP) {
            return Err(Error::NameContainsSeparator(server_name.to_owned()));
        }
        let transport = stdio::Transport::spawn(command, args, env).map_err(Error::Transport)?;
        let mut client = Self {
            transport: tokio::sync::Mutex::new(transport),
            next_id: AtomicI64::new(1),
            tools: HashMap::new(),
            prefix: format!("{server_name}{SEP}"),
            era: Revision::Legacy,
            protocol_version: std::sync::Mutex::new("unnegotiated".to_owned()),
        };
        client.detect_era().await?;
        client.load_tools().await?;
        Ok(client)
    }

    fn fresh_id(&self) -> Id {
        Id::Number(self.next_id.fetch_add(1, Ordering::Relaxed))
    }

    /// `server/discover` first; a successful reply means a modern server, an
    /// `UnsupportedProtocolVersionError` also means a modern server (just one
    /// that rejected the version we asked `server/discover` with -- pick a
    /// mutually supported one instead), and any other error means a server
    /// that has never heard of `server/discover`, which the pre-2026-07-28
    /// lifecycle requires falling back to `initialize` for: the client
    /// SHOULD NOT send requests other than pings before `initialize`
    /// completes, so `tools/list` cannot simply be tried cold.
    async fn detect_era(&mut self) -> Result<(), Error> {
        use sonic_rs::{JsonContainerTrait, JsonValueTrait};

        match self.send_and_wait("server/discover", None::<()>).await {
            Ok(result) => {
                self.era = Revision::Modern;
                let supported: Option<Vec<String>> = result
                    .get("supportedVersions")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|s| s.as_str().map(str::to_owned))
                            .collect()
                    });
                let version = crate::mcp::version::preferred(supported.as_deref());
                *self.protocol_version.lock().expect("poisoned") = version.to_owned();
                Ok(())
            }
            Err(Error::Rpc(e)) => match era::classify_discover_error(&e) {
                Revision::Modern => {
                    self.era = Revision::Modern;
                    if let Some(v) = crate::mcp::version::retry_version(&e) {
                        *self.protocol_version.lock().expect("poisoned") = v.to_owned();
                    }
                    Ok(())
                }
                Revision::Legacy => {
                    self.era = Revision::Legacy;
                    self.initialize().await
                }
            },
            Err(other) => Err(other),
        }
    }

    /// The legacy handshake: `initialize` request, then the fire-and-forget
    /// `notifications/initialized` notification. Only reached once
    /// `detect_era` has already decided the server predates
    /// `server/discover`.
    async fn initialize(&mut self) -> Result<(), Error> {
        use sonic_rs::JsonValueTrait;

        #[derive(serde::Serialize)]
        struct ClientInfo<'a> {
            name: &'a str,
            version: &'a str,
        }
        #[derive(serde::Serialize)]
        #[serde(rename_all = "camelCase")]
        struct InitParams<'a> {
            protocol_version: &'a str,
            capabilities: HashMap<String, String>,
            client_info: ClientInfo<'a>,
        }

        let params = InitParams {
            protocol_version: V2025_11_25,
            capabilities: HashMap::new(),
            client_info: ClientInfo {
                name: "kobold",
                version: env!("CARGO_PKG_VERSION"),
            },
        };
        let result = self.send_and_wait("initialize", Some(params)).await?;

        let negotiated = result
            .get("protocolVersion")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                Error::Protocol("initialize result has no protocolVersion".to_owned())
            })?;
        *self.protocol_version.lock().expect("poisoned") = negotiated.to_owned();

        // No reply expected; a transport failure here is a real error (the
        // connection is unusable) but there is nothing to read a response
        // id against, so this goes straight to the transport rather than
        // through `send_and_wait`.
        let notification = Notification::<()>::new("notifications/initialized", None);
        let mut transport = self.transport.lock().await;
        transport
            .send(&notification)
            .await
            .map_err(Error::Transport)?;
        Ok(())
    }

    /// Walks the `tools/list` result by hand with `JsonValueTrait`/
    /// `JsonContainerTrait` rather than a derived `Deserialize`: a struct
    /// with a `sonic_rs::Value` field fails through `sonic_rs::from_value` --
    /// verified here (`invalid type: newtype struct, expected a valid json`)
    /// -- because `Value`'s zero-copy `Deserialize` impl relies on a raw
    /// parser handing it borrowed bytes directly, which the generic
    /// value-to-value bridge does not do. Direct traversal sidesteps needing
    /// that bridge at all.
    async fn load_tools(&mut self) -> Result<(), Error> {
        use sonic_rs::{JsonContainerTrait, JsonValueTrait};

        let result = self.request("tools/list", None::<()>).await?;

        let tools = result
            .get("tools")
            .and_then(|t| t.as_array())
            .ok_or_else(|| {
                Error::Protocol("tools/list result missing a `tools` array".to_owned())
            })?;

        for t in tools {
            let name = t
                .get("name")
                .and_then(|n| n.as_str())
                .ok_or_else(|| Error::Protocol("a tool in tools/list has no `name`".to_owned()))?
                .to_owned();
            let description = t
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or("")
                .to_owned();
            let input_schema = t
                .get("inputSchema")
                .map(crate::json::to_string)
                .transpose()
                .map_err(Error::Decode)?
                .unwrap_or_else(|| "{}".to_owned());
            self.tools.insert(
                format!("{}{name}", self.prefix),
                ToolInfo {
                    remote_name: name,
                    description,
                    input_schema,
                },
            );
        }
        Ok(())
    }

    /// Sends one request and applies the `-32022` retry: on
    /// `UnsupportedProtocolVersionError`, updates the remembered protocol
    /// version and retries the same call once more, with a fresh id per
    /// spec ("retry ... with a different JSON-RPC id"). Any other error, or
    /// a second `-32022` in a row, is returned rather than looped forever.
    async fn request<T: serde::Serialize + Clone>(
        &self,
        method: &str,
        params: Option<T>,
    ) -> Result<sonic_rs::Value, Error> {
        match self.send_and_wait(method, params.clone()).await {
            Err(Error::Rpc(e)) => match crate::mcp::version::retry_version(&e) {
                Some(v) => {
                    *self.protocol_version.lock().expect("poisoned") = v.to_owned();
                    self.send_and_wait(method, params).await
                }
                None => Err(Error::Rpc(e)),
            },
            other => other,
        }
    }

    /// Sends one request with a fresh id and keeps reading until a message
    /// whose `id` matches arrives. Anything else read in the meantime (a
    /// notification, e.g. `notifications/tools/list_changed`) is dropped
    /// rather than queued -- there is nowhere for it to go yet, since
    /// resource/prompt change notifications are not consumed by anything in
    /// this client.
    async fn send_and_wait<T: serde::Serialize>(
        &self,
        method: &str,
        params: Option<T>,
    ) -> Result<sonic_rs::Value, Error> {
        // 2026-07-28's defining mechanic, not an optional extra: every
        // request declares its version itself, in `_meta`, because that
        // revision has no handshake to negotiate it once up front. A modern
        // path that never stamps this is not an implementation of that
        // revision -- it is a legacy-shaped client that happens to reach a
        // server which tolerates the omission, and a strict server will not.
        //
        // Only stamped once era is actually known to be Modern: `initialize`
        // and the `server/discover` probe itself run before that, and
        // stamping a version before one is negotiated would be declaring
        // something not yet true.
        let params: Option<sonic_rs::Value> = if self.era == Revision::Modern {
            let base = match params {
                Some(p) => to_value(&p)?,
                None => sonic_rs::Value::new_object(),
            };
            Some(self.stamp_meta(base))
        } else {
            params.as_ref().map(to_value).transpose()?
        };

        let id = self.fresh_id();
        let req = Request::new(id.clone(), method, params);
        let mut transport = self.transport.lock().await;
        transport.send(&req).await.map_err(Error::Transport)?;
        loop {
            let line = transport.recv().await.map_err(Error::Transport)?;
            let msg: Incoming = crate::json::from_slice(line.as_bytes()).map_err(Error::Decode)?;
            match msg {
                Incoming::Response {
                    id: got,
                    result,
                    error,
                } if got == id => {
                    if let Some(e) = error {
                        return Err(Error::Rpc(e));
                    }
                    return Ok(result.unwrap_or(sonic_rs::Value::default()));
                }
                Incoming::Response { .. } | Incoming::Notification { .. } => continue,
                Incoming::Request { .. } => {
                    // A server-initiated request (roots/list, sampling, or
                    // legacy-revision elicitation) is not yet answered here.
                    return Err(Error::UnexpectedMessage);
                }
            }
        }
    }

    /// Inserts `_meta.io.modelcontextprotocol/protocolVersion` into `base`,
    /// which must be a JSON object (guaranteed by `send_and_wait`'s callers:
    /// either a caller's real params, already an object, or a fresh empty
    /// one built for a request that otherwise has none).
    fn stamp_meta(&self, mut base: sonic_rs::Value) -> sonic_rs::Value {
        use sonic_rs::JsonValueMutTrait;

        let version = self.protocol_version.lock().expect("poisoned").clone();
        let mut meta = sonic_rs::Value::new_object();
        if let Some(obj) = meta.as_object_mut() {
            obj.insert(&"io.modelcontextprotocol/protocolVersion", version.as_str());
        }
        if let Some(obj) = base.as_object_mut() {
            obj.insert(&"_meta", meta);
        }
        base
    }
}

#[async_trait::async_trait]
impl crate::tools::McpTools for Client {
    fn owns(&self, name: &str) -> bool {
        name.starts_with(&self.prefix)
    }

    async fn call(&self, call: &Call) -> Outcome {
        let Some(info) = self.tools.get(&call.name) else {
            return Outcome::Refused(format!("no such tool '{}'", call.name));
        };
        let args: sonic_rs::Value = if call.arguments.trim().is_empty() {
            sonic_rs::Value::default()
        } else {
            match crate::json::from_slice(call.arguments.as_bytes()) {
                Ok(v) => v,
                Err(e) => return Outcome::Refused(format!("bad arguments: {e}")),
            }
        };

        #[derive(serde::Serialize, Clone)]
        struct CallParams {
            name: String,
            arguments: sonic_rs::Value,
        }
        let params = CallParams {
            name: info.remote_name.clone(),
            arguments: args,
        };

        match self.request("tools/call", Some(params)).await {
            Ok(result) => text_of(&result),
            // A transport or protocol failure is the model's problem to react
            // to, same as any other bad call -- not a reason to end the turn.
            Err(e) => Outcome::Refused(format!("mcp call failed: {e}")),
        }
    }

    fn schemas(&self) -> Vec<(String, String, String)> {
        self.tools
            .iter()
            .map(|(namespaced, info)| {
                (
                    namespaced.clone(),
                    info.description.clone(),
                    info.input_schema.clone(),
                )
            })
            .collect()
    }
}

/// Converts any serializable value to a `sonic_rs::Value` through a real
/// string round-trip (`to_string` then `from_str`), not `sonic_rs::from_value`
/// -- the latter's zero-copy `Deserialize` impl needs a raw parser handing it
/// borrowed bytes directly, and breaks on a value that itself nests a
/// `sonic_rs::Value` (`invalid type: newtype struct, expected a valid json`).
/// Hit twice in this module already (the `tools/list` result and `RpcError`'s
/// `data` field in `jsonrpc.rs`) -- this is the one place left that builds a
/// `Value` from an arbitrary caller-supplied type, so it goes through the
/// string round-trip unconditionally rather than risk a third rediscovery.
fn to_value<T: serde::Serialize>(v: &T) -> Result<sonic_rs::Value, Error> {
    let json = crate::json::to_string(v).map_err(Error::Decode)?;
    sonic_rs::from_str(&json).map_err(Error::Decode)
}

/// Reduces an MCP `tools/call` result to the plain text `Outcome::Done`
/// wants. MCP results carry a `content` array of typed blocks (text, image,
/// resource); only text blocks are readable as tool output today, so others
/// are dropped rather than guessed at -- a lossy join beats a wrong render.
fn text_of(result: &sonic_rs::Value) -> Outcome {
    use sonic_rs::{JsonContainerTrait, JsonValueTrait};

    let is_error = result
        .get("isError")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let text = result
        .get("content")
        .and_then(|c| c.as_array())
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();

    if is_error {
        Outcome::Refused(text)
    } else {
        Outcome::Done(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::McpTools;

    /// A legacy-revision fake server: rejects `server/discover` (method not
    /// found, the standard shape of "I have never heard of this"), then
    /// answers `initialize`, tolerates `notifications/initialized`, and
    /// finally serves `tools/list` / `tools/call`. Hermetic: it only ever
    /// talks to its own stdin/stdout.
    const LEGACY_SERVER: &str = r#"
import sys, json

def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()

# Enforces the pre-2026-07-28 lifecycle for real, rather than tolerating a
# skipped handshake: tools/list and tools/call before notifications/initialized
# get an error instead of an answer, so a Client that skipped initialize hangs
# waiting for a tools/list reply that never comes, rather than quietly working
# anyway.
initialized = False
for line in sys.stdin:
    req = json.loads(line)
    method = req["method"]
    if method == "server/discover":
        send({"jsonrpc": "2.0", "id": req["id"], "error": {"code": -32601, "message": "method not found"}})
    elif method == "initialize":
        send({"jsonrpc": "2.0", "id": req["id"], "result": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "serverInfo": {"name": "legacy-fake", "version": "0.0.0"}
        }})
    elif method == "notifications/initialized":
        initialized = True
    elif method == "tools/list" and initialized:
        send({"jsonrpc": "2.0", "id": req["id"], "result": {"tools": [
            {"name": "echo", "description": "echoes back", "inputSchema": {"type": "object"}}
        ]}})
    elif method == "tools/call" and initialized:
        args = req["params"]["arguments"]
        send({"jsonrpc": "2.0", "id": req["id"], "result": {
            "content": [{"type": "text", "text": "echo: " + json.dumps(args)}]
        }})
    elif req.get("id") is not None:
        send({"jsonrpc": "2.0", "id": req["id"], "error": {"code": -32600, "message": "not initialized"}})
"#;

    /// A modern-revision fake server: answers `server/discover` directly, so
    /// `detect_era` must never fall back to `initialize` -- if it did, this
    /// server would silently ignore the `initialize` request (no handler for
    /// it below) and the test would hang, which is exactly the failure mode
    /// worth catching. Strict about `_meta` too: `tools/list` and
    /// `tools/call` genuinely inspect the request for
    /// `_meta."io.modelcontextprotocol/protocolVersion"` and reject with a
    /// real `-32022` if it is missing or wrong, exactly as a strict
    /// 2026-07-28 server is entitled to. This is what proves `send_and_wait`
    /// actually stamps every request rather than only the ones a lenient
    /// fixture would let slide.
    const MODERN_SERVER: &str = r#"
import sys, json

def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()

def declared_version(req):
    return (req.get("params") or {}).get("_meta", {}).get("io.modelcontextprotocol/protocolVersion")

for line in sys.stdin:
    req = json.loads(line)
    method = req["method"]
    if method == "server/discover":
        send({"jsonrpc": "2.0", "id": req["id"], "result": {
            "resultType": "complete",
            "supportedVersions": ["2026-07-28"],
            "capabilities": {}
        }})
        continue
    if declared_version(req) != "2026-07-28":
        send({"jsonrpc": "2.0", "id": req["id"], "error": {
            "code": -32022, "message": "unsupported protocol version",
            "data": {"supported": ["2026-07-28"], "requested": declared_version(req)}
        }})
        continue
    if method == "tools/list":
        send({"jsonrpc": "2.0", "id": req["id"], "result": {"tools": [
            {"name": "echo", "description": "echoes back", "inputSchema": {"type": "object"}}
        ]}})
    elif method == "tools/call":
        args = req["params"]["arguments"]
        send({"jsonrpc": "2.0", "id": req["id"], "result": {
            "content": [{"type": "text", "text": "echo: " + json.dumps(args)}]
        }})
"#;

    /// A server whose `discover` reply omits `supportedVersions` entirely
    /// (allowed -- it is not a required field), so the client has nothing to
    /// narrow its guess with and stamps its own newest, `2026-07-28`. This
    /// particular server, though, only actually accepts `2025-11-25`-stamped
    /// requests -- a real, inspected mismatch, not a first-call-always-fails
    /// stub -- so the first `tools/list` is genuinely rejected with
    /// `-32022`, and the retried one, restamped after
    /// `version::retry_version` reads `data.supported`, genuinely succeeds.
    const VERSION_MISMATCH_SERVER: &str = r#"
import sys, json

def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()

def declared_version(req):
    return (req.get("params") or {}).get("_meta", {}).get("io.modelcontextprotocol/protocolVersion")

for line in sys.stdin:
    req = json.loads(line)
    method = req["method"]
    if method == "server/discover":
        send({"jsonrpc": "2.0", "id": req["id"], "result": {
            "resultType": "complete", "capabilities": {}
        }})
        continue
    if declared_version(req) != "2025-11-25":
        send({"jsonrpc": "2.0", "id": req["id"], "error": {
            "code": -32022, "message": "unsupported protocol version",
            "data": {"supported": ["2025-11-25"], "requested": declared_version(req)}
        }})
        continue
    if method == "tools/list":
        send({"jsonrpc": "2.0", "id": req["id"], "result": {"tools": []}})
"#;

    /// Same lifecycle enforcement as `LEGACY_SERVER`, but every reply is
    /// preceded by a stray response carrying an id nobody asked for -- a
    /// server-initiated request with a made-up id, or a notification that
    /// picked one up by mistake. `Client::send_and_wait` must skip it and
    /// keep reading rather than returning the first thing that arrives,
    /// since a JSON-RPC reply is matched by id, not by arrival order.
    const STRAY_RESPONSE_SERVER: &str = r#"
import sys, json

def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()

def decoy():
    send({"jsonrpc": "2.0", "id": "not-mine", "result": {"decoy": True}})

initialized = False
for line in sys.stdin:
    req = json.loads(line)
    method = req["method"]
    decoy()
    if method == "server/discover":
        send({"jsonrpc": "2.0", "id": req["id"], "error": {"code": -32601, "message": "method not found"}})
    elif method == "initialize":
        send({"jsonrpc": "2.0", "id": req["id"], "result": {
            "protocolVersion": "2025-11-25", "capabilities": {},
            "serverInfo": {"name": "stray-fake", "version": "0.0.0"}
        }})
    elif method == "notifications/initialized":
        initialized = True
    elif method == "tools/list" and initialized:
        send({"jsonrpc": "2.0", "id": req["id"], "result": {"tools": [
            {"name": "echo", "description": "echoes back", "inputSchema": {"type": "object"}}
        ]}})
    elif method == "tools/call" and initialized:
        send({"jsonrpc": "2.0", "id": req["id"], "result": {
            "content": [{"type": "text", "text": "real reply"}]
        }})
    elif req.get("id") is not None:
        send({"jsonrpc": "2.0", "id": req["id"], "error": {"code": -32600, "message": "not initialized"}})
"#;

    fn python3_available() -> bool {
        std::process::Command::new("python3")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
    }

    async fn spawn_fake(server_name: &str, script: &str) -> Client {
        with_timeout(Client::spawn(
            server_name,
            "python3",
            &["-c".to_string(), script.to_string()],
            &[],
        ))
        .await
        .expect("fake server should start and answer discover/initialize/tools-list")
    }

    /// Bounds a future well under `cargo-mutants`' 20s-per-mutant budget, so a
    /// mutant that breaks the id-matching loop or the era fallback (e.g. the
    /// match guard always failing, or Modern never falling back to
    /// `initialize`) fails fast and deterministically instead of running out
    /// the clock as an uncategorized timeout.
    async fn with_timeout<F: std::future::Future>(f: F) -> F::Output {
        tokio::time::timeout(std::time::Duration::from_secs(5), f)
            .await
            .expect("operation should complete well within the mutant test budget")
    }

    #[tokio::test]
    async fn a_legacy_server_is_initialized_before_tools_list_is_ever_sent() {
        if !python3_available() {
            eprintln!("skipping: python3 not on PATH");
            return;
        }
        let client = spawn_fake("fs", LEGACY_SERVER).await;
        assert_eq!(client.era, Revision::Legacy);
        assert_eq!(*client.protocol_version.lock().unwrap(), V2025_11_25);
        assert_eq!(
            client.schemas().len(),
            1,
            "tools/list must have been reached after initialize"
        );
    }

    #[tokio::test]
    async fn a_modern_server_answering_discover_never_falls_back_to_initialize() {
        if !python3_available() {
            eprintln!("skipping: python3 not on PATH");
            return;
        }
        let client = spawn_fake("fs", MODERN_SERVER).await;
        assert_eq!(client.era, Revision::Modern);
        // If detect_era had fallen back to `initialize` here, MODERN_SERVER
        // has no handler for it and `with_timeout` inside `spawn_fake` would
        // have failed the test instead of this ever running.
        assert_eq!(client.schemas().len(), 1);
    }

    #[tokio::test]
    async fn an_unsupported_version_error_is_retried_once_with_a_supported_version() {
        if !python3_available() {
            eprintln!("skipping: python3 not on PATH");
            return;
        }
        // The client guesses 2026-07-28 (its own newest, since discover gave
        // no hint), the server genuinely rejects that stamped version, and
        // the retry -- restamped as 2025-11-25 -- genuinely succeeds. Proves
        // both that _meta is really being read by the server and that the
        // retry loop really re-stamps rather than just resending.
        let client = spawn_fake("fs", VERSION_MISMATCH_SERVER).await;
        assert_eq!(
            *client.protocol_version.lock().unwrap(),
            V2025_11_25,
            "retry should adopt the server's supported version"
        );
        assert_eq!(
            client.schemas().len(),
            0,
            "the retried tools/list succeeded with an empty list"
        );
    }

    #[tokio::test]
    async fn a_strict_modern_server_accepts_the_stamped_version_on_the_first_try() {
        if !python3_available() {
            eprintln!("skipping: python3 not on PATH");
            return;
        }
        // MODERN_SERVER genuinely inspects _meta and rejects a wrong or
        // missing version with -32022; if send_and_wait failed to stamp
        // tools/list at all, this would fail exactly like the mismatch case
        // above rather than succeed on the first attempt.
        let client = spawn_fake("fs", MODERN_SERVER).await;
        assert_eq!(*client.protocol_version.lock().unwrap(), "2026-07-28");
        assert_eq!(
            client.schemas().len(),
            1,
            "a correctly-stamped request should not need a retry"
        );
    }

    #[tokio::test]
    async fn a_stray_response_carrying_the_wrong_id_is_skipped_not_returned() {
        if !python3_available() {
            eprintln!("skipping: python3 not on PATH");
            return;
        }
        // Bounded: a client that matched on the wrong id (accepting whatever
        // arrives first) would return the decoy instead of hanging, but a
        // client that stopped matching ids altogether would hang forever
        // reading strays -- the timeout turns that failure mode into a fast,
        // deterministic one too.
        let client = spawn_fake("fs", STRAY_RESPONSE_SERVER).await;
        let out = with_timeout(client.call(&Call {
            id: "c1".into(),
            name: "fs__echo".into(),
            arguments: "{}".into(),
        }))
        .await;
        assert_eq!(out, Outcome::Done("real reply".to_owned()));
    }

    #[tokio::test]
    async fn a_tool_from_the_server_is_namespaced_and_described() {
        if !python3_available() {
            eprintln!("skipping: python3 not on PATH");
            return;
        }
        let client = spawn_fake("fs", LEGACY_SERVER).await;
        let schemas = client.schemas();
        assert_eq!(schemas.len(), 1);
        assert_eq!(schemas[0].0, "fs__echo");
        assert_eq!(schemas[0].1, "echoes back");
        assert!(client.owns("fs__echo"));
        assert!(!client.owns("echo"), "an unnamespaced name is not owned");
        assert!(
            !client.owns("other__echo"),
            "another server's namespace is not owned"
        );
    }

    #[tokio::test]
    async fn calling_a_known_tool_returns_its_text_content() {
        if !python3_available() {
            eprintln!("skipping: python3 not on PATH");
            return;
        }
        let client = spawn_fake("fs", LEGACY_SERVER).await;
        let out = with_timeout(client.call(&Call {
            id: "c1".into(),
            name: "fs__echo".into(),
            arguments: r#"{"x":1}"#.into(),
        }))
        .await;
        assert_eq!(out, Outcome::Done(r#"echo: {"x": 1}"#.to_owned()));
    }

    #[tokio::test]
    async fn calling_an_unknown_namespaced_name_is_refused_not_a_hang() {
        if !python3_available() {
            eprintln!("skipping: python3 not on PATH");
            return;
        }
        let client = spawn_fake("fs", LEGACY_SERVER).await;
        let out = client
            .call(&Call {
                id: "c1".into(),
                name: "fs__nope".into(),
                arguments: "{}".into(),
            })
            .await;
        assert!(matches!(out, Outcome::Refused(_)));
    }

    #[tokio::test]
    async fn a_server_name_containing_the_namespace_separator_is_refused_at_registration() {
        // The whole reason `__` is disallowed in a server's own name: it
        // could otherwise present tools under a fabricated prefix like
        // `evil__file_read`'s namespace being indistinguishable from a
        // genuine registration for server "evil". Rejected, not renamed.
        match Client::spawn("evil__spoof", "true", &[], &[]).await {
            Err(Error::NameContainsSeparator(n)) => assert_eq!(n, "evil__spoof"),
            Err(other) => panic!("expected NameContainsSeparator, got a different error: {other}"),
            Ok(_) => panic!("expected registration to be refused, got a live client"),
        }
    }

    #[test]
    fn a_result_marked_iserror_becomes_a_refused_outcome_not_a_done_one() {
        let result: sonic_rs::Value = sonic_rs::from_str(
            r#"{"isError": true, "content": [{"type": "text", "text": "boom"}]}"#,
        )
        .unwrap();
        assert_eq!(text_of(&result), Outcome::Refused("boom".to_owned()));
    }

    #[test]
    fn a_result_with_no_content_array_yields_empty_text_rather_than_panicking() {
        let result: sonic_rs::Value = sonic_rs::from_str(r#"{}"#).unwrap();
        assert_eq!(text_of(&result), Outcome::Done(String::new()));
    }

    #[test]
    fn non_text_content_blocks_are_dropped_rather_than_guessed_at() {
        let result: sonic_rs::Value = sonic_rs::from_str(
            r#"{"content": [{"type": "image", "data": "..."}, {"type": "text", "text": "kept"}]}"#,
        )
        .unwrap();
        assert_eq!(text_of(&result), Outcome::Done("kept".to_owned()));
    }

    #[test]
    fn each_error_variant_displays_a_message_naming_its_own_failure() {
        assert!(Error::Protocol("x".into()).to_string().contains('x'));
        assert!(Error::UnexpectedMessage
            .to_string()
            .contains("not our reply"));
        assert!(Error::NameContainsSeparator("evil__x".into())
            .to_string()
            .contains("evil__x"));
    }
}
