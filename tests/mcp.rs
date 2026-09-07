//! An MCP server, end to end, through the surface the rest of kobold uses.
//!
//! The unit tests in `src/mcp/` reach inside the client. These use only what is
//! public and go the whole way: spawn a real subprocess speaking the protocol,
//! let the client detect its era and list its tools, register it as a source,
//! and call one of its tools through `tools::run` -- the same function the
//! event loop calls when the model asks for something.
//!
//! Hermetic. The server is a Python script written here, so no network, no
//! installed MCP server, and nothing to configure. What it is *not* is a mock:
//! it parses real JSON-RPC off its stdin and answers on stdout, and it enforces
//! its own lifecycle, so a client that skipped the handshake would fail here
//! rather than be quietly tolerated.

use kobold::mcp::client::Client;
use kobold::tools::{run, Call, McpTools, Outcome, Sources};

/// A 2025-11-25 server. Refuses everything but `initialize` until the client
/// has said `notifications/initialized`, which is what the lifecycle requires
/// and what makes a skipped handshake fail rather than pass by accident.
const LEGACY: &str = r#"
import sys, json
ready = False
def send(o): sys.stdout.write(json.dumps(o) + "\n"); sys.stdout.flush()
for line in sys.stdin:
    line = line.strip()
    if not line: continue
    m = json.loads(line)
    method, mid = m.get("method"), m.get("id")
    if method == "notifications/initialized":
        ready = True; continue
    if method == "server/discover":
        send({"jsonrpc":"2.0","id":mid,"error":{"code":-32601,"message":"unknown method"}}); continue
    if method == "initialize":
        send({"jsonrpc":"2.0","id":mid,"result":{
            "protocolVersion":"2025-11-25",
            "capabilities":{"tools":{}},
            "serverInfo":{"name":"legacy-fixture","version":"1"}}}); continue
    if not ready:
        send({"jsonrpc":"2.0","id":mid,"error":{"code":-32002,"message":"not initialized"}}); continue
    if method == "tools/list":
        send({"jsonrpc":"2.0","id":mid,"result":{"tools":[
            {"name":"echo","description":"Echo the text back to the caller.",
             "inputSchema":{"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}}]}}); continue
    if method == "tools/call":
        text = m["params"]["arguments"].get("text","")
        send({"jsonrpc":"2.0","id":mid,"result":{"content":[{"type":"text","text":"echo: "+text}]}}); continue
    send({"jsonrpc":"2.0","id":mid,"error":{"code":-32601,"message":"unknown method"}})
"#;

/// A 2026-07-28 server: answers `server/discover`, so no handshake, and it
/// rejects `initialize` outright to prove the client never sends one.
const MODERN: &str = r#"
import sys, json
def send(o): sys.stdout.write(json.dumps(o) + "\n"); sys.stdout.flush()
for line in sys.stdin:
    line = line.strip()
    if not line: continue
    m = json.loads(line)
    method, mid = m.get("method"), m.get("id")
    if method == "initialize":
        send({"jsonrpc":"2.0","id":mid,"error":{"code":-32601,"message":"this server is modern; do not initialize"}}); continue
    if method == "server/discover":
        send({"jsonrpc":"2.0","id":mid,"result":{
            "protocolVersions":["2026-07-28"],
            "serverInfo":{"name":"modern-fixture","version":"1"}}}); continue
    if method == "tools/list":
        send({"jsonrpc":"2.0","id":mid,"result":{"tools":[
            {"name":"echo","description":"Echo the text back to the caller.",
             "inputSchema":{"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}}]}}); continue
    if method == "tools/call":
        text = m["params"]["arguments"].get("text","")
        send({"jsonrpc":"2.0","id":mid,"result":{"content":[{"type":"text","text":"echo: "+text}]}}); continue
    send({"jsonrpc":"2.0","id":mid,"error":{"code":-32601,"message":"unknown method"}})
"#;

fn python3_available() -> bool {
    std::process::Command::new("python3")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn block<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(f)
}

/// Async, and every test drives it inside a single `block_on`.
///
/// A `Client` owns a child process registered with the runtime that spawned it,
/// so connecting under one runtime and calling under another fails with "a
/// Tokio 1.x context was found, but it is being shutdown" -- which reads like a
/// protocol fault and is not one. One runtime per test.
async fn connect(name: &str, script: &str) -> Client {
    tokio::time::timeout(
        std::time::Duration::from_secs(20),
        Client::spawn(name, "python3", &["-c".to_owned(), script.to_owned()], &[]),
    )
    .await
    .expect("the server should answer well within the timeout")
    .expect("the server should connect")
}

fn call(name: &str, arguments: &str) -> Call {
    Call {
        id: "call_it".into(),
        name: name.into(),
        arguments: arguments.into(),
    }
}

#[test]
fn a_legacy_server_is_handshaken_and_its_tool_answers_through_the_tool_path() {
    if !python3_available() {
        eprintln!("skipping: no python3 to run the fixture server");
        return;
    }
    block(async {
        let client = connect("files", LEGACY).await;

        // Namespaced, so the model's name and kobold's routing agree.
        let names: Vec<String> = client.schemas().into_iter().map(|(n, _, _)| n).collect();
        assert_eq!(names, vec!["files__echo".to_owned()]);
        assert!(client.owns("files__echo"));
        assert!(!client.owns("echo"), "the bare name must not be claimed");

        // The whole way through: `run` is what the event loop calls.
        let mut set = Sources::new();
        set.push(Box::new(client));
        let root = std::env::current_dir().expect("cwd");
        let out = run(
            &root,
            &call("files__echo", r#"{"text":"hello"}"#),
            Some(&set),
        )
        .await;
        let Outcome::Done(text) = out else {
            panic!("the server should have answered: {out:?}")
        };
        assert!(text.contains("echo: hello"), "{text}");
    });
}

#[test]
fn a_modern_server_is_never_sent_an_initialize() {
    // The fixture rejects `initialize`, so a client that sent one would fail to
    // connect at all. That is the assertion: era detection has to choose the
    // right lifecycle, not try both.
    if !python3_available() {
        eprintln!("skipping: no python3 to run the fixture server");
        return;
    }
    block(async {
        let client = connect("api", MODERN).await;
        assert_eq!(
            client
                .schemas()
                .into_iter()
                .map(|(n, _, _)| n)
                .collect::<Vec<_>>(),
            vec!["api__echo".to_owned()]
        );

        let mut set = Sources::new();
        set.push(Box::new(client));
        let root = std::env::current_dir().expect("cwd");
        let out = run(&root, &call("api__echo", r#"{"text":"there"}"#), Some(&set)).await;
        let Outcome::Done(text) = out else {
            panic!("the server should have answered: {out:?}")
        };
        assert!(text.contains("echo: there"), "{text}");
    });
}

#[test]
fn both_revisions_can_be_connected_at_once_and_neither_shadows_the_other() {
    // The reason tools are namespaced by server rather than only on collision:
    // these two offer a tool of the same name, and both have to stay reachable.
    if !python3_available() {
        eprintln!("skipping: no python3 to run the fixture servers");
        return;
    }
    block(async {
        let mut set = Sources::new();
        set.push(Box::new(connect("old", LEGACY).await));
        set.push(Box::new(connect("new", MODERN).await));

        let names: Vec<String> = set.all_schemas().into_iter().map(|(n, _, _)| n).collect();
        assert_eq!(names, vec!["old__echo".to_owned(), "new__echo".to_owned()]);

        let root = std::env::current_dir().expect("cwd");
        for server in ["old", "new"] {
            let out = run(
                &root,
                &call(&format!("{server}__echo"), r#"{"text":"x"}"#),
                Some(&set),
            )
            .await;
            assert!(
                matches!(out, Outcome::Done(ref t) if t.contains("echo: x")),
                "{server} did not answer: {out:?}"
            );
        }
    });
}

#[test]
fn a_server_cannot_shadow_a_local_tool_however_it_names_its_own() {
    // The security property from outside, against a real server rather than a
    // fake: local names are matched first, so `file_read` is ours whatever a
    // server offers.
    if !python3_available() {
        eprintln!("skipping: no python3 to run the fixture server");
        return;
    }
    block(async {
        let mut set = Sources::new();
        set.push(Box::new(connect("files", LEGACY).await));

        let dir = std::env::temp_dir().join(format!("kobold-mcp-it-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("local.txt"), b"from the local tool").expect("write");

        let out = run(
            &dir,
            &call("file_read", r#"{"path":"local.txt"}"#),
            Some(&set),
        )
        .await;
        assert_eq!(out, Outcome::Done("from the local tool".to_owned()));
        let _ = std::fs::remove_dir_all(&dir);
    });
}

#[test]
fn a_server_that_does_not_start_is_an_error_rather_than_a_hang() {
    // A bad command in a config file must fail quickly and say so, not leave
    // the session waiting on a process that will never speak.
    let started = block(async {
        tokio::time::timeout(
            std::time::Duration::from_secs(20),
            Client::spawn("broken", "definitely-not-a-real-command-xyz", &[], &[]),
        )
        .await
    });
    match started {
        Err(_) => panic!("connecting to a missing command hung instead of failing"),
        Ok(Ok(_)) => panic!("a missing command should not connect"),
        Ok(Err(_)) => {}
    }
}
