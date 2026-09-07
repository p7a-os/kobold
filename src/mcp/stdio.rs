//! stdio transport: the MCP server is a child process, messages are
//! newline-delimited JSON on its stdin/stdout.
//!
//! Framing per spec: "Messages are delimited by newlines, and MUST NOT
//! contain embedded newlines." `sonic_rs::to_string` never emits a raw
//! newline inside a JSON string (it escapes to `\n`), so a single `write_all`
//! of the serialized line plus `\n` is a correctly framed message. stderr is
//! explicitly non-protocol per spec ("the client... SHOULD NOT assume stderr
//! output indicates error conditions"), so it is inherited straight to our
//! own stderr rather than parsed.

use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

#[derive(Debug)]
pub enum Error {
    Spawn(std::io::Error),
    Write(std::io::Error),
    Read(std::io::Error),
    /// The child exited (stdout closed) before or instead of answering.
    Closed,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Spawn(e) => write!(f, "spawn: {e}"),
            Error::Write(e) => write!(f, "write: {e}"),
            Error::Read(e) => write!(f, "read: {e}"),
            Error::Closed => write!(f, "server process closed stdout"),
        }
    }
}

impl std::error::Error for Error {}

pub struct Transport {
    child: Child,
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
}

impl Transport {
    /// Launches `command` with `args`, wiring stdin/stdout as pipes and
    /// leaving stderr inherited so a misbehaving server's diagnostics land in
    /// the terminal the user is already looking at instead of being lost.
    ///
    /// The child's environment is built rather than inherited. See
    /// `crate::childenv`: an MCP server is a separate program, often one the
    /// user did not write, and the parent's environment is where the API key
    /// lives.
    pub fn spawn(command: &str, args: &[String], env: &[(String, String)]) -> Result<Self, Error> {
        let mut cmd = Command::new(command);
        // Before any `env` call, or the allowlist below is cleared straight
        // back out again and this reads as working while passing nothing.
        crate::childenv::restrict(&mut cmd);
        // Last, so a server's own configuration wins over the base list --
        // a server given a `PATH` in its settings meant that `PATH`.
        for (key, value) in env {
            cmd.env(key, value);
        }
        let mut child = cmd
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .map_err(Error::Spawn)?;

        let stdin = child.stdin.take().expect("piped stdin was requested");
        let stdout = child.stdout.take().expect("piped stdout was requested");
        Ok(Self {
            child,
            stdin,
            lines: BufReader::new(stdout).lines(),
        })
    }

    /// Serializes `message` and writes it as one framed line.
    pub async fn send<T: serde::Serialize>(&mut self, message: &T) -> Result<(), Error> {
        let mut line = crate::json::to_string(message).map_err(|e| {
            Error::Write(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                e.to_string(),
            ))
        })?;
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(Error::Write)
    }

    /// Reads one line and hands back its raw bytes for the caller to parse as
    /// `jsonrpc::Incoming`. Left unparsed here so this module stays ignorant
    /// of MCP method semantics, matching the seam `ws.rs` draws between
    /// transport and event decoding.
    pub async fn recv(&mut self) -> Result<String, Error> {
        match self.lines.next_line().await.map_err(Error::Read)? {
            Some(line) => Ok(line),
            None => Err(Error::Closed),
        }
    }

    /// True once the child has exited, so a caller can distinguish "no
    /// message yet" from "never coming."
    pub fn try_wait_exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }
}

#[cfg(test)]
mod tests {

    /// Reads one variable out of a spawned child, through the real spawn.
    async fn child_sees(var: &str, env: &[(String, String)]) -> Option<String> {
        let script = format!("printf '%s\\n' \"${{{var}:-\u{1}ABSENT\u{1}}}\"");
        let mut t = Transport::spawn("sh", &["-c".to_owned(), script], env)
            .expect("sh is on PATH in test environments");
        let line = t.recv().await.expect("the child prints one line");
        (line != "\u{1}ABSENT\u{1}").then_some(line)
    }

    #[tokio::test]
    async fn a_server_is_not_handed_the_environment_kobold_was_started_with() {
        // The defect this exists to prevent: `run.sh` puts LLM_API_KEY in
        // kobold's environment, and a spawned server used to inherit it
        // wholesale. Confirmed before the fix -- a child printed
        // "sk-SECRET-would-be-real" back.
        //
        // Set here rather than assumed present, so the test proves the
        // filtering rather than the absence of a variable that was never
        // there. That distinction is the whole assertion.
        std::env::set_var("LLM_API_KEY", "sk-must-not-reach-a-subprocess");
        std::env::set_var("KOBOLD_TEST_UNLISTED", "also-must-not");

        assert_eq!(
            child_sees("LLM_API_KEY", &[]).await,
            None,
            "the API key reached the server"
        );
        assert_eq!(
            child_sees("KOBOLD_TEST_UNLISTED", &[]).await,
            None,
            "so did an unlisted var"
        );

        std::env::remove_var("LLM_API_KEY");
        std::env::remove_var("KOBOLD_TEST_UNLISTED");
    }

    #[tokio::test]
    async fn the_base_list_still_reaches_the_server() {
        // The partner to the test above. A spawn that passed nothing at all
        // -- or one where `env_clear` ran after the allowlist instead of
        // before -- would satisfy every "must not arrive" assertion
        // perfectly, and break every real server.
        assert!(
            child_sees("PATH", &[]).await.is_some(),
            "PATH must survive, or a server invoked as `npx` cannot find its interpreter"
        );
        assert!(child_sees("HOME", &[]).await.is_some(), "HOME must survive");
    }

    #[tokio::test]
    async fn a_server_receives_exactly_the_variables_it_was_granted() {
        // The point of the per-server map: a credential reaches the one
        // server it was named for, and nothing it was not named for.
        let granted = vec![("GITHUB_TOKEN".to_owned(), "ghp-for-this-server".to_owned())];
        assert_eq!(
            child_sees("GITHUB_TOKEN", &granted).await.as_deref(),
            Some("ghp-for-this-server"),
            "a granted variable must arrive"
        );
        assert_eq!(
            child_sees("GITHUB_TOKEN", &[]).await,
            None,
            "and must not arrive at a server that was not granted it"
        );
    }

    #[tokio::test]
    async fn a_servers_own_env_overrides_the_base_list() {
        // Applied after the base list, so a server given its own value for
        // one of those names meant that value rather than kobold's.
        //
        // `HOME` rather than `PATH`: overriding PATH genuinely works, which
        // is the problem -- the child can then no longer find `sh`, and the
        // test would be asserting on a spawn failure instead of on an
        // environment.
        let granted = vec![
            ("HOME", "/only/this"),
            ("PATH", &std::env::var("PATH").unwrap()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect::<Vec<_>>();
        assert_eq!(
            child_sees("HOME", &granted).await.as_deref(),
            Some("/only/this"),
            "the server's own value should win over the base list"
        );
    }
    use super::*;

    /// `cat` echoes stdin to stdout unchanged, which makes it a stand-in MCP
    /// server for framing tests: whatever we send, framed correctly, comes
    /// back as one line.
    #[tokio::test]
    async fn a_sent_message_round_trips_through_a_line_echoing_child_process() {
        let mut t = Transport::spawn("cat", &[], &[]).expect("cat is on PATH in test environments");
        let req = crate::mcp::jsonrpc::Request::<()>::new(1.into(), "ping", None);
        t.send(&req).await.unwrap();
        // Bounded rather than a bare `.await`: if `send` silently did nothing,
        // `cat` would never see input and this would hang instead of failing.
        let line = tokio::time::timeout(std::time::Duration::from_secs(5), t.recv())
            .await
            .expect("recv should not hang if send actually wrote the line")
            .unwrap();
        assert!(line.contains("\"method\":\"ping\""), "{line}");
    }

    #[test]
    fn a_closed_error_displays_a_message_naming_stdout() {
        assert!(Error::Closed.to_string().contains("stdout"));
    }

    #[tokio::test]
    async fn try_wait_exited_is_false_while_the_child_is_still_running_and_true_after_it_exits() {
        // `sleep 5` outlives the check; `true` has already exited by the time
        // we get to ask, since spawning and scheduling the child both yield.
        let mut running = Transport::spawn("sleep", &["5".to_string()], &[])
            .expect("sleep is on PATH in test environments");
        assert!(!running.try_wait_exited());

        let mut exited =
            Transport::spawn("true", &[], &[]).expect("true is on PATH in test environments");
        // Give the child a moment to actually exit before polling it.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(exited.try_wait_exited());
    }

    #[tokio::test]
    async fn sending_after_the_child_has_exited_and_closed_its_stdin_reports_a_write_error() {
        let mut t =
            Transport::spawn("true", &[], &[]).expect("true is on PATH in test environments");
        for _ in 0..50 {
            if t.try_wait_exited() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let req = crate::mcp::jsonrpc::Request::<()>::new(1.into(), "ping", None);
        // Write enough, and repeatedly enough, that a closed pipe surfaces as
        // an error instead of being absorbed by the kernel's pipe buffer.
        let mut last = Ok(());
        for _ in 0..2000 {
            last = t.send(&req).await;
            if last.is_err() {
                break;
            }
        }
        assert!(matches!(last, Err(Error::Write(_))), "{last:?}");
    }

    #[tokio::test]
    async fn reading_after_the_child_exits_reports_closed_rather_than_hanging() {
        // `true` exits immediately, closing its stdout with nothing written.
        let mut t =
            Transport::spawn("true", &[], &[]).expect("true is on PATH in test environments");
        let err = t.recv().await.unwrap_err();
        assert!(matches!(err, Error::Closed), "{err}");
    }
}
