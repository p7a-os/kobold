//! The adapter as a process, at its stdio boundary.
//!
//! Hermetic, which bounds what can be checked here: everything past the
//! startup frame opens a WebSocket to the real provider, and there is no fake
//! Responses server in the tree to point it at. So these cover the contract
//! that holds *before* any network -- that a startup frame is required, comes
//! first, and must be a startup frame -- and nothing beyond it.
//!
//! What that leaves untested is worth naming rather than implying: the
//! provider task itself has no tests and did not have any before this crate
//! existed. Moving it across a process boundary did not change that, but it
//! is now the far side of an interface rather than a function call.

use std::io::Write;
use std::process::{Command, Stdio};

fn adapter() -> std::path::PathBuf {
    // Sits next to the test binary: target/<profile>/deps/adapter-<hash>
    let mut p = std::env::current_exe().expect("test exe path");
    p.pop();
    if p.ends_with("deps") {
        p.pop();
    }
    p.join("kobold-openai")
}

/// Feeds `stdin` to the adapter and waits for it, with no network reachable
/// past the startup frame.
fn run(stdin: &str) -> std::process::Output {
    let mut child = Command::new(adapter())
        .env_remove("LLM_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the adapter");
    child
        .stdin
        .as_mut()
        .expect("piped")
        .write_all(stdin.as_bytes())
        .expect("write");
    child.wait_with_output().expect("wait")
}

#[test]
fn stdin_closing_before_the_startup_frame_is_an_error_not_a_silent_exit() {
    // The adapter has been told nothing and cannot do anything. Exiting 0
    // here would look identical to a clean shutdown, which is the failure
    // mode Kobold spent a commit fixing for its own dead socket.
    let out = run("");
    assert!(!out.status.success(), "exited zero with nothing to do");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("startup frame"),
        "the reason must say what was missing, got {err:?}"
    );
    assert!(
        out.stdout.is_empty(),
        "nothing may reach stdout but protocol"
    );
}

#[test]
fn a_first_line_that_is_not_a_startup_frame_is_refused_and_says_what_arrived() {
    // The realistic causes are a version skew between Kobold and an adapter,
    // and an adapter or wrapper script printing something to stdout. Both are
    // undiagnosable unless the message quotes what it actually read.
    for (name, line) in [
        ("a diagnostic", "Loading configuration...\n"),
        ("a command sent first", "{\"Quit\":null}\n"),
        ("valid JSON of the wrong shape", "{\"hello\":\"world\"}\n"),
        ("not JSON at all", "<html>502 Bad Gateway</html>\n"),
    ] {
        let out = run(line);
        assert!(!out.status.success(), "{name}: exited zero");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("not a protocol message"),
            "{name}: unhelpful error {err:?}"
        );
        assert!(out.stdout.is_empty(), "{name}: wrote to stdout");
    }
}

#[test]
fn the_startup_frame_is_accepted_and_carries_the_credential_off_the_command_line() {
    // The partner to the refusals above: they are all satisfied by an adapter
    // that rejects everything, so one frame has to get through. It cannot
    // reach a provider here, so what is asserted is that it got *past* the
    // frame -- it fails at the network instead of at the parse.
    let startup = kobold_proto::codec::encode(&kobold_proto::Startup {
        egress: None,
        api_key: "sk-not-a-real-key".to_owned(),
        model: kobold_proto::Model {
            name: "gpt-5.6-luna".to_owned(),
            effort: "none".to_owned(),
            server_tools: Vec::new(),
            tools: Vec::new(),
        },
    })
    .expect("encode");
    let out = run(&startup);

    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !err.contains("not a protocol message") && !err.contains("startup frame"),
        "the frame should have parsed, got {err:?}"
    );

    // Whatever happened next, it was reported as protocol rather than
    // swallowed: a connection that cannot be made is a `Disconnected` on
    // stdout, which is how Kobold learns to tell the user.
    let stdout = String::from_utf8_lossy(&out.stdout);
    for line in stdout.lines().filter(|l| !l.trim().is_empty()) {
        kobold_proto::codec::decode::<kobold_proto::IncomingFrame>(line)
            .unwrap_or_else(|e| panic!("non-protocol line on stdout: {line:?} ({e})"));
    }

    // And the key never became an argument, which is the whole reason it
    // arrives on stdin: a command line is world-readable in `ps`.
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("sk-not-a-real-key"),
        "the credential was echoed to stderr"
    );
}
