//! Shell execution in a persistent tmux session.
//!
//! tmux, not a bare child process, because an agent loop wants a shell that
//! survives between tool calls: cwd, env, and background jobs persist across
//! turns the way they would for a human at a terminal.

use std::path::Path;
use tokio::process::Command;

pub struct Session {
    name: String,
}

#[derive(Debug)]
pub enum Error {
    Spawn(std::io::Error),
    Tmux(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Spawn(e) => write!(f, "tmux spawn: {e}"),
            Error::Tmux(s) => write!(f, "tmux: {s}"),
        }
    }
}

impl std::error::Error for Error {}

impl Session {
    pub async fn open(name: &str, workdir: &Path) -> Result<Self, Error> {
        let out = Command::new("tmux")
            .args(["new-session", "-d", "-s", name, "-c"])
            .arg(workdir)
            .output()
            .await
            .map_err(Error::Spawn)?;

        // Re-attaching to an existing session is success, not failure.
        let stderr = String::from_utf8_lossy(&out.stderr);
        if !out.status.success() && !stderr.contains("duplicate session") {
            return Err(Error::Tmux(stderr.into_owned()));
        }
        Ok(Self {
            name: name.to_owned(),
        })
    }

    pub async fn send(&self, line: &str) -> Result<(), Error> {
        let out = Command::new("tmux")
            .args(["send-keys", "-t", &self.name, line, "Enter"])
            .output()
            .await
            .map_err(Error::Spawn)?;
        if out.status.success() {
            Ok(())
        } else {
            Err(Error::Tmux(
                String::from_utf8_lossy(&out.stderr).into_owned(),
            ))
        }
    }

    /// Last `lines` of the pane. Cap this: a huge dump costs far more in input
    /// tokens and model latency than it will ever cost to parse.
    pub async fn capture(&self, lines: u32) -> Result<String, Error> {
        let out = Command::new("tmux")
            .args([
                "capture-pane",
                "-p",
                "-t",
                &self.name,
                "-S",
                &format!("-{lines}"),
            ])
            .output()
            .await
            .map_err(Error::Spawn)?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(Error::Tmux(
                String::from_utf8_lossy(&out.stderr).into_owned(),
            ))
        }
    }

    pub async fn kill(&self) -> Result<(), Error> {
        Command::new("tmux")
            .args(["kill-session", "-t", &self.name])
            .output()
            .await
            .map_err(Error::Spawn)?;
        Ok(())
    }
}
