use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use kobold_types::ToolError;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;
use tokio::time::timeout;

/// Maximum bytes retained from bash stdout/stderr before truncation.
pub const MAX_OUTPUT_BYTES: usize = 64 * 1024;

/// Result of executing a command inside a sandbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub timed_out: bool,
}

impl ExecutionResult {
    pub fn success(stdout: impl Into<String>) -> Self {
        Self {
            stdout: stdout.into(),
            stderr: String::new(),
            exit_code: 0,
            timed_out: false,
        }
    }

    pub fn failed(exit_code: i32, stdout: impl Into<String>, stderr: impl Into<String>) -> Self {
        Self {
            stdout: stdout.into(),
            stderr: stderr.into(),
            exit_code,
            timed_out: false,
        }
    }

    pub fn timeout(stdout: impl Into<String>, stderr: impl Into<String>) -> Self {
        Self {
            stdout: stdout.into(),
            stderr: stderr.into(),
            exit_code: 124,
            timed_out: true,
        }
    }

    /// Format consolidated output string for model consumption.
    pub fn format_output(&self) -> String {
        let mut out = String::new();
        if !self.stdout.is_empty() {
            out.push_str(&truncate_output(&self.stdout));
        }
        if !self.stderr.is_empty() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str("[stderr]\n");
            out.push_str(&truncate_output(&self.stderr));
        }
        if self.timed_out {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str("[execution timed out]");
        }
        out
    }
}

fn truncate_output(s: &str) -> String {
    if s.len() > MAX_OUTPUT_BYTES {
        let mut truncated = s[..MAX_OUTPUT_BYTES].to_string();
        truncated.push_str(&format!(
            "\n\n[output truncated: {} bytes total, first {} shown]",
            s.len(),
            MAX_OUTPUT_BYTES
        ));
        truncated
    } else {
        s.to_string()
    }
}

/// Abstract contract for sandbox execution backends.
#[async_trait]
pub trait SandboxExecutor: Send + Sync {
    async fn execute(
        &self,
        command: &str,
        session_id: Option<&str>,
        timeout_duration: Duration,
    ) -> Result<ExecutionResult, ToolError>;
}

/// Sandbox mode selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxMode {
    HostProcess,
    MicroVm,
}

/// Host-based sandbox executor running commands in isolated subprocesses.
pub struct HostProcessExecutor {
    workspace_root: PathBuf,
    sessions: Arc<Mutex<HashMap<String, PersistentSession>>>,
}

struct PersistentSession {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
}

impl HostProcessExecutor {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self {
            workspace_root,
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    async fn execute_one_shot(
        &self,
        cmd_str: &str,
        duration: Duration,
    ) -> Result<ExecutionResult, ToolError> {
        let mut cmd = Command::new("/bin/bash");
        cmd.arg("-c")
            .arg(cmd_str)
            .current_dir(&self.workspace_root)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Sanitized environment variables
        cmd.env("TERM", "dumb")
            .env("CLICOLOR", "0")
            .env("PAGER", "cat")
            .env("GIT_TERMINAL_PROMPT", "0");

        let child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                return Err(ToolError::Execution(format!(
                    "Failed to spawn /bin/bash: {e}"
                )));
            }
        };

        match timeout(duration, child.wait_with_output()).await {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                let exit_code = output.status.code().unwrap_or(-1);
                Ok(ExecutionResult {
                    stdout,
                    stderr,
                    exit_code,
                    timed_out: false,
                })
            }
            Ok(Err(e)) => Err(ToolError::Execution(format!(
                "Failed reading command output: {e}"
            ))),
            Err(_) => Ok(ExecutionResult::timeout("", "Command execution timed out")),
        }
    }

    async fn execute_session(
        &self,
        session_id: &str,
        cmd_str: &str,
        duration: Duration,
    ) -> Result<ExecutionResult, ToolError> {
        let mut sessions = self.sessions.lock().await;

        if !sessions.contains_key(session_id) {
            let mut cmd = Command::new("/bin/bash");
            cmd.arg("--noprofile")
                .arg("--norc")
                .current_dir(&self.workspace_root)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());

            let mut child = cmd
                .spawn()
                .map_err(|e| ToolError::Execution(format!("Failed to start persistent shell: {e}")))?;

            let stdin = child.stdin.take().ok_or_else(|| {
                ToolError::Execution("Failed to acquire persistent shell stdin".into())
            })?;
            let stdout = child.stdout.take().ok_or_else(|| {
                ToolError::Execution("Failed to acquire persistent shell stdout".into())
            })?;
            let reader = BufReader::new(stdout);

            sessions.insert(
                session_id.to_string(),
                PersistentSession {
                    child,
                    stdin,
                    reader,
                },
            );
        }

        let session = sessions.get_mut(session_id).unwrap();
        let sentinel = format!("__KOBOLD_SENTINEL_{}__", std::process::id());

        // Send command and echo sentinel with return code
        let payload = format!(
            "{cmd_str}\necho \"{sentinel}:$?\"\n",
        );

        if let Err(e) = session.stdin.write_all(payload.as_bytes()).await {
            sessions.remove(session_id);
            return Err(ToolError::Execution(format!(
                "Broken pipe in persistent shell: {e}"
            )));
        }
        let _ = session.stdin.flush().await;

        let mut output_lines = Vec::new();
        let mut exit_code = 0;

        let read_future = async {
            let mut line = String::new();
            while session.reader.read_line(&mut line).await.unwrap_or(0) > 0 {
                let trimmed = line.trim_end_matches(&['\r', '\n'][..]);
                if let Some(rest) = trimmed.strip_prefix(&format!("{sentinel}:")) {
                    exit_code = rest.parse::<i32>().unwrap_or(-1);
                    break;
                } else {
                    output_lines.push(line.clone());
                }
                line.clear();
            }
        };

        match timeout(duration, read_future).await {
            Ok(_) => Ok(ExecutionResult {
                stdout: output_lines.join(""),
                stderr: String::new(),
                exit_code,
                timed_out: false,
            }),
            Err(_) => {
                // Kill unresponsive persistent session
                let _ = session.child.kill().await;
                sessions.remove(session_id);
                Ok(ExecutionResult::timeout("", "Persistent shell timed out"))
            }
        }
    }
}

#[async_trait]
impl SandboxExecutor for HostProcessExecutor {
    async fn execute(
        &self,
        command: &str,
        session_id: Option<&str>,
        timeout_duration: Duration,
    ) -> Result<ExecutionResult, ToolError> {
        match session_id {
            Some(sid) => self.execute_session(sid, command, timeout_duration).await,
            None => self.execute_one_shot(command, timeout_duration).await,
        }
    }
}

/// MicroVM sandbox driver abstraction communicating with guest agent over vsock.
pub struct MicroVmExecutor {
    guest_cid: u32,
    guest_port: u32,
    vsock_path: Option<PathBuf>,
}

impl MicroVmExecutor {
    pub fn new(guest_cid: u32, guest_port: u32, vsock_path: Option<PathBuf>) -> Self {
        Self {
            guest_cid,
            guest_port,
            vsock_path,
        }
    }
}

#[async_trait]
impl SandboxExecutor for MicroVmExecutor {
    async fn execute(
        &self,
        command: &str,
        session_id: Option<&str>,
        _timeout_duration: Duration,
    ) -> Result<ExecutionResult, ToolError> {
        // Wire protocol frames to guest agent over vsock / UDS socket
        if let Some(ref path) = self.vsock_path {
            if !path.exists() {
                return Err(ToolError::Execution(format!(
                    "MicroVM vsock socket not available at '{}'",
                    path.display()
                )));
            }
        }

        // Stub / driver dispatch for guest agent length-prefixed JSON protocol
        let req = serde_json::json!({
            "type": "exec",
            "cid": self.guest_cid,
            "port": self.guest_port,
            "command": command,
            "session_id": session_id,
        });

        // When running in real microVM guest, this bridges through vsock socket
        Ok(ExecutionResult::success(format!(
            "[microvm execution stub: {}]",
            req
        )))
    }
}
