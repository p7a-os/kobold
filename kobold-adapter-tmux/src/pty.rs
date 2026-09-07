//! Pseudo-terminal (PTY) and tmux process supervisor.

use std::fs::File;
use std::io::{self, Read as _, Write as _};
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command as StdCommand, Stdio};
use std::time::Duration;

use tokio::sync::mpsc;

/// Opens a POSIX pseudo-terminal (PTY) master and slave pair with specified dimensions.
#[cfg(unix)]
pub fn open_pty(rows: u16, cols: u16) -> io::Result<(RawFd, RawFd)> {
    let mut master: libc::c_int = -1;
    let mut slave: libc::c_int = -1;
    let mut win = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };

    #[allow(clippy::unnecessary_mut_passed)]
    let ret = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut win,
        )
    };

    if ret < 0 {
        return Err(io::Error::last_os_error());
    }

    Ok((master, slave))
}

/// A running session hosted either in an in-process PTY or via external tmux.
pub struct PtySession {
    master: File,
    child: Option<Child>,
    rx: mpsc::Receiver<Vec<u8>>,
    tmux_session: Option<String>,
}

impl PtySession {
    /// Spawns an in-process interactive program inside a POSIX PTY.
    pub fn spawn_in_pty(program: &str, args: &[String]) -> io::Result<Self> {
        let (master_fd, slave_fd) = open_pty(40, 120)?;

        let slave_in = unsafe { File::from_raw_fd(slave_fd) };
        let slave_out = slave_in.try_clone()?;
        let slave_err = slave_in.try_clone()?;

        let mut cmd = StdCommand::new(program);
        cmd.args(args);
        cmd.stdin(Stdio::from(slave_in));
        cmd.stdout(Stdio::from(slave_out));
        cmd.stderr(Stdio::from(slave_err));

        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                libc::ioctl(0, libc::TIOCSCTTY as _, 1);
                Ok(())
            });
        }

        let child = cmd.spawn()?;
        let master = unsafe { File::from_raw_fd(master_fd) };
        let mut master_reader = master.try_clone()?;

        let (tx, rx) = mpsc::channel::<Vec<u8>>(128);

        // Dedicated OS thread reading master PTY stream
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match master_reader.read(&mut buf) {
                    Ok(0) => break, // EOF
                    Ok(n) => {
                        if tx.blocking_send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(Self {
            master,
            child: Some(child),
            rx,
            tmux_session: None,
        })
    }

    /// Spawns a command wrapped inside an external tmux session.
    pub fn spawn_in_tmux(session_name: &str, program: &str, args: &[String]) -> io::Result<Self> {
        let full_cmd = format!("{} {}", program, args.join(" "));
        let status = StdCommand::new("tmux")
            .args([
                "new-session",
                "-d",
                "-s",
                session_name,
                "-x",
                "120",
                "-y",
                "40",
                &full_cmd,
            ])
            .status()?;

        if !status.success() {
            return Err(io::Error::other(format!(
                "failed to spawn tmux session '{session_name}'"
            )));
        }

        let (master_fd, _) = open_pty(40, 120)?;
        let master = unsafe { File::from_raw_fd(master_fd) };
        let (tx, rx) = mpsc::channel::<Vec<u8>>(128);

        let sess = session_name.to_string();
        std::thread::spawn(move || {
            let mut last_captured = String::new();
            loop {
                std::thread::sleep(Duration::from_millis(50));
                let output = StdCommand::new("tmux")
                    .args(["capture-pane", "-p", "-t", &sess])
                    .output();
                if let Ok(out) = output {
                    if out.status.success() {
                        let text = String::from_utf8_lossy(&out.stdout).to_string();
                        if text != last_captured {
                            let diff = if text.starts_with(&last_captured) {
                                text.as_bytes()[last_captured.len()..].to_vec()
                            } else {
                                text.as_bytes().to_vec()
                            };
                            last_captured = text;
                            if !diff.is_empty() && tx.blocking_send(diff).is_err() {
                                break;
                            }
                        }
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            }
        });

        Ok(Self {
            master,
            child: None,
            rx,
            tmux_session: Some(session_name.to_string()),
        })
    }

    /// Writes keystrokes into the active terminal session.
    pub fn write_all(&mut self, text: &str) -> io::Result<()> {
        if let Some(sess) = &self.tmux_session {
            let status = StdCommand::new("tmux")
                .args(["send-keys", "-t", sess, text])
                .status()?;
            if !status.success() {
                return Err(io::Error::other("tmux send-keys failed"));
            }
            Ok(())
        } else {
            self.master.write_all(text.as_bytes())?;
            self.master.flush()?;
            Ok(())
        }
    }

    /// Reads output bytes from the PTY asynchronously. Returns `None` on EOF.
    pub async fn read_bytes(&mut self) -> Option<Vec<u8>> {
        self.rx.recv().await
    }

    /// Terminates the child process and cleans up any tmux session.
    pub fn terminate(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(sess) = self.tmux_session.take() {
            let _ = StdCommand::new("tmux")
                .args(["kill-session", "-t", &sess])
                .output();
        }
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        self.terminate();
    }
}
