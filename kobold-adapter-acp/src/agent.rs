//! Supervision, process execution, and piped I/O for the external ACP agent.

use std::path::Path;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use crate::proto::JsonRpcMessage;

/// Manages a running ACP agent subprocess and its stdio pipes.
pub struct AgentProcess {
    child: Child,
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
}

impl AgentProcess {
    /// Spawns an agent command in the given working directory.
    pub fn spawn(cmd: &str, args: &[String], workdir: &Path) -> Result<Self, std::io::Error> {
        let mut command = Command::new(cmd);
        command
            .args(args)
            .current_dir(workdir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());

        let mut child = command.spawn()?;
        let stdin = child.stdin.take().expect("child stdin");
        let stdout = child.stdout.take().expect("child stdout");
        let lines = BufReader::new(stdout).lines();

        Ok(Self {
            child,
            stdin,
            lines,
        })
    }

    /// Sends a JSON-RPC message to the agent over stdin.
    pub async fn send(&mut self, msg: &JsonRpcMessage) -> Result<(), std::io::Error> {
        let mut line = sonic_rs::to_string(msg)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.flush().await?;
        Ok(())
    }

    /// Reads the next JSON-RPC message from the agent's stdout.
    pub async fn recv(&mut self) -> Result<Option<JsonRpcMessage>, std::io::Error> {
        while let Some(line) = self.lines.next_line().await? {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(msg) = sonic_rs::from_str::<JsonRpcMessage>(trimmed) {
                return Ok(Some(msg));
            }
        }
        Ok(None)
    }

    /// Terminates the child agent process.
    pub fn kill(&mut self) -> Result<(), std::io::Error> {
        self.child.start_kill()
    }
}

impl Drop for AgentProcess {
    fn drop(&mut self) {
        let _ = self.kill();
    }
}
