//! Integration tests over the built binary.
//!
//! Tests that need the network are `#[ignore]`d so `cargo test` stays offline
//! and deterministic; run them with `cargo test -- --ignored` and a key set.

use std::process::{Command, Stdio};

fn bin() -> std::path::PathBuf {
    // Sits next to the test binary: target/<profile>/deps/cli-<hash>
    let mut p = std::env::current_exe().expect("test exe path");
    p.pop();
    if p.ends_with("deps") {
        p.pop();
    }
    let candidate = p.join("kobold");
    if candidate.exists() {
        return candidate;
    }
    let fallback = std::path::PathBuf::from("target/debug/kobold");
    if fallback.exists() {
        return fallback;
    }
    candidate
}

fn run(args: &[&str], env: &[(&str, &str)], stdin: Option<&str>) -> std::process::Output {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut cmd = Command::new(bin());
    cmd.current_dir(tmp.path());
    cmd.args(args);
    // Cleared so a developer's own key or config cannot change the result.
    cmd.env_remove("LLM_API_KEY").env_remove("OPENAI_API_KEY");
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn kobold");
    if let Some(text) = stdin {
        use std::io::Write;
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
    }
    child.wait_with_output().expect("run kobold")
}

#[test]
fn missing_api_key_fails_with_a_useful_message() {
    let out = run(&["-p", "hello"], &[], None);
    assert!(!out.status.success(), "should exit non-zero");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("LLM_API_KEY"), "unhelpful error: {err}");
}

#[test]
fn empty_prompt_is_rejected() {
    let out = run(&["-p"], &[("LLM_API_KEY", "x")], Some("   \n"));
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("empty prompt"));
}

#[test]
fn bad_key_exits_non_zero_rather_than_hanging() {
    // Exercises the whole connect path; a 401 must surface as a failure.
    let out = run(&["-p", "hi"], &[("LLM_API_KEY", "sk-invalid")], None);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty(), "no partial output on a failed turn");
}

#[test]
fn stdout_carries_the_reply_and_nothing_else() {
    // In `-p` mode stdout is the caller's data, so anything decorative written
    // there is corruption of it. The progress bar used to go to stdout gated
    // only on the terminal being one that supports it -- so this passed
    // wherever TERM_PROGRAM was unset and failed on the machines the feature
    // was built for, writing two OSC sequences into a redirected file.
    //
    // Set here rather than inherited, so the check does not depend on the
    // terminal whoever runs the suite happens to use.
    for term in ["ghostty", "wezterm", "xterm", ""] {
        let out = run(
            &["-p", "hi"],
            &[
                ("LLM_API_KEY", "sk-invalid"),
                ("TERM_PROGRAM", term),
                ("TERM", term),
            ],
            None,
        );
        assert!(
            out.stdout.is_empty(),
            "TERM_PROGRAM={term:?} put {:?} on stdout",
            String::from_utf8_lossy(&out.stdout)
        );
    }
}

#[test]
#[ignore = "needs LLM_API_KEY and network"]
fn programmatic_turn_prints_the_reply() {
    let key = std::env::var("LLM_API_KEY").expect("set LLM_API_KEY to run this");
    let out = run(
        &["-p", "Reply with exactly: integration ok"],
        &[("LLM_API_KEY", key.as_str())],
        None,
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("integration ok"), "unexpected reply: {text}");
}

#[test]
#[ignore = "needs LLM_API_KEY and network"]
fn prompt_can_come_from_stdin() {
    let key = std::env::var("LLM_API_KEY").expect("set LLM_API_KEY to run this");
    let out = run(
        &["-p"],
        &[("LLM_API_KEY", key.as_str())],
        Some("Say the word pipeline only."),
    );
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout)
        .to_lowercase()
        .contains("pipeline"));
}
