//! The provider adapter, as a process Kobold owns.
//!
//! Kobold used to call the OpenAI socket task directly. Now it starts a
//! separate program and talks to it in `kobold-proto` over its stdin and
//! stdout, so a provider implementation cannot reach Kobold's memory, its
//! terminal, or its tools -- and so a second provider is a second binary
//! rather than a second branch through this one.
//!
//! The interface deliberately matches what the in-process task offered: a
//! sender of `Command` and a receiver of `IncomingFrame`. The event loop does not
//! know it is talking to a process, which is what kept the switch to one
//! commit.

use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};

use kobold_proto::{codec, Command, IncomingFrame, Startup, Transport};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

#[derive(Debug)]
pub enum Error {
    /// The binary could not be found or could not be started. Distinct from
    /// an authentication failure, which happens later and inside the
    /// adapter: "I could not start the provider" and "the provider refused
    /// your key" are different problems with different fixes.
    Spawn {
        command: String,
        source: std::io::Error,
    },
    /// The startup frame could not be written, which in practice means the
    /// adapter died between spawning and reading.
    Handshake(std::io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Spawn { command, source } => {
                write!(f, "could not start the adapter {command:?}: {source}")
            }
            Error::Handshake(e) => write!(f, "the adapter closed before it was configured: {e}"),
        }
    }
}

impl std::error::Error for Error {}

/// The live adapter's process id, so the panic hook can kill it.
///
/// A static because a panic hook cannot borrow, and because the case this
/// covers is one where `Drop` does not run at all: the release profile sets
/// `panic = "abort"`, so an abort unwinds nothing and `kill_on_drop` never
/// fires. Combined with macOS having no `--die-with-parent`, a panicking
/// Kobold would otherwise leave an adapter holding a credential and a socket.
///
/// Zero means none running. Real pids are never zero.
static LIVE: AtomicU32 = AtomicU32::new(0);

/// Kill whatever adapter is running, from a context that cannot await.
///
/// For the panic hook. Best-effort by nature -- it sends a signal and does
/// not reap, because a panicking process has no runtime left to wait on.
pub fn kill_live() {
    let pid = LIVE.swap(0, Ordering::Relaxed);
    // Not merely an optimisation, and not optional: `kill(0, ...)` signals
    // *every process in the caller's process group*, which from a shell is
    // the whole job. A guard that lets zero through does not fail to kill
    // the adapter, it kills the terminal the user is sitting in.
    if pid == 0 {
        return;
    }
    #[cfg(unix)]
    kill_tree(pid);
}

/// The process groups that make up a spawned adapter, outermost first.
///
/// **One pid is not enough under a sandbox, and the reason is not obvious
/// from the argument list.** `bwrap --new-session` calls `setsid` for the
/// sandboxed process, so the adapter lands in a session *and process group of
/// its own*, keyed on the inner bwrap's pid -- not the pid Kobold spawned and
/// not that pid's group. Measured directly: outer bwrap in Kobold's group,
/// inner bwrap and the adapter in a group of the inner's own.
///
/// So signalling the outer pid's group reaches only the outer process, and
/// the adapter is left to `--die-with-parent`, which is a race Kobold wins
/// when idle and loses under load. Reading the one direct child closes it.
///
/// `--new-session` is kept rather than dropped, which would have made the
/// group uniform and this function unnecessary: it is a defence against
/// TIOCSTI injection through the terminal the adapter inherits on stderr.
/// This kernel disables that anyway (`dev.tty.legacy_tiocsti = 0`, checked),
/// but the flag is what makes it true on a kernel that does not.
#[cfg(target_os = "linux")]
pub fn sandbox_groups(pid: u32) -> Vec<i32> {
    let mut groups = vec![pid as i32];
    // The kernel lists a process's direct children here, space-separated.
    // Cheaper and far more precise than scanning all of /proc, and there is
    // exactly one child to find: bwrap's inner monitor.
    let path = format!("/proc/{pid}/task/{pid}/children");
    if let Ok(children) = std::fs::read_to_string(path) {
        groups.extend(
            children
                .split_ascii_whitespace()
                .filter_map(|c| c.parse::<i32>().ok()),
        );
    }
    groups
}

/// Signal every process in an adapter's tree.
///
/// Both the groups and the bare pids, because the two cases differ: with a
/// sandbox the adapter is in the inner group, and without one there is a
/// single process whose group may hold nothing else.
#[cfg(unix)]
fn kill_tree(pid: u32) {
    #[cfg(target_os = "linux")]
    let members = sandbox_groups(pid);
    #[cfg(not(target_os = "linux"))]
    let members = vec![pid as i32];

    for m in members {
        // SAFETY: pids this process spawned or their direct children, and
        // never zero -- `kill(0)` and `kill(-0)` both name the caller's own
        // group, which from a shell is the user's whole job. `spawn` puts the
        // child in a group of its own so the negation cannot reach Kobold.
        if m <= 0 {
            continue;
        }
        unsafe {
            libc::kill(-m, libc::SIGKILL);
            libc::kill(m, libc::SIGKILL);
        }
    }
}

/// A running adapter. Dropping it kills the process.
pub struct Adapter {
    child: tokio::process::Child,
    /// Held for the adapter's lifetime rather than used: dropping it stops
    /// the broker and removes the socket, so an adapter's egress dies exactly
    /// when the adapter does.
    _egress: Option<crate::broker::Broker>,
    /// Same: dropping it removes the directory the socket lived in.
    _hutch: Option<crate::broker::Hutch>,
}

impl Adapter {
    /// Start `command`, hand it the startup frame, and wire its stdio to a
    /// pair of channels.
    ///
    /// stderr is inherited rather than piped, so an adapter's diagnostics
    /// land in the terminal the user is already looking at instead of being
    /// swallowed -- the same choice the MCP transport makes, and for the same
    /// reason.
    /// `allow` is the set of hosts this adapter may reach, and it is
    /// **Kobold's policy, never the adapter's** -- passed in here rather than
    /// read from anything the adapter controls.
    ///
    /// An **empty allowlist is a broker that refuses everything**, not an
    /// absent one. That direction matters: a caller who forgets to grant a
    /// host gets refused connections, where the other default would silently
    /// hand a community adapter the open internet.
    pub async fn spawn(
        command: &str,
        args: &[String],
        startup: &Startup,
        allow: &[String],
    ) -> Result<
        (
            Self,
            mpsc::UnboundedSender<Command>,
            mpsc::Receiver<IncomingFrame>,
        ),
        Error,
    > {
        Self::spawn_inner(command, args, startup, allow, false).await
    }

    /// Spawns an adapter unconfined by sandbox or broker.
    ///
    /// Used for meta-harness adapters (e.g. `kobold-adapter-tmux`) that require
    /// direct access to POSIX PTYs and host subprocesses.
    pub async fn spawn_unconfined(
        command: &str,
        args: &[String],
        startup: &Startup,
    ) -> Result<
        (
            Self,
            mpsc::UnboundedSender<Command>,
            mpsc::Receiver<IncomingFrame>,
        ),
        Error,
    > {
        Self::spawn_inner(command, args, startup, &[], true).await
    }

    async fn spawn_inner(
        command: &str,
        args: &[String],
        startup: &Startup,
        allow: &[String],
        unconfined: bool,
    ) -> Result<
        (
            Self,
            mpsc::UnboundedSender<Command>,
            mpsc::Receiver<IncomingFrame>,
        ),
        Error,
    > {
        // Resolved in the parent, before any wrapper is chosen, and the
        // wrapped case is what makes it necessary. `bwrap` exists, so
        // spawning it succeeds however broken the adapter behind it is, and
        // a missing adapter then fails after the fork -- where the parent
        // sees a child that exited, not a start that failed. Resolving here
        // keeps "could not start the adapter" a spawn error naming the
        // adapter, confined or not.
        let Some(exe) = crate::sandbox::resolve(command) else {
            return Err(Error::Spawn {
                command: command.to_owned(),
                source: std::io::Error::new(std::io::ErrorKind::NotFound, "not found on PATH"),
            });
        };

        // The absolute path, not the name as typed: the wrapper gives the
        // child a different filesystem view and a different working
        // directory, so a bare name or a relative path would be looked up
        // against the wrong one.
        let argv: Vec<String> = std::iter::once(exe.to_string_lossy().into_owned())
            .chain(args.iter().cloned())
            .collect();

        let (mut cmd, egress, hutch, broker) = if unconfined {
            let mut c = tokio::process::Command::new(&exe);
            c.args(args);
            (c, None, None, None)
        } else {
            let h = crate::broker::hutch().map_err(|source| Error::Spawn {
                command: command.to_owned(),
                source,
            })?;
            let b =
                crate::broker::start(allow.to_vec(), h.path()).map_err(|source| Error::Spawn {
                    command: command.to_owned(),
                    source,
                })?;
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let policy = crate::sandbox::Policy {
                allow_network: false,
                writable_root: false,
                share_workdir: false,
            };
            let c = match crate::sandbox::wrap_with_socket(policy, &cwd, &argv, b.path()) {
                Some(wrapped) => wrapped,
                None => {
                    let mut uc = tokio::process::Command::new(&exe);
                    uc.args(args);
                    uc
                }
            };
            let eg = Some(b.path().to_string_lossy().into_owned());
            (c, eg, Some(h), Some(b))
        };

        let startup = Startup {
            egress,
            ..startup.clone()
        };
        let startup = &startup;

        // Before anything else is added, and the reason is not hygiene: the
        // credential travels on stdin precisely so it is not in an
        // environment that `/proc/PID/environ` exposes, and inheriting
        // `LLM_API_KEY` here would put it straight back.
        crate::childenv::restrict(&mut cmd);
        // Its own process group, so killing it can name the group and reach
        // everything the wrapper forked -- and so that negating the pid can
        // never name Kobold's own group, which from a shell would be the
        // user's whole job.
        #[cfg(unix)]
        cmd.process_group(0);
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .map_err(|source| Error::Spawn {
                command: command.to_owned(),
                source,
            })?;

        let mut stdin = child.stdin.take().expect("piped stdin was requested");
        let stdout = child.stdout.take().expect("piped stdout was requested");

        // Written before either pump starts, so the ordering the protocol
        // requires is the ordering the code performs rather than a rule.
        let frame = codec::encode(startup).expect("the startup frame is plain data");
        stdin
            .write_all(frame.as_bytes())
            .await
            .map_err(Error::Handshake)?;
        stdin.flush().await.map_err(Error::Handshake)?;

        // Bounded, unlike the command side. An adapter can produce updates
        // far faster than a terminal can show them -- a flood, or simply a
        // fast turn against a slow tty -- and an unbounded queue turns that
        // into unbounded memory in Kobold rather than backpressure on the
        // adapter. The bound is large enough that an ordinary turn never
        // touches it: deltas arrive 4-48ms apart and the loop drains what is
        // queued before each paint, so 256 is roughly two seconds of the
        // fastest streaming ever measured.
        //
        // The command side stays unbounded because it is driven by
        // keystrokes, and a bounded send there would mean the UI awaiting an
        // adapter that has stopped reading.
        let (update_tx, update_rx) = mpsc::channel::<IncomingFrame>(256);
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<Command>();

        // Commands out.
        tokio::spawn(async move {
            while let Some(command) = cmd_rx.recv().await {
                let Ok(line) = codec::encode(&command) else {
                    continue;
                };
                if stdin.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
                if stdin.flush().await.is_err() {
                    break;
                }
            }
        });

        // Updates in, until the adapter's stdout closes -- which is what its
        // death looks like from here, however it died.
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) if line.trim().is_empty() => continue,
                    Ok(Some(line)) => match codec::decode::<IncomingFrame>(&line) {
                        Ok(frame) => {
                            // Awaits when the queue is full, which is the
                            // backpressure: the adapter's stdout stops being
                            // read, its own write blocks, and it slows to the
                            // rate Kobold can consume.
                            if update_tx.send(frame).await.is_err() {
                                break;
                            }
                        }
                        // A line that is not protocol means the adapter is
                        // writing something else to stdout, most likely a
                        // diagnostic that belongs on stderr. Reported rather
                        // than skipped: silently dropping it would leave a
                        // turn hanging with no reason given.
                        Err(e) => {
                            let _ = update_tx
                                .send(IncomingFrame::Transport(Transport::Disconnected(
                                    e.to_string(),
                                )))
                                .await;
                            break;
                        }
                    },
                    Ok(None) => {
                        // Clean EOF. The adapter is gone; whether it meant to
                        // be is commit 4's problem, but the user finds out
                        // either way rather than watching a turn never end.
                        let _ = update_tx
                            .send(IncomingFrame::Transport(Transport::Disconnected(
                                "the adapter exited".to_owned(),
                            )))
                            .await;
                        break;
                    }
                    Err(e) => {
                        let _ = update_tx
                            .send(IncomingFrame::Transport(Transport::Disconnected(
                                e.to_string(),
                            )))
                            .await;
                        break;
                    }
                }
            }
        });

        if let Some(pid) = child.id() {
            LIVE.store(pid, Ordering::Relaxed);
        }
        Ok((
            Self {
                child,
                _egress: broker,
                _hutch: hutch,
            },
            cmd_tx,
            update_rx,
        ))
    }

    /// The adapter's process id while it is running.
    pub fn id(&self) -> Option<u32> {
        self.child.id()
    }

    /// Kill the adapter and reap it.
    ///
    /// Explicit rather than left to `kill_on_drop`, because **macOS has no
    /// `--die-with-parent` equivalent** -- verified on the user's machine,
    /// adapters orphan -- and `kill_on_drop` only covers the path where
    /// Kobold's own destructors run. The case that matters is Kobold being
    /// killed outright, which no in-process mechanism can cover; that is
    /// commit 4's problem and this is the half that is available now.
    pub async fn shutdown(&mut self) {
        LIVE.store(0, Ordering::Relaxed);
        // The whole tree, for the same reason `kill_live` does it: under a
        // sandbox the pid is `bwrap`'s, and the adapter is in a session of
        // its own that neither that pid nor its group names.
        //
        // **This was fixed in `kill_live` first and missed here**, which is
        // what a second copy of a mechanism costs -- and the panic hook got
        // the attention because it is the frightening one, while this is the
        // path that runs every single time.
        #[cfg(unix)]
        if let Some(pid) = self.child.id() {
            kill_tree(pid);
        }
        let _ = self.child.start_kill();
        let _ = self.child.wait().await;
    }
}

impl Drop for Adapter {
    /// Take the sandbox tree with it, not just the process Kobold spawned.
    ///
    /// **`kill_on_drop` is not enough and the gap is the same one `shutdown`
    /// had.** It kills the `Child`, which under a sandbox is `bwrap` -- while
    /// the adapter lives in a session of its own, keyed on the inner bwrap,
    /// that neither that pid nor its group names. So every path that drops an
    /// `Adapter` without calling `shutdown` left the adapter running, holding
    /// the credential and its socket.
    ///
    /// Found from a test, but it is not a test problem: a panic unwinds, this
    /// runs, and the tree goes. Without it a failing test leaves behind
    /// exactly the wedged adapter it was testing against, which then fails the
    /// *next* run -- one poisoned run poisoning every run after it, with the
    /// symptom migrating away from the cause. In production the same gap is an
    /// adapter that outlives the UI.
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.child.id() {
            kill_tree(pid);
        }
    }
}
