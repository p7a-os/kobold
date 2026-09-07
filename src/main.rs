//! kobold — agentic client for the Responses WebSocket API.

use crossterm::event::{Event as TermEvent, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures_util::StreamExt;
use kobold::app::{App, Effect, Mode, Status, Who};
use kobold::daemon::{default_socket_path, DaemonClient};
use kobold::net::{self, Command, Incoming, IncomingFrame, Startup};
use kobold::{osc, progress, sandbox, settings, tools, transcript, tts};
use kobold_proto::agui;
use kobold_proto::northbound::{ClientFrame, ClientServerFrame, LaneStatus};
use std::sync::atomic::{AtomicU32, Ordering};
use tokio::sync::mpsc;

// Opt-in: see the `fast-alloc` feature in Cargo.toml for the measurement and
// why it is not the default.
#[cfg(feature = "fast-alloc")]
#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

const LANE: &str = "main";

/// The adapter shipped with Kobold. Overridable in settings, because the
/// point of the split is that this is one implementation rather than the
/// only one.
const DEFAULT_ADAPTER: &str = "kobold-openai";

static LIVE_DAEMON: AtomicU32 = AtomicU32::new(0);

fn find_koboldd_binary() -> std::path::PathBuf {
    let mut roots: Vec<std::path::PathBuf> = vec![std::path::PathBuf::from(".")];
    if let Ok(exe) = std::env::current_exe() {
        roots.extend(exe.ancestors().skip(1).take(4).map(|p| p.to_path_buf()));
    }
    roots
        .iter()
        .flat_map(|r| {
            [
                r.join("koboldd"),
                r.join("target/debug/koboldd"),
                r.join("target/release/koboldd"),
            ]
        })
        .find(|p| p.is_file())
        .unwrap_or_else(|| std::path::PathBuf::from("koboldd"))
}

fn print_session_list() {
    let sessions = kobold::session::SessionRegistry::list();
    if sessions.is_empty() {
        println!(
            "kobold: no active sessions found in {}",
            kobold::session::session_runtime_dir().display()
        );
        return;
    }
    println!(
        "{:<38} {:<8} {:<16} {:<12} WORKDIR",
        "SESSION ID", "PID", "ADAPTER", "AGE"
    );
    for s in sessions {
        println!(
            "{:<38} {:<8} {:<16} {:<12} {}",
            s.session_id,
            s.pid,
            s.adapter,
            s.age_display(),
            s.workdir.display()
        );
    }
}

fn kill_session(id: &str) -> Result<(), Box<dyn std::error::Error>> {
    if kobold::session::SessionRegistry::kill(id)? {
        println!("kobold: terminated session '{id}'");
        Ok(())
    } else {
        Err(format!("kobold: session '{id}' not found").into())
    }
}

fn prompt_for_worktree(path: &std::path::Path) -> bool {
    use std::io::Write as _;

    if let Ok(val) = std::env::var("KOBOLD_WORKTREE") {
        return val == "1" || val.eq_ignore_ascii_case("true") || val.eq_ignore_ascii_case("yes");
    }

    #[cfg(unix)]
    {
        if unsafe { libc::isatty(libc::STDIN_FILENO) } != 1 {
            return false;
        }
    }

    eprint!(
        "kobold: an active session is running in {}.\nWould you like to start kobold in a different git worktree instead? [Y/n] ",
        path.display()
    );
    let _ = std::io::stderr().flush();

    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_ok() {
        let trimmed = line.trim();
        trimmed.is_empty()
            || trimmed.eq_ignore_ascii_case("y")
            || trimmed.eq_ignore_ascii_case("yes")
    } else {
        false
    }
}

struct DaemonGuard {
    child: Option<std::process::Child>,
    socket: std::path::PathBuf,
    detached: bool,
}

impl DaemonGuard {
    fn disown(&mut self) {
        self.detached = true;
        self.child = None;
        LIVE_DAEMON.store(0, Ordering::Relaxed);
    }

    fn cleanup(&mut self) {
        if self.detached {
            return;
        }
        if let Some(mut child) = self.child.take() {
            LIVE_DAEMON.store(0, Ordering::Relaxed);
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_file(&self.socket);
        }
    }
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        self.cleanup();
    }
}

struct DaemonHandle {
    client: DaemonClient,
    guard: DaemonGuard,
}

impl DaemonHandle {
    fn into_parts(
        self,
    ) -> (
        mpsc::UnboundedSender<ClientFrame>,
        mpsc::Receiver<ClientServerFrame>,
        DaemonGuard,
    ) {
        let (tx, rx) = self.client.into_channels();
        (tx, rx, self.guard)
    }
}

fn print_help() {
    println!("Kobold — Autonomous Agent Harness & Streaming TUI Client\n");
    println!("Usage: kobold [OPTIONS] [SUBCOMMAND]\n");
    println!("Subcommands:");
    println!("  list, ls              List active daemon sessions");
    println!("  attach [SESSION_ID]   Attach to an existing daemon session");
    println!("  kill <SESSION_ID>     Terminate a running daemon session\n");
    println!("Options:");
    println!("  -p, --prompt <TEXT>   Execute a single turn to stdout and exit");
    println!("  -a, --adapter <CMD>   Agent adapter executable (default: 'kobold-openai')");
    println!("  -w, --workdir <PATH>  Working directory for session");
    println!("  -s, --socket <PATH>   Path to Unix Domain Socket");
    println!("      --session <ID>    Session identifier");
    println!("      --detach          Start daemon in background and exit without attaching");
    println!("      --ws-port <PORT>  Start Web Companion WebSocket server on specified port");
    println!("      --unconfined      Run adapter unconfined (without sandbox or egress broker)");
    println!("      --mock            Run with mock adapter channels for testing");
    println!("      --debug           Log tool calls and network diagnostics in transcript");
    println!("  -V, --version         Print version");
    println!("  -h, --help            Print help\n");
    println!("Examples:");
    println!("  kobold                                  # Interactive TUI with default adapter");
    println!("  kobold -p \"Explain ownership in Rust\"   # Run single turn to stdout");
    println!("  kobold --ws-port 3000                   # Launch with Web Companion on port 3000");
    println!("  kobold -a kobold-adapter-acp            # Run with Agent Client Protocol (ACP)");
    println!("  kobold list                             # List active sessions");
    println!("  kobold attach                           # Reattach to last session");
}

#[allow(clippy::too_many_arguments)]
async fn ensure_daemon(
    socket: &std::path::Path,
    session_id: &str,
    workdir: &std::path::Path,
    adapter_override: Option<&str>,
    mock_mode: bool,
    unconfined: bool,
    ws_port: Option<u16>,
) -> Result<DaemonHandle, Box<dyn std::error::Error>> {
    match DaemonClient::connect(socket).await {
        Ok(client) => Ok(DaemonHandle {
            client,
            guard: DaemonGuard {
                child: None,
                socket: socket.to_path_buf(),
                detached: true,
            },
        }),
        Err(_) => {
            let koboldd_bin = find_koboldd_binary();
            let mut cmd = std::process::Command::new(&koboldd_bin);
            cmd.arg("--socket")
                .arg(socket)
                .arg("--session")
                .arg(session_id)
                .arg("--workdir")
                .arg(workdir);
            if let Some(a) = adapter_override {
                cmd.arg("--adapter").arg(a);
            }
            if mock_mode {
                cmd.arg("--mock");
            }
            if unconfined {
                cmd.arg("--unconfined");
            }
            if let Some(port) = ws_port {
                cmd.arg("--ws-port").arg(port.to_string());
            }
            cmd.stdin(std::process::Stdio::null());
            cmd.stdout(std::process::Stdio::null());

            let log_path = socket.with_extension("log");
            if let Some(parent) = log_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(f) = std::fs::File::create(&log_path) {
                cmd.stderr(f);
            } else {
                cmd.stderr(std::process::Stdio::null());
            }

            let mut child = cmd.spawn().map_err(|e| {
                format!(
                    "failed to spawn koboldd from {}: {e}",
                    koboldd_bin.display()
                )
            })?;

            LIVE_DAEMON.store(child.id(), Ordering::Relaxed);

            let mut connected_client = None;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while std::time::Instant::now() < deadline {
                if let Ok(Some(status)) = child.try_wait() {
                    let err_msg = std::fs::read_to_string(&log_path).unwrap_or_default();
                    return Err(format!("koboldd exited prematurely ({status}): {err_msg}").into());
                }
                if socket.exists() {
                    if let Ok(c) = DaemonClient::connect(socket).await {
                        connected_client = Some(c);
                        break;
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }

            let client = match connected_client {
                Some(c) => c,
                None => {
                    let err_msg = std::fs::read_to_string(&log_path).unwrap_or_default();
                    let _ = child.kill();
                    return Err(format!(
                        "timed out connecting to koboldd at {}: {}",
                        socket.display(),
                        err_msg
                    )
                    .into());
                }
            };

            Ok(DaemonHandle {
                client,
                guard: DaemonGuard {
                    child: Some(child),
                    socket: socket.to_path_buf(),
                    detached: false,
                },
            })
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        kobold::term::restore();
        let pid = LIVE_DAEMON.swap(0, Ordering::Relaxed);
        if pid != 0 {
            #[cfg(unix)]
            unsafe {
                libc::kill(pid as i32, libc::SIGKILL);
            }
        }
        kobold::adapter::kill_live();
        hook(info);
    }));

    #[cfg(unix)]
    {
        tokio::spawn(async {
            use tokio::signal::unix::{signal, SignalKind};
            if let (Ok(mut sigint), Ok(mut sigterm)) = (
                signal(SignalKind::interrupt()),
                signal(SignalKind::terminate()),
            ) {
                tokio::select! {
                    _ = sigint.recv() => {},
                    _ = sigterm.recv() => {},
                }
                kobold::term::restore();
                let pid = LIVE_DAEMON.swap(0, Ordering::Relaxed);
                if pid != 0 {
                    unsafe {
                        libc::kill(pid as i32, libc::SIGKILL);
                    }
                }
                kobold::adapter::kill_live();
                std::process::exit(130);
            }
        });
    }

    let argv: Vec<String> = std::env::args().skip(1).collect();

    if argv
        .iter()
        .any(|a| a == "--version" || a == "-V" || a == "version")
    {
        println!("kobold {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    if argv
        .iter()
        .any(|a| a == "--help" || a == "-h" || a == "help")
    {
        print_help();
        return Ok(());
    }

    if let Some(first) = argv.first() {
        match first.as_str() {
            "list" | "ls" | "--list" => {
                print_session_list();
                return Ok(());
            }
            "kill" => {
                let id = argv.get(1).ok_or("usage: kobold kill <SESSION_ID>")?;
                return kill_session(id);
            }
            "doctor" => {
                let workdir =
                    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                return kobold::doctor::run_doctor(&workdir).await;
            }
            _ => {}
        }
    }

    let debug = argv.iter().any(|a| a == "--debug");

    let mut socket_path: Option<std::path::PathBuf> = None;
    let mut session_id_arg: Option<String> = None;
    let mut adapter_override: Option<String> = None;
    let mut mock_mode = false;
    let mut unconfined = false;
    let mut ws_port: Option<u16> = None;
    let mut workdir: Option<std::path::PathBuf> = None;
    let mut prompt_arg: Option<String> = None;
    let mut detach_only = false;
    let mut attach_mode = false;
    let mut attach_target: Option<String> = None;

    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--help" | "-h" => {
                print_help();
                return Ok(());
            }
            "attach" | "--attach" => {
                attach_mode = true;
                if i + 1 < argv.len() && !argv[i + 1].starts_with('-') {
                    attach_target = Some(argv[i + 1].clone());
                    i += 2;
                } else {
                    i += 1;
                }
            }
            "--detach" => {
                detach_only = true;
                i += 1;
            }
            "--unconfined" => {
                unconfined = true;
                i += 1;
            }
            "--ws-port" if i + 1 < argv.len() => {
                if let Ok(p) = argv[i + 1].parse::<u16>() {
                    ws_port = Some(p);
                    i += 2;
                } else {
                    i += 1;
                }
            }
            "-p" | "--prompt" => {
                let rest = argv[i + 1..].join(" ");
                let prompt = if rest.trim().is_empty() {
                    let mut buf = String::new();
                    std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)?;
                    buf
                } else {
                    rest
                };
                prompt_arg = Some(prompt);
                break;
            }
            "-s" | "--socket" if i + 1 < argv.len() => {
                socket_path = Some(std::path::PathBuf::from(&argv[i + 1]));
                i += 2;
            }
            "--session" if i + 1 < argv.len() => {
                session_id_arg = Some(argv[i + 1].clone());
                i += 2;
            }
            "-a" | "--adapter" if i + 1 < argv.len() => {
                adapter_override = Some(argv[i + 1].clone());
                i += 2;
            }
            "-w" | "--workdir" if i + 1 < argv.len() => {
                workdir = Some(std::path::PathBuf::from(&argv[i + 1]));
                i += 2;
            }
            "--mock" => {
                mock_mode = true;
                i += 1;
            }
            _ => {
                i += 1;
            }
        }
    }

    let mut root = workdir.unwrap_or_else(|| {
        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
    });

    if !attach_mode {
        let existing = kobold::session::SessionRegistry::find_for_workdir(&root);
        if !existing.is_empty() {
            eprintln!(
                "kobold: warning: active session '{}' (pid {}) is already running in {}",
                existing[0].session_id,
                existing[0].pid,
                root.display()
            );
            if kobold::worktree::is_git_repo(&root) {
                if let Some(repo_root) = kobold::worktree::find_git_root(&root) {
                    if prompt_for_worktree(&root) {
                        let wt_name = kobold::worktree::generate_worktree_name();
                        match kobold::worktree::create_git_worktree(&repo_root, &wt_name) {
                            Ok(new_path) => {
                                eprintln!(
                                    "kobold: created new git worktree at {}",
                                    new_path.display()
                                );
                                eprintln!("kobold: switching session to worktree: {wt_name}");
                                root = new_path;
                                if session_id_arg.is_none() {
                                    session_id_arg = Some(wt_name);
                                }
                            }
                            Err(e) => {
                                eprintln!("kobold: warning: failed to create worktree ({e}); continuing in current directory");
                            }
                        }
                    }
                }
            }
        }
    }

    let (session_id, socket) = if attach_mode {
        let meta = if let Some(target) = attach_target {
            match kobold::session::SessionRegistry::find_by_id(&target) {
                Some(m) => m,
                None => return Err(format!("kobold: session '{target}' not found").into()),
            }
        } else {
            let matches = kobold::session::SessionRegistry::find_for_workdir(&root);
            if matches.is_empty() {
                return Err(format!(
                    "kobold: no active session found for workdir '{}'. Start one with: kobold",
                    root.display()
                )
                .into());
            }
            if matches.len() > 1 {
                eprintln!(
                    "kobold: multiple sessions active for this workdir; attaching to latest ({})",
                    matches[0].session_id
                );
            }
            matches.into_iter().next().unwrap()
        };
        (meta.session_id, meta.socket_path)
    } else {
        let session_id = session_id_arg.unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
        let socket = socket_path.unwrap_or_else(|| default_socket_path(&session_id));
        (session_id, socket)
    };

    if detach_only {
        let handle = ensure_daemon(
            &socket,
            &session_id,
            &root,
            adapter_override.as_deref(),
            mock_mode,
            unconfined,
            ws_port,
        )
        .await?;
        let (_, _, mut guard) = handle.into_parts();
        guard.disown();
        println!("kobold: started detached session {session_id}");
        println!("kobold: socket at {}", socket.display());
        println!("kobold: re-attach with: kobold attach {session_id}");
        return Ok(());
    }

    if let Some(prompt) = prompt_arg {
        return oneshot(
            prompt.trim(),
            &socket,
            &session_id,
            &root,
            adapter_override.as_deref(),
            mock_mode,
            unconfined,
            ws_port,
        )
        .await;
    }

    let kobold_dir = root.join(kobold::transcript::DIR);
    if !kobold_dir.exists() {
        kobold::wizard::run_onboarding_wizard(&root).await?;
    } else {
        let _ = settings::Settings::ensure_file(&root);
    }
    let (mut cfg, cfg_note) = settings::Settings::load(&root);

    // If no default agent or current harness is configured, prompt or auto-select from enabled ones
    if cfg.default_agent.is_none() && cfg.current_harness.is_none() {
        let mut enabled_harnesses: Vec<&str> = Vec::new();
        for (name, agent) in &cfg.agents {
            if agent.enabled {
                if let Some(norm) = kobold::catalog::normalize_harness(name) {
                    if !enabled_harnesses.contains(&norm) {
                        enabled_harnesses.push(norm);
                    }
                }
            }
        }
        for (name, prov) in &cfg.providers {
            if prov.enabled {
                if let Some(norm) = kobold::catalog::normalize_harness(name) {
                    if !enabled_harnesses.contains(&norm) {
                        enabled_harnesses.push(norm);
                    }
                }
            }
        }
        if !enabled_harnesses.is_empty() {
            if cfg.models_cache.is_empty() {
                cfg.models_cache = kobold::catalog::build_models_cache(&enabled_harnesses);
            }
            let chosen = if enabled_harnesses.len() == 1 {
                enabled_harnesses[0].to_string()
            } else {
                kobold::wizard::prompt_default_harness(&enabled_harnesses, &cfg.models_cache)
                    .unwrap_or_else(|_| enabled_harnesses[0].to_string())
            };
            let def_model = kobold::catalog::default_model_for_harness(&chosen);
            let _ = cfg.update_harness_and_model(&root, &chosen, def_model);
        }
    }

    let handle = ensure_daemon(
        &socket,
        &session_id,
        &root,
        adapter_override.as_deref(),
        mock_mode,
        unconfined,
        ws_port,
    )
    .await?;

    let mut log = transcript::Log::open(&root);
    let mut app = App::new(LANE, &session_id);
    app.root = Some(root.clone());
    app.history = transcript::user_history(&root);

    if sandbox::available().is_none() {
        app.push(
            Who::System,
            "no sandbox available — tools and the adapter run unconfined, \
             and the adapter's egress allowlist is not enforced",
        );
    }

    let synth: Vec<String> = if cfg.voice.command.trim().is_empty() {
        bundled_sidecar().into_iter().collect()
    } else {
        cfg.voice
            .command
            .split_whitespace()
            .map(str::to_owned)
            .collect()
    };

    let (notice_tx, mut notice_rx) = mpsc::unbounded_channel::<String>();
    let audio_addr = (!cfg.voice.audio_addr.is_empty()).then(|| cfg.voice.audio_addr.clone());
    app.voice_remote = audio_addr.is_some();
    if !cfg.voice.voice.is_empty() {
        std::env::set_var("KOBOLD_TTS_VOICE", &cfg.voice.voice);
    }
    app.voice_engine = !synth.is_empty();
    app.voice = cfg.voice.enabled;
    app.debug = debug;
    app.model = cfg.model.clone();
    app.harness = cfg.active_harness().to_string();
    app.effort = cfg.reasoning_effort.clone();
    app.context_window = cfg.context_window;
    app.code_bg = cfg.code_bg;
    if let Some(note) = cfg_note {
        app.push(Who::System, note);
    }

    let is_external = adapter_override
        .as_deref()
        .map(|a| a.contains("acp") || a.contains("tmux") || a.contains("pty"))
        .unwrap_or_else(|| {
            cfg.adapter.contains("acp")
                || cfg.adapter.contains("tmux")
                || cfg.adapter.contains("pty")
                || cfg.agents.values().any(|a| a.enabled)
        });
    if !mock_mode && !is_external {
        let has_key = std::env::var("LLM_API_KEY")
            .or_else(|_| std::env::var("OPENAI_API_KEY"))
            .map(|k| !k.trim().is_empty())
            .unwrap_or(false);
        if !has_key {
            app.push(
                Who::System,
                "Notice: No LLM API key detected in environment. Set LLM_API_KEY or OPENAI_API_KEY, or launch with an external agent: kobold -a kobold-adapter-acp",
            );
        }
    }

    let mut speaker: Option<Box<dyn tts::StreamingTts>> = if synth.is_empty() {
        None
    } else if let Some(addr) = audio_addr {
        Some(Box::new(tts::SocketTts::spawn(synth, addr)))
    } else {
        let (program, args) = synth.split_first().expect("checked non-empty");
        let resident = std::env::var_os("KOBOLD_TTS_RESIDENT").is_some()
            || program.ends_with("pocket-say")
            || program.ends_with("kobold-tts");
        if resident {
            Some(Box::new(tts::CommandTts::spawn_resident(
                program.clone(),
                args.to_vec(),
                notice_tx.clone(),
            )))
        } else {
            Some(Box::new(tts::CommandTts::spawn(
                program.clone(),
                args.to_vec(),
            )))
        }
    };

    if cfg.voice.enabled {
        if let Some(s) = speaker.as_mut() {
            s.warm();
        }
    }

    osc::title(&format!("kobold — {}", cfg.model));

    let mut screen = kobold::term::Screen::init()?;
    kobold::term::enable_key_disambiguation();
    let mut keys = EventStream::new();

    let (client_tx, mut client_rx, mut daemon_guard) = handle.into_parts();

    let result = run_client(
        &mut screen,
        &mut app,
        &client_tx,
        &mut client_rx,
        &mut keys,
        &mut log,
        &mut speaker,
        &mut notice_rx,
    )
    .await;

    kobold::term::restore();

    let detached = result.as_ref().copied().unwrap_or(false);
    if detached {
        daemon_guard.disown();
        eprintln!("kobold: detached from session {session_id}. Re-attach with: kobold attach {session_id}");
    } else {
        daemon_guard.cleanup();
    }

    if std::env::var_os("KOBOLD_STATS").is_some() {
        eprintln!("kobold: {}", kobold::term::stats());
        eprintln!("kobold: {}", kobold::layout::stats());
    }
    result.map(|_| ())
}

#[allow(clippy::too_many_arguments)]
async fn run_client<B, K>(
    screen: &mut kobold::term::Screen<B>,
    app: &mut App,
    client_tx: &mpsc::UnboundedSender<ClientFrame>,
    client_rx: &mut mpsc::Receiver<ClientServerFrame>,
    keys: &mut K,
    log: &mut transcript::Log,
    speaker: &mut Option<Box<dyn tts::StreamingTts>>,
    notices: &mut mpsc::UnboundedReceiver<String>,
) -> Result<bool, Box<dyn std::error::Error>>
where
    B: ratatui::backend::Backend,
    B::Error: std::error::Error + Send + Sync + 'static,
    K: futures_util::Stream<Item = std::io::Result<TermEvent>> + Unpin,
{
    screen.draw(|buf, area| app.render(buf, area))?;
    let _guard = progress::Guard;
    let mut reported = progress::State::Clear;
    let mut done_at: Option<std::time::Instant> = None;
    const DONE_FLASH: std::time::Duration = std::time::Duration::from_millis(700);

    let mut tick = tokio::time::interval(std::time::Duration::from_millis(110));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    const FRAME: std::time::Duration = std::time::Duration::from_millis(8);
    let mut pending = false;
    let mut last_draw = std::time::Instant::now();

    loop {
        let dirty = tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                kobold::term::restore();
                app.should_quit = true;
                break;
            },
            frame = client_rx.recv() => match frame {
                Some(f) => {
                    absorb_client_server_frame(app, client_tx, log, f).await;
                    true
                }
                None => return Err(disconnect_reason(app).into()),
            },
            key = keys.next() => match key {
                Some(Ok(ev)) => handle_client_key(app, client_tx, log, speaker, ev),
                Some(Err(e)) => return Err(e.into()),
                None => break,
            },
            notice = notices.recv() => match notice {
                Some(line) => { app.set_notice(line); true }
                None => false,
            },
            _ = tokio::time::sleep(FRAME.saturating_sub(last_draw.elapsed())), if pending => false,
            _ = tick.tick() => {
                if app.panes.iter().any(|p| p.status() == Status::Waiting)
                    || app.notice_live()
                    || done_at.is_some()
                {
                    app.spinner = app.spinner.wrapping_add(1);
                    true
                } else {
                    false
                }
            }
        };

        let busy = app.panes.iter().any(|p| p.status() == Status::Waiting);
        let failed = app.any_pane_failed();
        if busy {
            done_at = None;
        } else if reported == progress::State::Indeterminate {
            done_at = Some(std::time::Instant::now());
        }
        let want = if failed {
            progress::State::Error
        } else if busy {
            progress::State::Indeterminate
        } else if done_at.is_some_and(|t| t.elapsed() < DONE_FLASH) {
            progress::State::At(100)
        } else {
            done_at = None;
            progress::State::Clear
        };
        if want != reported {
            progress::set(want);
            reported = want;
        }

        if let Some(s) = speaker.as_mut() {
            if std::mem::take(&mut app.speech_cancel) {
                s.cancel();
            }
            for chunk in app.speech.drain(..) {
                s.enqueue(chunk);
            }
        } else {
            app.speech.clear();
            app.speech_cancel = false;
        }

        if app.should_detach {
            let _ = client_tx.send(ClientFrame::Detach);
            return Ok(true);
        }

        if app.should_quit {
            let _ = client_tx.send(ClientFrame::Detach);
            return Ok(false);
        }

        pending |= dirty;
        if pending && last_draw.elapsed() >= FRAME {
            while let Ok(f) = client_rx.try_recv() {
                absorb_client_server_frame(app, client_tx, log, f).await;
            }
            screen.draw(|buf, area| app.render(buf, area))?;
            last_draw = std::time::Instant::now();
            pending = false;
        }
    }
    Ok(false)
}

async fn absorb_client_server_frame(
    app: &mut App,
    client_tx: &mpsc::UnboundedSender<ClientFrame>,
    log: &mut transcript::Log,
    frame: ClientServerFrame,
) {
    match frame {
        ClientServerFrame::Snapshot {
            lane,
            branch,
            messages,
            active_interrupt,
            status,
        } => {
            app.apply_snapshot(&lane, &branch, &messages, active_interrupt.as_ref(), status);
        }
        ClientServerFrame::StatusChange { lane, status } => {
            app.set_lane_status(&lane, status);
            drain_client_queue(app, &lane, client_tx, log);
        }
        ClientServerFrame::Event { lane, event } => {
            if matches!(event, agui::Incoming::RunFinished { .. }) {
                if let Some(i) = app.panes.iter().position(|p| p.lane == lane) {
                    let done = app.panes[i]
                        .transcript
                        .last()
                        .filter(|e| e.who == Who::Model)
                        .map(|e| e.text.clone());
                    if let Some(text) = done {
                        log.append(app.panes[i].cursor(), Who::Model, &text);
                        app.panes[i].logged += 1;
                    }
                }
            }
            let _ = app.apply(&lane, event);
        }
        ClientServerFrame::Notice { text } => {
            app.set_notice(text);
        }
        ClientServerFrame::Error { message } => {
            app.push(Who::System, message);
        }
    }
    drain_all_client_queues(app, client_tx, log);
}

fn drain_all_client_queues(
    app: &mut App,
    client_tx: &mpsc::UnboundedSender<ClientFrame>,
    log: &mut transcript::Log,
) {
    let ready: Vec<String> = app
        .panes
        .iter()
        .filter(|p| p.status() == Status::Ready && !p.queue.is_empty() && p.pending_asks.is_empty())
        .map(|p| p.lane.clone())
        .collect();

    for lane in ready {
        drain_client_queue(app, &lane, client_tx, log);
    }
}

fn drain_client_queue(
    app: &mut App,
    lane_name: &str,
    client_tx: &mpsc::UnboundedSender<ClientFrame>,
    log: &mut transcript::Log,
) {
    if let Some(idx) = app.panes.iter().position(|p| p.lane == lane_name) {
        if app.panes[idx].status() == Status::Ready
            && !app.panes[idx].queue.is_empty()
            && app.panes[idx].pending_asks.is_empty()
        {
            let next = app.panes[idx].queue.remove(0);
            dispatch_client_prompt(app, lane_name, client_tx, log, next);
        }
    }
}

fn dispatch_client_prompt(
    app: &mut App,
    lane: &str,
    client_tx: &mpsc::UnboundedSender<ClientFrame>,
    log: &mut transcript::Log,
    text: String,
) {
    app.history.push(text.clone());
    if let Some(pane) = app.panes.iter_mut().find(|p| p.lane == lane) {
        log.append(pane.cursor(), Who::User, &text);
        pane.logged += 1;
        pane.input.clear();
        pane.cursor = 0;
        pane.hist = None;
        pane.draft.clear();
    }
    app.push(Who::User, text.clone());
    app.sent_request(lane);
    let _ = client_tx.send(ClientFrame::Prompt {
        lane: lane.to_owned(),
        text,
    });
}

fn handle_client_key(
    app: &mut App,
    client_tx: &mpsc::UnboundedSender<ClientFrame>,
    log: &mut transcript::Log,
    speaker: &mut Option<Box<dyn tts::StreamingTts>>,
    ev: TermEvent,
) -> bool {
    if let TermEvent::Paste(text) = ev {
        if text.is_empty() {
            return false;
        }
        if app.panel_open() {
            return app.panel_paste(&text);
        }
        app.mode = Mode::Send;
        app.pane_mut().selected = None;
        app.pane_mut().insert_str(&text);
        return true;
    }
    let TermEvent::Key(key) = ev else {
        return matches!(ev, TermEvent::Resize(..));
    };
    if key.kind != KeyEventKind::Press {
        return false;
    }

    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    if ctrl && key.code == KeyCode::Char('d') {
        if app.quit_armed {
            app.should_quit = true;
        }
        app.quit_armed = true;
        return true;
    }
    if ctrl && key.code == KeyCode::Char('\\') {
        app.should_detach = true;
        return true;
    }
    let was_armed = std::mem::take(&mut app.quit_armed);
    let esc_was_armed = std::mem::take(&mut app.esc_armed);

    if app.panel_open() {
        return match key.code {
            KeyCode::Up => app.panel_move(-1),
            KeyCode::Down => app.panel_move(1),
            KeyCode::Char(' ') => app.panel_toggle() || app.panel_type(' '),
            KeyCode::Left => app.panel_move_left(ctrl || alt),
            KeyCode::Right => app.panel_move_right(ctrl || alt),
            KeyCode::Char('b') if alt => app.panel_move_left(true),
            KeyCode::Char('f') if alt => app.panel_move_right(true),
            KeyCode::Home => app.panel_home(),
            KeyCode::End => app.panel_end(),
            KeyCode::Char('a') if ctrl => app.panel_home(),
            KeyCode::Char('e') if ctrl => app.panel_end(),
            KeyCode::Char('u') if ctrl => app.panel_kill_to_start(),
            KeyCode::Char('k') if ctrl => app.panel_kill_to_end(),
            KeyCode::Backspace if alt => app.panel_delete_word_back(),
            KeyCode::Delete => app.panel_delete_forward(),
            KeyCode::Enter => {
                let lane = app.pane().lane.clone();
                for (call_id, answer) in app.submit_panel() {
                    app.record_answer(&lane, "", &answer);
                    app.sent_request(&lane);
                    let _ = client_tx.send(ClientFrame::SubmitInterrupt {
                        lane: lane.clone(),
                        call_id,
                        answers: vec![answer],
                    });
                }
                true
            }
            KeyCode::Esc => {
                let lane = app.pane().lane.clone();
                for call_id in app.cancel_panel_calls() {
                    app.sent_request(&lane);
                    let _ = client_tx.send(ClientFrame::CancelInterrupt {
                        lane: lane.clone(),
                        call_id,
                    });
                }
                true
            }
            KeyCode::Backspace => app.panel_backspace(),
            KeyCode::Char('c') if ctrl => {
                app.should_quit = true;
                true
            }
            KeyCode::Char(c) => app.panel_char(c),
            _ => false,
        };
    }

    if ctrl && key.code == KeyCode::Char('w') {
        return app.close_pane() || was_armed;
    }
    if shift && matches!(key.code, KeyCode::Left | KeyCode::Right) {
        return app.focus_pane(if key.code == KeyCode::Left { -1 } else { 1 });
    }

    if shift && matches!(key.code, KeyCode::Up | KeyCode::Down) {
        return match key.code {
            KeyCode::Up => app.select_prev(),
            _ => app.select_next(),
        };
    }
    if app.mode == Mode::Chat {
        return match key.code {
            KeyCode::Char('y') => {
                let text = app
                    .pane()
                    .selected
                    .and_then(|i| app.pane().transcript.get(i))
                    .map(|e| e.text.clone());
                match text {
                    Some(t) => {
                        osc::copy(&t);
                        app.set_notice(format!("copied {} characters", t.chars().count()));
                    }
                    None => app.set_notice("nothing selected"),
                }
                true
            }
            KeyCode::Char('r') => app.rewind(),
            KeyCode::Char('f') => app.fork(),
            KeyCode::Esc => {
                app.leave_chat();
                true
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                app.should_quit = true;
                true
            }
            _ => {
                app.leave_chat();
                handle_client_key(app, client_tx, log, speaker, ev)
            }
        };
    }

    match key.code {
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.should_quit = true;
        }
        KeyCode::Esc => {
            if app.unqueue() {
                app.esc_armed = false;
            } else if app.pane().status() == Status::Waiting {
                if esc_was_armed {
                    app.interrupt();
                    let lane = app.pane().lane.clone();
                    let _ = client_tx.send(ClientFrame::CancelTurn { lane });
                } else {
                    app.esc_armed = true;
                }
            }
        }
        KeyCode::Up if app.menu_open() => return app.menu_move(-1),
        KeyCode::Down if app.menu_open() => return app.menu_move(1),
        KeyCode::Up => return app.history_prev(),
        KeyCode::Down => return app.history_next(),
        KeyCode::Tab | KeyCode::BackTab => return app.complete_slash(),
        KeyCode::Enter if shift || alt => app.pane_mut().insert('\n'),
        KeyCode::Char('j') if ctrl => app.pane_mut().insert('\n'),
        KeyCode::Enter => {
            let text = app.pane().input.trim().to_owned();
            if text.is_empty() {
                return false;
            }
            if text.starts_with('/') {
                let p = app.pane_mut();
                p.input.clear();
                p.cursor = 0;
                slash(app, speaker, &text);
                return true;
            }
            let lane = app.pane().lane.clone();
            if app.pane().status() == Status::Waiting {
                let p = app.pane_mut();
                p.queue.push(text);
                p.input.clear();
                p.cursor = 0;
            } else {
                dispatch_client_prompt(app, &lane, client_tx, log, text);
            }
        }
        KeyCode::Left => app.pane_mut().move_left(ctrl || alt),
        KeyCode::Right => app.pane_mut().move_right(ctrl || alt),
        KeyCode::Char('b') if alt => app.pane_mut().move_left(true),
        KeyCode::Char('f') if alt => app.pane_mut().move_right(true),
        KeyCode::Home => app.pane_mut().home(),
        KeyCode::End => app.pane_mut().end(),
        KeyCode::Char('a') if ctrl => app.pane_mut().home(),
        KeyCode::Char('e') if ctrl => app.pane_mut().end(),
        KeyCode::Char('u') if ctrl => app.pane_mut().kill_to_start(),
        KeyCode::Char('k') if ctrl => app.pane_mut().kill_to_end(),
        KeyCode::Backspace if alt => app.pane_mut().delete_word_back(),
        KeyCode::Backspace => {
            app.pane_mut().backspace();
        }
        KeyCode::Delete => app.pane_mut().delete_forward(),
        KeyCode::PageUp => {
            let p = app.pane_mut();
            p.scroll = p.scroll.saturating_add(10);
        }
        KeyCode::PageDown => {
            let p = app.pane_mut();
            p.scroll = p.scroll.saturating_sub(10);
        }
        KeyCode::Char(c) => app.pane_mut().insert(c),
        _ => return false,
    }
    true
}

// Eight handles, and a struct to hold them would only move the list somewhere
// else: every one is a distinct long-lived thing this loop owns for the life of
// the session, and none of them group into anything with a name.
#[allow(clippy::too_many_arguments)]
#[allow(dead_code)]
/// Generic over the screen's backend and the key source, and for one reason:
/// it makes the whole loop drivable without a terminal. `Screen` was already
/// generic -- `term.rs`'s own tests build one on `TestBackend` -- so the key
/// source was the only concrete thing left between this function and a
/// headless test.
///
/// The loop under test is therefore the loop that ships. There is no
/// `#[cfg(test)]` branch anywhere inside it, and there must not be: the value
/// of driving the real event loop evaporates the moment the tested path and
/// the shipped path differ in shape.
async fn run<B, K>(
    screen: &mut kobold::term::Screen<B>,
    app: &mut App,
    updates: &mut mpsc::Receiver<IncomingFrame>,
    commands: &mpsc::UnboundedSender<Command>,
    keys: &mut K,
    log: &mut transcript::Log,
    speaker: &mut Option<Box<dyn tts::StreamingTts>>,
    notices: &mut mpsc::UnboundedReceiver<String>,
    mcp: &tools::Sources,
) -> Result<(), Box<dyn std::error::Error>>
where
    B: ratatui::backend::Backend,
    B::Error: std::error::Error + Send + Sync + 'static,
    K: futures_util::Stream<Item = std::io::Result<TermEvent>> + Unpin,
{
    screen.draw(|buf, area| app.render(buf, area))?;
    let _guard = progress::Guard;
    let mut reported = progress::State::Clear;
    // A turn that finishes instantly would otherwise flash indeterminate and
    // vanish, reading as "nothing happened". A brief full bar says "done".
    let mut done_at: Option<std::time::Instant> = None;
    const DONE_FLASH: std::time::Duration = std::time::Duration::from_millis(700);

    // Only drives the spinner, and only while a turn is in flight. An idle
    // screen never repaints.
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(110));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    // Repaints are paced. A paste on a terminal without bracketed paste, a
    // held-down key, or a burst of deltas all arrive as many events in a row,
    // and painting each one is work the user cannot see. Anything that arrives
    // inside the interval is folded into one frame.
    //
    // This adds no latency when idle: `last_draw` is already older than the
    // interval, so the first event after a pause paints immediately. Only a
    // burst is delayed, and only by the interval.
    const FRAME: std::time::Duration = std::time::Duration::from_millis(8);
    let mut pending = false;
    let mut last_draw = std::time::Instant::now();

    // See `SilenceWatch` for what the two thresholds mean and why they differ.
    let mut silence = SilenceWatch::new(std::time::Instant::now());

    loop {
        // Redraw only when something changed. A fixed tick would repaint at a
        // constant rate whether or not the screen differs, which is wasted work
        // and visible flicker on a slow terminal.
        let dirty = tokio::select! {
            update = updates.recv() => match update {
                Some(u) => {
                    let was_delta = matches!(
                        u,
                        IncomingFrame::Event { event: Incoming::TextMessageContent { .. }, .. }
                    );
                    absorb(app, commands, log, mcp, u).await;
                    // After `absorb`, not before: a tool call runs inline in
                    // there, so that time is Kobold's rather than silence
                    // from the adapter.
                    silence.saw_update(was_delta, std::time::Instant::now());
                    true
                }
                // The socket task dropping its sender is how a connection
                // failure ends -- see the eight send-then-return paths in
                // net::run. Returning it as an error, rather than a bare
                // break, is what makes a dead socket exit non-zero instead
                // of looking exactly like /quit.
                None => return Err(disconnect_reason(app).into()),
            },
            key = keys.next() => match key {
                Some(Ok(ev)) => handle_key(app, commands, log, speaker, ev),
                Some(Err(e)) => return Err(e.into()),
                None => break,
            },
            notice = notices.recv() => match notice {
                // Engine progress: install, download, model load. Shown as a
                // system note so a slow first run does not look like a hang.
                Some(line) => { app.set_notice(line); true }
                None => false,
            },
            // Guarantees the deferred frame actually lands: without this a
            // burst that ends could leave the last repaint queued until some
            // unrelated event arrived.
            _ = tokio::time::sleep(FRAME.saturating_sub(last_draw.elapsed())), if pending => false,
            _ = tick.tick() => {
                if app.panes.iter().any(|p| p.status() == Status::Waiting)
                    || app.notice_live()
                    || done_at.is_some()
                {
                    app.spinner = app.spinner.wrapping_add(1);
                    true
                } else {
                    false
                }
            }
        };

        // Reported to the terminal, not drawn by us: Ghostty and friends put a
        // bar in their own chrome, so a long turn is visible from another
        // window without switching to this one.
        let busy = app.panes.iter().any(|p| p.status() == Status::Waiting);
        let now = std::time::Instant::now();
        silence.turn_boundary(busy, now);
        match silence.check(busy, now) {
            Silence::Fine => {}
            // Named, because "wedged" and "crashed" send someone looking in
            // different places.
            Silence::Wedged => {
                return Err(format!(
                    "the adapter stopped responding {}s into a reply",
                    silence.silent_for(now).as_secs()
                )
                .into())
            }
            Silence::Notice => {
                app.set_notice(format!(
                    "no reply for {} minutes -- the model may still be thinking, or the \
                     adapter may be wedged. Esc twice interrupts.",
                    silence.silent_for(now).as_secs() / 60
                ));
                silence.noticed = true;
            }
        }
        let failed = app.any_pane_failed();
        if busy {
            done_at = None;
        } else if reported == progress::State::Indeterminate {
            done_at = Some(std::time::Instant::now());
        }
        let want = if failed {
            progress::State::Error
        } else if busy {
            progress::State::Indeterminate
        } else if done_at.is_some_and(|t| t.elapsed() < DONE_FLASH) {
            progress::State::At(100)
        } else {
            done_at = None;
            progress::State::Clear
        };
        if want != reported {
            progress::set(want);
            reported = want;
        }

        if let Some(s) = speaker.as_mut() {
            if std::mem::take(&mut app.speech_cancel) {
                s.cancel();
            }
            for chunk in app.speech.drain(..) {
                s.enqueue(chunk);
            }
        } else {
            app.speech.clear();
            app.speech_cancel = false;
        }

        if app.should_quit {
            let _ = commands.send(Command::Quit);
            break;
        }
        pending |= dirty;
        if pending && last_draw.elapsed() >= FRAME {
            // Drain anything else already queued before painting, so a burst of
            // deltas costs one redraw instead of one per delta. Through the same
            // handler as the select arm: a `Completed` landing in the same burst
            // as its final deltas is the ordinary case, not a rare one, and
            // applying it any other way loses the turn from the log.
            while let Ok(u) = updates.try_recv() {
                let was_delta = matches!(
                    u,
                    IncomingFrame::Event {
                        event: Incoming::TextMessageContent { .. },
                        ..
                    }
                );
                absorb(app, commands, log, mcp, u).await;
                silence.saw_update(was_delta, std::time::Instant::now());
            }
            screen.draw(|buf, area| app.render(buf, area))?;
            last_draw = std::time::Instant::now();
            pending = false;
        }
    }
    Ok(())
}

/// One turn, plain stdout, no UI. Exits non-zero if the turn fails, so a
/// caller can branch on it.
#[allow(clippy::too_many_arguments)]
async fn oneshot(
    prompt: &str,
    socket: &std::path::Path,
    session_id: &str,
    workdir: &std::path::Path,
    adapter_override: Option<&str>,
    mock_mode: bool,
    unconfined: bool,
    ws_port: Option<u16>,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write as _;

    if prompt.is_empty() {
        return Err("empty prompt".into());
    }
    let (cfg, _) = settings::Settings::load(workdir);
    let adapter_name = adapter_override
        .or_else(|| (!cfg.adapter.is_empty()).then_some(cfg.adapter.as_str()))
        .unwrap_or(DEFAULT_ADAPTER);
    let is_external_adapter = adapter_name.contains("acp")
        || adapter_name.contains("tmux")
        || adapter_name.contains("pty")
        || cfg.agents.values().any(|a| a.enabled);
    if !mock_mode && !is_external_adapter {
        let _ = std::env::var("LLM_API_KEY")
            .or_else(|_| std::env::var("OPENAI_API_KEY"))
            .map_err(|_| {
                "no API key: set LLM_API_KEY (or use -a kobold-adapter-acp for external agents)"
            })?;
    }

    let _guard = progress::Guard;
    progress::set(progress::State::Indeterminate);

    let handle = ensure_daemon(
        socket,
        session_id,
        workdir,
        adapter_override,
        mock_mode,
        unconfined,
        ws_port,
    )
    .await?;
    let (client_tx, mut client_rx, mut daemon_guard) = handle.into_parts();

    let _ = client_tx.send(ClientFrame::Prompt {
        lane: LANE.to_owned(),
        text: prompt.to_owned(),
    });

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut failure: Option<String> = None;

    while let Some(frame) = client_rx.recv().await {
        match frame {
            ClientServerFrame::Event {
                event: agui::Incoming::TextMessageContent { delta, .. },
                ..
            }
            | ClientServerFrame::Event {
                event:
                    agui::Incoming::TextMessageChunk {
                        delta: Some(delta), ..
                    },
                ..
            } => {
                let _ = out.write_all(delta.as_bytes());
                let _ = out.flush();
            }
            ClientServerFrame::Event {
                event: agui::Incoming::RunFinished { .. },
                ..
            } => break,
            ClientServerFrame::Event {
                event: agui::Incoming::RunError { code, message, .. },
                ..
            } => {
                failure = Some(format!("{} {message}", code.unwrap_or_else(|| "?".into())));
                break;
            }
            ClientServerFrame::Error { message } => {
                failure = Some(message);
                break;
            }
            ClientServerFrame::StatusChange {
                status: LaneStatus::Gone,
                ..
            } => {
                failure = Some("daemon lane closed unexpectedly".to_string());
                break;
            }
            _ => {}
        }
    }

    daemon_guard.cleanup();

    if let Some(why) = failure {
        // No trailing newline, and nothing else: on a failed turn stdout is
        // the caller's data channel and must stay empty, which
        // `stdout_carries_the_reply_and_nothing_else` pins. The reason goes
        // to stderr through the error return.
        progress::set(progress::State::Error);
        return Err(why.into());
    }
    out.write_all(b"\n")?;
    out.flush()?;
    Ok(())
}

/// Everything the adapter is told once, before any command.
/// Which hosts the adapter may reach.
///
/// The default is the one host Kobold's own adapter needs. It is a default
/// rather than a hardcoding because a third-party adapter talks to a
/// different provider -- but it is **not** an escape hatch: there is no value
/// of `adapter_allow` that means "any host", so configuring a new adapter
/// means naming what it may reach.
#[allow(dead_code)]
fn adapter_allow(cfg: &settings::Settings) -> Vec<String> {
    if cfg.adapter_allow.is_empty() {
        vec!["api.openai.com".to_owned()]
    } else {
        cfg.adapter_allow.clone()
    }
}

#[allow(dead_code)]
fn startup_frame(api_key: String, cfg: &settings::Settings, mcp: &tools::Sources) -> Startup {
    Startup {
        // Filled in by `Adapter::spawn`, which is the only place that knows
        // where the broker's socket landed.
        egress: None,
        api_key,
        model: net::Model {
            name: cfg.model.clone(),
            effort: cfg.reasoning_effort.clone(),
            server_tools: cfg.server_tools.clone(),
            // Local and MCP tools in one list. The adapter describes them to
            // the provider and reports the calls back; which of them Kobold
            // answers locally and which it answers over MCP is Kobold's
            // business and stays on this side.
            tools: tools::schemas()
                .into_iter()
                .map(|(n, d, s)| (n.to_owned(), d.to_owned(), s))
                .chain(mcp.all_schemas())
                .collect(),
        },
    }
}

/// Which adapter binary to run.
///
/// Found beside Kobold the same way the speech engine is, because it is
/// shipped the same way: built into the same target directory, and installed
/// next to the binary that spawns it.
#[allow(dead_code)]
fn adapter_binary(cfg: &settings::Settings) -> String {
    if !cfg.adapter.is_empty() {
        return cfg.adapter.clone();
    }
    let mut roots: Vec<std::path::PathBuf> = vec![std::path::PathBuf::from(".")];
    if let Ok(exe) = std::env::current_exe() {
        roots.extend(exe.ancestors().skip(1).take(4).map(|p| p.to_path_buf()));
    }
    roots
        .iter()
        .flat_map(|r| {
            [
                r.join(DEFAULT_ADAPTER),
                r.join("target/release").join(DEFAULT_ADAPTER),
            ]
        })
        .find(|p| p.is_file())
        .map(|p| p.to_string_lossy().into_owned())
        // Falling back to the bare name lets PATH answer, and makes the
        // failure "could not start the adapter" with a name in it rather
        // than a silent nothing.
        .unwrap_or_else(|| DEFAULT_ADAPTER.to_owned())
}

fn bundled_sidecar() -> Option<String> {
    let mut roots: Vec<std::path::PathBuf> = vec![std::path::PathBuf::from(".")];
    if let Ok(exe) = std::env::current_exe() {
        roots.extend(exe.ancestors().skip(1).take(4).map(|p| p.to_path_buf()));
    }

    let native = roots
        .iter()
        .flat_map(|r| [r.join("kobold-tts"), r.join("target/release/kobold-tts")])
        .find(|p| p.is_file());
    if let Some(p) = native {
        return Some(p.to_string_lossy().into_owned());
    }

    roots
        .into_iter()
        .map(|r| r.join("tools").join("pocket-say"))
        .find(|p| p.is_file())
        .map(|p| p.to_string_lossy().into_owned())
}

/// Handle a line beginning with `/`. Slash commands are local UI controls, so
/// they are consumed here and never reach the model.
fn slash(app: &mut App, speaker: &mut Option<Box<dyn tts::StreamingTts>>, line: &str) {
    let mut parts = line[1..].split_whitespace();
    let cmd = parts.next().unwrap_or("");
    let arg = parts.next().unwrap_or("");

    match cmd {
        "voice" => {
            // `/voice <name>` switches voice and turns speech on if it was off,
            // because asking for a specific voice plainly means "speak".
            if !arg.is_empty() && arg != "on" && arg != "off" {
                if !tts::VOICES.contains(&arg) {
                    app.push(
                        Who::System,
                        format!(
                            "unknown voice '{arg}'; available: {}",
                            tts::VOICES.join(", ")
                        ),
                    );
                    return;
                }
                let switched = speaker.as_mut().is_some_and(|s| s.set_voice(arg));
                app.voice = true;
                if let Some(root) = &app.root {
                    let _ = settings::Settings::update_voice(root, true, Some(arg));
                }
                if let Some(s) = speaker.as_mut() {
                    s.warm();
                }
                app.push(
                    Who::System,
                    if switched {
                        format!("voice on, {arg}")
                    } else {
                        format!("voice on, {arg} — engine cannot switch live; restart to apply")
                    },
                );
                return;
            }
            app.voice = match arg {
                "on" => true,
                "off" => false,
                _ => !app.voice,
            };
            if let Some(root) = &app.root {
                let _ = settings::Settings::update_voice(root, app.voice, None);
            }
            if app.voice {
                if let Some(s) = speaker.as_mut() {
                    s.warm();
                }
            }
            let note = match (app.voice, app.voice_engine, app.voice_remote) {
                (false, _, _) => "voice off".to_owned(),
                (true, false, _) => {
                    "voice on, but no engine: set KOBOLD_TTS_CMD (macOS: KOBOLD_TTS_CMD=say)"
                        .to_owned()
                }
                (true, true, false) => "voice on".to_owned(),
                (true, true, true) => {
                    "voice on — audio is sent to the configured address, so `tools/kobold-play` \
                     must be listening there"
                        .to_owned()
                }
            };
            app.push(Who::System, note);
        }
        "model" => {
            let (mut cfg, _) = if let Some(root) = &app.root {
                settings::Settings::load(root)
            } else {
                (settings::Settings::default(), None)
            };
            let cur_harness = cfg.active_harness().to_string();
            let cur_model = cfg.model.clone();

            if arg.is_empty() {
                let mut out = format!(
                    "Current: {} \u{b7} {}\n\nAvailable models for {}:",
                    kobold::catalog::harness_title(&cur_harness),
                    cur_model,
                    cur_harness
                );
                if let Some(models) = cfg.models_cache.get(&cur_harness) {
                    for m in models {
                        out.push_str(&format!("\n  - {m}"));
                    }
                } else {
                    let def_m = kobold::catalog::default_model_for_harness(&cur_harness);
                    out.push_str(&format!("\n  - {def_m} (default)"));
                }
                out.push_str("\n\nSwitch model: /model <name> or /model <name>@<harness>");
                app.push(Who::System, out);
                return;
            }

            match kobold::catalog::match_model(&cfg.models_cache, Some(&cur_harness), arg) {
                Ok((matched_harness, matched_model)) => {
                    let harness_changed = matched_harness != cur_harness;
                    let model_changed = matched_model != cur_model;

                    if let Some(root) = &app.root {
                        let _ =
                            cfg.update_harness_and_model(root, &matched_harness, &matched_model);
                    }
                    app.harness = matched_harness.clone();
                    app.model = matched_model.clone();

                    let msg = if harness_changed {
                        format!(
                            "switched to model: {} and harness: {}",
                            matched_model,
                            kobold::catalog::harness_title(&matched_harness)
                        )
                    } else if model_changed {
                        format!("switched to model: {matched_model}")
                    } else {
                        format!("already using model: {matched_model}")
                    };
                    app.push(Who::System, msg);
                }
                Err(err) => {
                    app.push(Who::System, err);
                }
            }
        }
        "backend" | "harness" => {
            let (mut cfg, _) = if let Some(root) = &app.root {
                settings::Settings::load(root)
            } else {
                (settings::Settings::default(), None)
            };
            let cur_harness = cfg.active_harness();

            if arg.is_empty() {
                let mut out = format!(
                    "Current harness: {} ({})\n\nAvailable harnesses:\n",
                    kobold::catalog::harness_title(cur_harness),
                    cur_harness
                );
                for h in [
                    kobold::catalog::HARNESS_CLAUDE_CODE,
                    kobold::catalog::HARNESS_GROK_BUILD,
                    kobold::catalog::HARNESS_CODEX,
                    kobold::catalog::HARNESS_OPENCODE,
                    kobold::catalog::HARNESS_ANTIGRAVITY,
                    kobold::catalog::HARNESS_OPENAI,
                    kobold::catalog::HARNESS_OPENROUTER,
                ] {
                    let title = kobold::catalog::harness_title(h);
                    let def_m = kobold::catalog::default_model_for_harness(h);
                    let marker = if h == cur_harness { " (active)" } else { "" };
                    out.push_str(&format!("  - {h} ({title}, default: {def_m}){marker}\n"));
                }
                out.push_str("\nSwitch with: /backend <name>");
                app.push(Who::System, out);
                return;
            }

            if let Some(target_harness) = kobold::catalog::normalize_harness(arg) {
                let def_model = kobold::catalog::default_model_for_harness(target_harness);
                if let Some(root) = &app.root {
                    let _ = cfg.update_harness_and_model(root, target_harness, def_model);
                }
                app.harness = target_harness.to_string();
                app.model = def_model.to_string();
                app.push(
                    Who::System,
                    format!(
                        "switched harness to: {} (model: {})",
                        kobold::catalog::harness_title(target_harness),
                        def_model
                    ),
                );
            } else {
                app.push(
                    Who::System,
                    format!("unknown harness '{arg}'. Run /backend to see available harnesses."),
                );
            }
        }
        "detach" => app.should_detach = true,
        "quit" | "q" => app.should_quit = true,
        // Generated from the same table that drives completion, so the list
        // shown here and the list offered above the prompt cannot disagree.
        "help" | "" => {
            let mut out = kobold::complete::COMMANDS
                .iter()
                .map(|c| format!("/{} — {}", c.name, c.help))
                .collect::<Vec<_>>()
                .join("\n");
            out.push_str(&format!("\n  voices: {}", tts::VOICES.join(", ")));
            app.push(Who::System, out);
        }
        other => app.push(Who::System, format!("unknown command: /{other}")),
    }
}

/// Send `text` as a turn on the active pane: record it, log it, dispatch it.
#[allow(dead_code)]
fn dispatch(
    app: &mut App,
    commands: &mpsc::UnboundedSender<Command>,
    log: &mut transcript::Log,
    text: String,
) {
    app.history.push(text.clone());
    log.append(app.pane().cursor(), Who::User, &text);
    app.pane_mut().logged += 1;

    let (lane, prev, replay) = {
        let p = app.pane_mut();
        p.input.clear();
        p.cursor = 0;
        p.hist = None;
        p.draft.clear();
        (p.lane.clone(), p.last_response_id.clone(), p.replay())
    };
    app.push(Who::User, text.clone());
    app.pane_mut().sent_request();
    let _ = commands.send(Command::Send {
        lane,
        text,
        previous_response_id: prev,
        replay,
    });
}

/// What to do about an adapter that has not said anything for a while.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Silence {
    /// Nothing to do -- no turn in flight, or not silent long enough.
    Fine,
    /// Tell the user, and leave the turn running.
    Notice,
    /// Treat as death.
    Wedged,
}

/// Two silences, two different failures, and only one of them has a bound.
///
/// Measured against the real API, max effort, a hard prompt: 435 SECONDS of
/// complete silence before the first delta -- reasoning produces no wire
/// traffic at all -- and then 1211 deltas whose largest gap was 31ms, p50
/// 2ms, p99 31ms.
///
/// So mid-stream silence is bounded and tight. `STREAM_STALL` is 160x the
/// worst gap ever observed, and a firing there is a wedge with high
/// confidence, so it is treated as death.
///
/// Silence *before* the first update is not bounded. 435s is one
/// observation, not a maximum -- a harder prompt or a busier server goes
/// longer and there is no basis for saying how much. So that one only ever
/// puts a note on screen. Killing a turn that is merely slow would lose the
/// user minutes of work they have already paid for, which is the exact
/// failure this mechanism exists to prevent; the user has interrupt if they
/// want it, and the information is the deliverable.
///
/// Deliberately no settings key for either. A knob derived from a single
/// observation is premature, and someone hitting this in anger is a bug
/// report with data attached -- better input than a default nobody can
/// calibrate.
#[allow(dead_code)]
const STREAM_STALL: std::time::Duration = std::time::Duration::from_secs(5);
#[allow(dead_code)]
const FIRST_UPDATE_NOTICE: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// Tracks how long the adapter has been quiet, and what that means.
///
/// A struct with an injected clock rather than arithmetic inline in the event
/// loop, because the loop only runs with a live terminal and this is the part
/// that decides whether to end someone's session.
#[allow(dead_code)]
#[derive(Debug)]
struct SilenceWatch {
    last_update: std::time::Instant,
    /// Whether this turn has produced any output yet, which is what selects
    /// between the two thresholds.
    streaming: bool,
    /// The notice is said once. Repeating it every frame would bury the
    /// transcript under the same line.
    noticed: bool,
    was_busy: bool,
}

#[allow(dead_code)]
impl SilenceWatch {
    fn new(now: std::time::Instant) -> Self {
        Self {
            last_update: now,
            streaming: false,
            noticed: false,
            was_busy: false,
        }
    }

    /// Called every pass with whether a turn is in flight. Resets the state
    /// on the edge into a turn, because nothing has been heard for that turn
    /// yet however recently the last one spoke.
    fn turn_boundary(&mut self, busy: bool, now: std::time::Instant) {
        if busy && !self.was_busy {
            self.last_update = now;
            self.streaming = false;
            self.noticed = false;
        }
        self.was_busy = busy;
    }

    fn saw_update(&mut self, was_delta: bool, now: std::time::Instant) {
        self.streaming |= was_delta;
        self.last_update = now;
    }

    fn check(&self, busy: bool, now: std::time::Instant) -> Silence {
        if !busy {
            return Silence::Fine;
        }
        let silent_for = now.duration_since(self.last_update);
        if self.streaming {
            return if silent_for >= STREAM_STALL {
                Silence::Wedged
            } else {
                Silence::Fine
            };
        }
        if !self.noticed && silent_for >= FIRST_UPDATE_NOTICE {
            return Silence::Notice;
        }
        Silence::Fine
    }

    fn silent_for(&self, now: std::time::Instant) -> std::time::Duration {
        now.duration_since(self.last_update)
    }
}

/// Why the socket died, for the caller once `updates.recv()` returns `None`.
/// Every path that ends the connection sends `Disconnected(reason)` before
/// dropping its sender, and `app.apply` has already turned that into
/// `Status::Gone` on every pane by the time the channel actually closes, so
/// the reason is read back from there rather than threaded through a second
/// channel.
///
/// The fallback covers more than "should not happen": the socket task can
/// also disappear by panicking before it reaches a `Disconnected` send, in
/// which case no pane is ever `Gone` and the ordering this relies on never
/// held in the first place. That is the one real path to the generic
/// message rather than the recorded one -- worth knowing, since a fallback
/// standing in for a panic is not the same claim as a fallback standing in
/// for "unreachable".
fn disconnect_reason(app: &App) -> String {
    app.panes
        .iter()
        .find_map(|p| p.gone_reason().map(str::to_owned))
        .unwrap_or_else(|| "connection closed".to_owned())
}

/// Close out the parked `ask` call the panel was answering: send `output`
/// back as the call's result. The matching -- and the refusal to answer a
/// stale token -- is `App::resolve_pending_ask`, kept on `App` rather than
/// here so it is reachable from an integration test without a `Command`
/// channel or IO to stand up.
#[allow(dead_code)]
fn resolve_ask(
    app: &mut App,
    commands: &mpsc::UnboundedSender<Command>,
    token: String,
    output: String,
) {
    let Some((lane, ask)) = app.resolve_pending_ask(&token) else {
        return;
    };
    // Into the conversation as ordinary messages, now that there is an answer
    // to pair with the question. Before this the transcript showed the call and
    // not the exchange, which is the machinery rather than what happened.
    app.record_answer(&lane, &ask.question, &output);
    // The answer starts the model working again on the same turn, so the
    // pane has a request outstanding from here until a terminal update for
    // it arrives. Without this the pane reads idle through the whole
    // continuation: the spinner stops, a queued message dispatches into a
    // turn still in progress, and the adapter-silence watch -- which keys
    // off exactly this flag -- is blind for the duration.
    app.sent_request(&lane);
    // An answered question is never a failure: the person at the keyboard
    // said something, and whatever they said is the tool's answer.
    let _ = commands.send(Command::ToolResult {
        lane,
        call_id: ask.call_id,
        output,
        error: false,
    });
}

/// Take one frame from the adapter: persist a turn that just finished, fold it
/// into the UI, and start anything queued behind it.
///
/// One function because there are two callers -- the select arm and the burst
/// drain before a paint -- and an update that reaches only one of them is an
/// update whose turn never lands in the log.
///
/// The one place that knows the envelope. A transport frame is not about any
/// turn, so it skips everything below and goes straight to `App`; it still
/// falls through to `drain_queues`, because a connection coming up is exactly
/// when a queued message becomes sendable.
#[allow(dead_code)]
async fn absorb(
    app: &mut App,
    commands: &mpsc::UnboundedSender<Command>,
    log: &mut transcript::Log,
    mcp: &tools::Sources,
    frame: IncomingFrame,
) {
    match frame {
        IncomingFrame::Transport(transport) => app.apply_transport(transport),
        IncomingFrame::Event { lane, event } => {
            // Persist a model turn once it is complete, not per delta: the transcript
            // is a record of turns, not of packets.
            if matches!(event, Incoming::RunFinished { .. }) {
                if let Some(i) = app.panes.iter().position(|p| p.lane == lane) {
                    let done = app.panes[i]
                        .transcript
                        .last()
                        .filter(|e| e.who == Who::Model)
                        .map(|e| e.text.clone());
                    if let Some(text) = done {
                        log.append(app.panes[i].cursor(), Who::Model, &text);
                        app.panes[i].logged += 1;
                    }
                }
            }
            // A tool call is answered here rather than in `App`, which owns no IO and
            // cannot run one. Running it is `tools::run`, which may hit the network
            // for an MCP tool; deciding what to do with the result is `tools::decide`,
            // which stays pure and sync so routing/parking rules are testable without
            // a runtime.
            //
            // `apply` runs first now, because under AG-UI a call is not
            // complete until `TOOL_CALL_END` has been folded in -- the pieces
            // arrive across three events and `App` is what assembles them.
            let effect = app.apply(&lane, event);
            if let Effect::RunTool(call) = effect {
                let lane = &lane;
                let call_id = call.id.clone();
                let root =
                    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                // `None` when nothing is configured, so an unused MCP path costs
                // nothing and is not a second way for a local tool to be reached.
                let source = (!mcp.is_empty()).then_some(mcp as &dyn tools::McpTools);
                let outcome = tools::run(&root, &call, source).await;
                match tools::decide(outcome) {
                    tools::Dispatch::Reply { output, error } => {
                        // The common path: every file read and every MCP call. This
                        // could not be expressed while the pane carried a flag,
                        // because the `response.completed` for the response that
                        // requested the tool arrives *after* this and would have
                        // overwritten it. A count says both things at once.
                        app.sent_request(lane);
                        let _ = commands.send(Command::ToolResult {
                            lane: lane.clone(),
                            call_id: call_id.clone(),
                            output,
                            error,
                        });
                    }
                    tools::Dispatch::Park(ask) => {
                        app.park_ask(lane, ask);
                    }
                }
            }
        }
    }
    // One call for both halves, as before the envelope existed. A
    // connection coming up is as much a reason to look at the queues as a
    // turn ending is, and two call sites would be two chances to forget.
    drain_queues(app, commands, log);
}

/// Start the next queued message on any pane that just went idle.
#[allow(dead_code)]
fn drain_queues(
    app: &mut App,
    commands: &mpsc::UnboundedSender<Command>,
    log: &mut transcript::Log,
) {
    let ready: Vec<usize> = app
        .panes
        .iter()
        .enumerate()
        // A parked question means the turn is still the user's to finish,
        // however the API feels about it: `response.completed` is terminal,
        // so `apply` marks the pane Ready with the panel still on screen.
        // Draining there dispatches a message the user queued behind a turn
        // that, from where they are sitting, has not ended -- reordering
        // their own messages onto a lane with an unanswered tool call.
        //
        // `pending_asks` is already per-lane and already survives the run
        // boundary, so no new state is needed to say this.
        .filter(|(_, p)| {
            p.status() == Status::Ready && !p.queue.is_empty() && p.pending_asks.is_empty()
        })
        .map(|(i, _)| i)
        .collect();
    for i in ready {
        let was = app.active;
        app.active = i;
        let next = app.panes[i].queue.remove(0);
        dispatch(app, commands, log, next);
        app.active = was;
    }
}

#[allow(dead_code)]
fn handle_key(
    app: &mut App,
    commands: &mpsc::UnboundedSender<Command>,
    log: &mut transcript::Log,
    speaker: &mut Option<Box<dyn tts::StreamingTts>>,
    ev: TermEvent,
) -> bool {
    // A bracketed paste arrives whole. Without it the terminal sends one key
    // event per character, and since every event triggers a repaint, pasting a
    // couple of kilobytes froze the UI for seconds.
    if let TermEvent::Paste(text) = ev {
        if text.is_empty() {
            return false;
        }
        // A paste while the panel is open belongs to the field being typed
        // into, not to the prompt behind it.
        if app.panel_open() {
            return app.panel_paste(&text);
        }
        // Pasting is typing, so it lands in the input even if the browser had
        // focus -- otherwise the text would silently go nowhere.
        app.mode = Mode::Send;
        app.pane_mut().selected = None;
        app.pane_mut().insert_str(&text);
        return true;
    }
    let TermEvent::Key(key) = ev else {
        // Resize and mouse events still need a repaint.
        return matches!(ev, TermEvent::Resize(..));
    };
    // Windows terminals emit both press and release; only act on press.
    if key.kind != KeyEventKind::Press {
        return false;
    }

    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    // Ctrl+D arms, a second one quits. Any other key disarms, so a stray press
    // cannot leave the app one keystroke from exiting.
    if ctrl && key.code == KeyCode::Char('d') {
        if app.quit_armed {
            app.should_quit = true;
        }
        app.quit_armed = true;
        return true;
    }
    let was_armed = std::mem::take(&mut app.quit_armed);
    // Same one-shot arming as ^D: any other key cancels it.
    let esc_was_armed = std::mem::take(&mut app.esc_armed);

    // The panel takes the keyboard whole while it is up: the turn is parked
    // on its answer, so nothing below -- pane focus, Chat mode, ordinary
    // typing -- should still be reachable underneath it.
    if app.panel_open() {
        return match key.code {
            KeyCode::Up => app.panel_move(-1),
            KeyCode::Down => app.panel_move(1),
            // Toggling is what space means on a choice row and nothing at all
            // on a text one, where it is simply a space. Asking the panel first
            // and falling through keeps the two from needing a mode.
            KeyCode::Char(' ') => app.panel_toggle() || app.panel_type(' '),
            // The message input's keys, on the panel's text field. A question
            // asking for a sentence has to be answerable with one, which means
            // word motion and the readline kills, not just append-and-delete.
            KeyCode::Left => app.panel_move_left(ctrl || alt),
            KeyCode::Right => app.panel_move_right(ctrl || alt),
            // macOS terminals send Option+arrows as ESC b / ESC f rather than
            // an arrow with a modifier, so word motion needs these too.
            KeyCode::Char('b') if alt => app.panel_move_left(true),
            KeyCode::Char('f') if alt => app.panel_move_right(true),
            KeyCode::Home => app.panel_home(),
            KeyCode::End => app.panel_end(),
            KeyCode::Char('a') if ctrl => app.panel_home(),
            KeyCode::Char('e') if ctrl => app.panel_end(),
            KeyCode::Char('u') if ctrl => app.panel_kill_to_start(),
            KeyCode::Char('k') if ctrl => app.panel_kill_to_end(),
            KeyCode::Backspace if alt => app.panel_delete_word_back(),
            KeyCode::Delete => app.panel_delete_forward(),
            KeyCode::Enter => {
                // One result per question the panel was carrying: the model may
                // have asked several in a turn, and each is its own call.
                for (call_id, answer) in app.submit_panel() {
                    resolve_ask(app, commands, call_id, answer);
                }
                true
            }
            KeyCode::Esc => {
                // Every one of them, not just the first: a turn left waiting on
                // a panel that is no longer on screen never ends.
                for call_id in app.cancel_panel_calls() {
                    resolve_ask(
                        app,
                        commands,
                        call_id,
                        "the user declined to answer".to_owned(),
                    );
                }
                true
            }
            KeyCode::Backspace => app.panel_backspace(),
            KeyCode::Char('c') if ctrl => {
                app.should_quit = true;
                true
            }
            KeyCode::Char(c) => app.panel_char(c),
            _ => false,
        };
    }

    if ctrl && key.code == KeyCode::Char('w') {
        return app.close_pane() || was_armed;
    }
    if shift && matches!(key.code, KeyCode::Left | KeyCode::Right) {
        return app.focus_pane(if key.code == KeyCode::Left { -1 } else { 1 });
    }

    // Browsing the transcript takes over the arrow keys entirely, so handle it
    // before the editing bindings rather than threading a check through each.
    if shift && matches!(key.code, KeyCode::Up | KeyCode::Down) {
        return match key.code {
            KeyCode::Up => app.select_prev(),
            _ => app.select_next(),
        };
    }
    if app.mode == Mode::Chat {
        return match key.code {
            KeyCode::Char('y') => {
                let text = app
                    .pane()
                    .selected
                    .and_then(|i| app.pane().transcript.get(i))
                    .map(|e| e.text.clone());
                match text {
                    Some(t) => {
                        osc::copy(&t);
                        app.set_notice(format!("copied {} characters", t.chars().count()));
                    }
                    None => app.set_notice("nothing selected"),
                }
                true
            }
            KeyCode::Char('r') => app.rewind(),
            KeyCode::Char('f') => app.fork(),
            KeyCode::Esc => {
                app.leave_chat();
                true
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                app.should_quit = true;
                true
            }
            // Any other key returns to the prompt, so typing just works.
            _ => {
                app.leave_chat();
                handle_key(app, commands, log, speaker, ev)
            }
        };
    }

    match key.code {
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.should_quit = true;
        }
        // Esc no longer quits -- that is ^D^D. One press empties the queue,
        // two in a row interrupt an in-flight turn.
        KeyCode::Esc => {
            if app.unqueue() {
                app.esc_armed = false;
            } else if app.pane().status() == Status::Waiting {
                if esc_was_armed {
                    app.interrupt();
                    let lane = app.pane().lane.clone();
                    let _ = commands.send(Command::Cancel { lane });
                } else {
                    app.esc_armed = true;
                }
            }
        }
        // The suggestion list takes the arrows while it is open, and hands
        // them back to history the moment it closes. It only opens on a
        // leading slash, so this cannot swallow history navigation from
        // someone writing an ordinary message.
        KeyCode::Up if app.menu_open() => return app.menu_move(-1),
        KeyCode::Down if app.menu_open() => return app.menu_move(1),
        KeyCode::Up => return app.history_prev(),
        KeyCode::Down => return app.history_next(),
        // Tab completes the highlighted suggestion, and does nothing at all
        // when there is none -- a tab character in a prompt is never what was
        // meant, and inserting one here would be silently unhelpful.
        KeyCode::Tab | KeyCode::BackTab => return app.complete_slash(),
        // Newline, not send. Shift+Enter needs the Kitty keyboard protocol to
        // be distinguishable from plain Enter at all, so Alt+Enter and Ctrl+J
        // are accepted too -- those reach every terminal.
        KeyCode::Enter if shift || alt => app.pane_mut().insert('\n'),
        KeyCode::Char('j') if ctrl => app.pane_mut().insert('\n'),
        KeyCode::Enter => {
            let text = app.pane().input.trim().to_owned();
            if text.is_empty() {
                return false;
            }
            // Typing during a turn queues rather than blocking, so a train of
            // thought does not have to wait on the model.
            // Local commands never reach the model, and never queue behind a
            // turn: they control the UI, so they take effect immediately.
            if text.starts_with('/') {
                let p = app.pane_mut();
                p.input.clear();
                p.cursor = 0;
                slash(app, speaker, &text);
                return true;
            }
            if app.pane().status() == Status::Waiting {
                let p = app.pane_mut();
                p.queue.push(text);
                p.input.clear();
                p.cursor = 0;
            } else {
                dispatch(app, commands, log, text);
            }
        }
        // Readline-style line editing. Left/Right walk characters, with Ctrl or
        // Alt for words -- terminals disagree on which modifier they send for
        // word motion, so both are accepted.
        KeyCode::Left => app.pane_mut().move_left(ctrl || alt),
        KeyCode::Right => app.pane_mut().move_right(ctrl || alt),
        // Meta-b / Meta-f, the readline word-motion keys. macOS terminals send
        // these for Option+Left/Right (as ESC b / ESC f) rather than an
        // arrow-with-modifier sequence, so binding the arrows alone leaves word
        // motion dead on a Mac.
        KeyCode::Char('b') if alt => app.pane_mut().move_left(true),
        KeyCode::Char('f') if alt => app.pane_mut().move_right(true),
        KeyCode::Home => app.pane_mut().home(),
        KeyCode::End => app.pane_mut().end(),
        KeyCode::Char('a') if ctrl => app.pane_mut().home(),
        KeyCode::Char('e') if ctrl => app.pane_mut().end(),
        KeyCode::Char('u') if ctrl => app.pane_mut().kill_to_start(),
        KeyCode::Char('k') if ctrl => app.pane_mut().kill_to_end(),
        // Alt+Backspace, not Ctrl+W: Ctrl+W closes a pane here, and having it
        // also delete a word would make a destructive action one slip away.
        KeyCode::Backspace if alt => app.pane_mut().delete_word_back(),
        KeyCode::Backspace => {
            app.pane_mut().backspace();
        }
        KeyCode::Delete => app.pane_mut().delete_forward(),
        KeyCode::PageUp => {
            let p = app.pane_mut();
            p.scroll = p.scroll.saturating_add(10);
        }
        KeyCode::PageDown => {
            let p = app.pane_mut();
            p.scroll = p.scroll.saturating_sub(10);
        }
        KeyCode::Char(c) => app.pane_mut().insert(c),
        _ => return false,
    }
    true
}

#[cfg(test)]
mod tests {
    use kobold::net::Transport;

    /// **The allowlist is the trust boundary, and nothing asserted what was
    /// on it.** Replacing the default with `vec![]`, `vec![String::new()]`
    /// or `vec!["xyzzy".into()]` all survived: fail-closed means none of
    /// those is a vulnerability, they are worse in a boring way -- Kobold
    /// ships unable to reach any provider and the suite stays green.
    ///
    /// So the assertion is against `broker::permitted`, the function that
    /// actually decides, rather than against the list's length or contents.
    /// A non-empty list of the wrong thing passes every weaker check.
    #[test]
    fn the_default_allowlist_grants_the_host_this_adapter_needs_and_no_other() {
        use kobold::broker;
        let allow = adapter_allow(&settings::Settings::default());

        // The host `kobold-openai` connects to. Named as a literal here
        // rather than imported because `kobold` does not depend on the
        // adapter crate -- deliberately, an adapter is a separate process --
        // so the coupling is checked from both ends: `ws.rs` asserts its own
        // `HOSTNAME` is this string, and this asserts the default permits it.
        // Either half alone would let the two drift apart silently.
        assert!(
            broker::permitted(&allow, "api.openai.com", 443),
            "the default allowlist does not permit the provider the shipped adapter dials"
        );

        // And the negative half, without which a list of everything passes.
        assert!(
            !broker::permitted(&allow, "evil.example", 443),
            "an unlisted host was allowed"
        );
        assert!(
            !broker::permitted(&allow, "api.openai.com", 80),
            "a bare entry must grant 443 and nothing else -- a plaintext port is the \
             credential leaving the machine unencrypted"
        );
        // The suffix bug, from the direction an attacker uses it.
        assert!(
            !broker::permitted(&allow, "api.openai.com.attacker.net", 443),
            "a suffix match granted an attacker-controlled host"
        );
    }

    #[test]
    fn a_configured_allowlist_replaces_the_default_rather_than_extending_it() {
        use kobold::broker;
        // The partner, and it pins a decision rather than an accident:
        // someone naming their own adapter's host must not silently keep
        // egress to a provider they are no longer using. There is also no
        // value that means "any host" -- configuring an adapter means saying
        // what it may reach.
        let cfg = settings::Settings {
            adapter_allow: vec!["api.anthropic.com".to_owned()],
            ..settings::Settings::default()
        };
        let allow = adapter_allow(&cfg);
        assert!(
            broker::permitted(&allow, "api.anthropic.com", 443),
            "the configured host"
        );
        assert!(
            !broker::permitted(&allow, "api.openai.com", 443),
            "the default host survived a configured allowlist"
        );
    }

    /// A finished run for `LANE`, in the envelope it arrives in.
    fn finished_frame() -> IncomingFrame {
        IncomingFrame::Event {
            lane: LANE.to_owned(),
            event: Incoming::RunFinished {
                base: Default::default(),
                thread_id: LANE.to_owned(),
                run_id: String::new(),
                usage: None,
                outcome: None,
                result: None,
            },
        }
    }

    fn finished_frame_for(lane: &str) -> IncomingFrame {
        IncomingFrame::Event {
            lane: lane.to_owned(),
            event: Incoming::RunFinished {
                base: Default::default(),
                thread_id: lane.to_owned(),
                run_id: String::new(),
                usage: None,
                outcome: None,
                result: None,
            },
        }
    }

    /// The three frames one tool call arrives as. `absorb` has to see all
    /// three: `apply` assembles them and only `TOOL_CALL_END` dispatches.
    fn tool_call_frames(call_id: &str, name: &str, arguments: &str) -> Vec<IncomingFrame> {
        let wrap = |event| IncomingFrame::Event {
            lane: LANE.to_owned(),
            event,
        };
        vec![
            wrap(Incoming::ToolCallStart {
                base: Default::default(),
                tool_call_id: call_id.to_owned(),
                tool_call_name: name.to_owned(),
                parent_message_id: None,
            }),
            wrap(Incoming::ToolCallArgs {
                base: Default::default(),
                tool_call_id: call_id.to_owned(),
                delta: arguments.to_owned(),
            }),
            wrap(Incoming::ToolCallEnd {
                base: Default::default(),
                tool_call_id: call_id.to_owned(),
            }),
        ]
    }
    use super::*;

    use std::time::Duration;

    /// A watcher with a clock the test controls, so a fifteen-minute
    /// threshold costs nothing to exercise.
    fn watch() -> (SilenceWatch, std::time::Instant) {
        let t0 = std::time::Instant::now();
        (SilenceWatch::new(t0), t0)
    }

    #[test]
    fn silence_does_nothing_when_no_turn_is_in_flight() {
        // An idle session is silent by definition. Firing here would end a
        // session that is merely sitting there.
        let (mut w, t0) = watch();
        w.saw_update(true, t0);
        for secs in [0u64, 6, 3600] {
            assert_eq!(
                w.check(false, t0 + Duration::from_secs(secs)),
                Silence::Fine
            );
        }
    }

    #[test]
    fn silence_mid_stream_is_a_wedge_once_it_passes_the_short_threshold() {
        // The bounded case. 31ms was the worst gap ever measured across 1211
        // deltas, so anything near this threshold is not a slow model.
        let (mut w, t0) = watch();
        w.turn_boundary(true, t0);
        w.saw_update(true, t0);
        assert_eq!(
            w.check(true, t0 + STREAM_STALL - Duration::from_millis(1)),
            Silence::Fine
        );
        assert_eq!(w.check(true, t0 + STREAM_STALL), Silence::Wedged);
        assert_eq!(
            w.check(true, t0 + Duration::from_secs(600)),
            Silence::Wedged
        );
    }

    #[test]
    fn reasoning_updates_postpone_the_notice_without_arming_the_tight_stall() {
        // **This is what asking for reasoning summaries bought.** Kobold sent
        // `reasoning.effort` and no `summary` for its whole life, so a long
        // think was 435 measured seconds of nothing on the wire and the
        // 15-minute notice was a guess about whether anything was alive.
        // Summaries stream, so the silence is now filled with real frames.
        let (mut w, t0) = watch();
        w.turn_boundary(true, t0);

        // A reasoning frame is an update but not a text delta.
        for minute in [5u64, 12, 20, 40] {
            w.saw_update(false, t0 + Duration::from_secs(minute * 60));
        }
        let last = t0 + Duration::from_secs(40 * 60);

        // Well past FIRST_UPDATE_NOTICE measured from the turn's start, and
        // not past it measured from the last frame -- which is the whole
        // point: the clock follows the wire, not the turn.
        assert_eq!(
            w.check(true, last + Duration::from_secs(60)),
            Silence::Fine,
            "a turn that is visibly reasoning was reported as silent"
        );

        // And the partner, without which the above is satisfied by a watch
        // that never notices anything: once the frames stop, it still does.
        assert_eq!(
            w.check(true, last + FIRST_UPDATE_NOTICE + Duration::from_secs(1)),
            Silence::Notice,
            "reasoning frames disabled the notice permanently"
        );

        // The tight mid-stream rule must stay disarmed. Reasoning summaries
        // arrive in bursts with gaps between parts, so applying the 5s stall
        // to them would kill turns that are working.
        assert_ne!(
            w.check(true, last + STREAM_STALL * 10),
            Silence::Wedged,
            "a gap between reasoning parts was treated as a wedge"
        );

        // Its partner: a real text delta does arm it.
        w.saw_update(true, last);
        assert_eq!(
            w.check(true, last + STREAM_STALL * 2),
            Silence::Wedged,
            "streaming never armed, so the assertion above proves nothing"
        );
    }

    #[test]
    fn silence_before_the_first_update_never_kills_however_long_it_lasts() {
        // The unbounded case, and the one that matters most: a reasoning turn
        // was measured silent for 435 seconds, and that is one observation
        // rather than a maximum. Killing here would lose the user minutes of
        // work they have already paid for.
        let (mut w, t0) = watch();
        w.turn_boundary(true, t0);
        for secs in [0u64, 6, 435, 900, 86_400] {
            assert_ne!(
                w.check(true, t0 + Duration::from_secs(secs)),
                Silence::Wedged,
                "killed a turn that had not started streaming after {secs}s"
            );
        }
        // Specifically past the mid-stream threshold, which is the boundary a
        // single shared timer would have got wrong.
        assert_eq!(w.check(true, t0 + STREAM_STALL * 10), Silence::Fine);
    }

    #[test]
    fn the_first_update_notice_fires_once_and_only_once() {
        // The counter-assertion to the test above: "never kills" is also
        // satisfied by a watcher that does nothing at all, so the notice has
        // to be shown to actually happen.
        let (mut w, t0) = watch();
        w.turn_boundary(true, t0);
        assert_eq!(
            w.check(true, t0 + FIRST_UPDATE_NOTICE - Duration::from_secs(1)),
            Silence::Fine
        );
        assert_eq!(w.check(true, t0 + FIRST_UPDATE_NOTICE), Silence::Notice);
        w.noticed = true;
        assert_eq!(
            w.check(true, t0 + FIRST_UPDATE_NOTICE * 4),
            Silence::Fine,
            "said it twice"
        );
    }

    #[test]
    fn a_delta_switches_a_turn_onto_the_short_threshold() {
        // The transition the whole design turns on. Before any output the
        // turn is on the generous timer; the first delta moves it to the
        // tight one, because reasoning is over and silence now means a wedge.
        let (mut w, t0) = watch();
        w.turn_boundary(true, t0);
        let late = t0 + Duration::from_secs(400);
        assert_eq!(
            w.check(true, late),
            Silence::Fine,
            "still thinking, still fine"
        );

        w.saw_update(true, late);
        assert_eq!(
            w.check(true, late + STREAM_STALL),
            Silence::Wedged,
            "output began, so silence now kills"
        );
    }

    #[test]
    fn a_non_delta_update_resets_the_clock_without_starting_the_short_threshold() {
        // `Connected` and a tool call are updates but not output. They prove
        // the adapter is alive, so the clock restarts -- but the turn has
        // still produced nothing, so it stays on the generous timer.
        let (mut w, t0) = watch();
        w.turn_boundary(true, t0);
        let t1 = t0 + Duration::from_secs(100);
        w.saw_update(false, t1);
        assert_eq!(
            w.check(true, t1 + STREAM_STALL * 10),
            Silence::Fine,
            "moved to the short threshold too early"
        );
        assert_eq!(
            w.check(true, t1 + FIRST_UPDATE_NOTICE),
            Silence::Notice,
            "the clock did not restart"
        );
    }

    #[test]
    fn a_new_turn_starts_its_clock_over_and_forgets_the_last_ones_output() {
        // Without the reset, a second turn inherits `streaming` from the
        // first and is put on the five-second threshold from the moment it
        // is sent -- so every reasoning turn after the first would be killed
        // five seconds in. This is the edge case that reset exists for.
        let (mut w, t0) = watch();
        w.turn_boundary(true, t0);
        w.saw_update(true, t0);
        // Turn ends.
        w.turn_boundary(false, t0 + Duration::from_secs(1));
        // A new one starts, much later.
        let t2 = t0 + Duration::from_secs(60);
        w.turn_boundary(true, t2);
        assert!(!w.streaming, "the new turn inherited the last one's output");
        assert_eq!(
            w.check(true, t2 + Duration::from_secs(400)),
            Silence::Fine,
            "a fresh reasoning turn was killed on the streaming threshold"
        );
    }

    #[test]
    fn an_idle_pass_does_not_reset_a_turn_that_is_still_running() {
        // `turn_boundary` fires on the edge INTO a turn and nowhere else. A
        // condition that also fired while idle would clear `streaming` on
        // every pass between turns -- and, worse, clear it mid-turn on any
        // pass where the pane happened not to read as busy, putting a
        // streaming turn back on the fifteen-minute threshold.
        let (mut w, t0) = watch();
        w.turn_boundary(true, t0);
        w.saw_update(true, t0);
        assert!(w.streaming);

        // Repeated passes while the same turn runs: nothing resets.
        w.turn_boundary(true, t0 + Duration::from_secs(1));
        assert!(w.streaming, "reset in the middle of a turn");

        // And an idle pass leaves the recorded state alone rather than
        // rewriting it.
        w.turn_boundary(false, t0 + Duration::from_secs(2));
        assert!(
            w.streaming,
            "an idle pass cleared what the last turn had seen"
        );
    }

    #[test]
    fn silent_for_reports_the_gap_the_message_quotes() {
        // It is only used to fill in the number the user reads, which is
        // exactly why a wrong one is easy to miss: "the adapter stopped
        // responding 0s into a reply" reads like a different bug.
        let (mut w, t0) = watch();
        w.saw_update(true, t0);
        assert_eq!(
            w.silent_for(t0 + Duration::from_secs(7)),
            Duration::from_secs(7)
        );
        assert_eq!(w.silent_for(t0), Duration::ZERO);
    }

    #[test]
    fn the_two_thresholds_are_not_the_same_number() {
        // The whole design is that these differ by orders of magnitude: one
        // is bounded by measurement and one is not. Collapsing them -- which
        // a well-meaning simplification would do -- reintroduces the choice
        // between killing slow turns and detecting nothing.
        assert!(
            FIRST_UPDATE_NOTICE > STREAM_STALL * 100,
            "the thresholds have converged: {FIRST_UPDATE_NOTICE:?} against {STREAM_STALL:?}"
        );
    }

    /// Everything `run` needs, with a scripted adapter on one side and a
    /// scripted keyboard on the other, and no terminal anywhere.
    ///
    /// This is the loop that ships. Nothing inside `run` knows it is being
    /// tested -- the only difference is which backend and which key source
    /// were handed to it, both of which are ordinary parameters.
    struct Headless {
        app: App,
        cmd_tx: mpsc::UnboundedSender<Command>,
        updates: mpsc::Receiver<IncomingFrame>,
        adapter: kobold::adapter::Adapter,
        log: transcript::Log,
        speaker: Option<Box<dyn tts::StreamingTts>>,
        notices: mpsc::UnboundedReceiver<String>,
        mcp: tools::Sources,
    }

    fn fake_adapter_path() -> String {
        let mut p = std::env::current_exe().expect("test exe");
        p.pop();
        if p.ends_with("deps") {
            p.pop();
        }
        p.join("fake-adapter").to_string_lossy().into_owned()
    }

    impl Headless {
        async fn new(script: &str) -> Self {
            let startup = Startup {
                egress: None,
                api_key: "sk-not-a-real-key".to_owned(),
                model: net::Model {
                    name: "test".to_owned(),
                    effort: "none".to_owned(),
                    server_tools: Vec::new(),
                    tools: Vec::new(),
                },
            };
            let (adapter, cmd_tx, updates) = kobold::adapter::Adapter::spawn(
                &fake_adapter_path(),
                &[script.to_owned()],
                &startup,
                &[],
            )
            .await
            .expect("spawn the fake adapter");
            let (_ntx, notices) = mpsc::unbounded_channel();
            let mut app = App::new(LANE, "b0");
            app.apply_transport(Transport::Connected);
            Headless {
                app,
                cmd_tx,
                updates,
                adapter,
                // Degrades to a silent no-op on an unopenable path, by
                // design, so it is already the fake this needs.
                log: transcript::Log::open(std::path::Path::new("/dev/null/nope")),
                speaker: None,
                notices,
                mcp: tools::Sources::new(),
            }
        }

        /// Drives the real loop with `keys` typed at it, and returns whatever
        /// it returned.
        async fn drive(&mut self, keys: Vec<TermEvent>) -> Result<(), Box<dyn std::error::Error>> {
            let mut screen = kobold::term::Screen::with_backend(
                ratatui::backend::TestBackend::new(80, 24),
                ratatui::layout::Rect::new(0, 0, 80, 24),
            );
            // Chained onto a stream that never yields, because an exhausted
            // key source means "stdin closed, quit" to the loop -- correct
            // for a real terminal, but here it would end the session before
            // the adapter had said anything. What ends these tests is the
            // adapter, which is the thing under test.
            let mut stream = futures_util::stream::iter(keys.into_iter().map(Ok))
                .chain(futures_util::stream::pending());
            // Bounded, because a test that hangs is worse than one that
            // fails: it reports nothing, and under `cargo mutants` a mutant
            // that stops the loop making progress shows as a timeout rather
            // than as the caught mutant it is.
            //
            // **Two minutes, and the number is a measurement.** It was twenty
            // seconds, which is only four times the five-second wedge
            // threshold the slowest test here waits on -- not the margin it
            // reads as, once process startup under a sandbox and a loaded box
            // are in the same budget. Under six CPU spinners the wedge test
            // failed one run in three, at 20.02s, with the loop's own
            // "never finished" message standing in for the wedge reason it
            // was asserting.
            //
            // The check that this is contention and not a hang is the one
            // that caught a real bug in `kill_live` a commit ago: **a test
            // filling exactly the bound, whatever the bound is, never
            // finishes; one completing far under it was only waiting.** At
            // 120s, under the same load, this completes in 5.01s -- the wedge
            // threshold, four times over.
            let out = tokio::time::timeout(
                std::time::Duration::from_secs(120),
                run(
                    &mut screen,
                    &mut self.app,
                    &mut self.updates,
                    &self.cmd_tx,
                    &mut stream,
                    &mut self.log,
                    &mut self.speaker,
                    &mut self.notices,
                    &self.mcp,
                ),
            )
            .await;
            self.adapter.shutdown().await;
            out.unwrap_or_else(|_| Err("the event loop never finished".into()))
        }
    }

    fn typed(text: &str) -> Vec<TermEvent> {
        let mut out: Vec<TermEvent> = text
            .chars()
            .map(|ch| {
                TermEvent::Key(crossterm::event::KeyEvent::new(
                    KeyCode::Char(ch),
                    KeyModifiers::NONE,
                ))
            })
            .collect();
        out.push(TermEvent::Key(crossterm::event::KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        out
    }

    static SPAWNING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[tokio::test]
    async fn a_turn_typed_at_the_real_loop_reaches_the_adapter_and_comes_back() {
        let _guard = SPAWNING.lock().await;
        // The loop end to end with no terminal: a key stream in, a scripted
        // adapter on the far side of a real pipe, and the reply landing in
        // the transcript. Everything between is the shipped code.
        let mut h = Headless::new("connected,await,delta:the reply,complete").await;
        let _ = h.drive(typed("hello")).await;

        let said: Vec<String> = h
            .app
            .pane()
            .transcript
            .iter()
            .map(|e| e.text.clone())
            .collect();
        assert!(
            said.iter().any(|t| t == "hello"),
            "the typed turn is not in the transcript: {said:?}"
        );
        assert!(
            said.iter().any(|t| t.contains("the reply")),
            "the adapter's reply never arrived: {said:?}"
        );
    }

    #[tokio::test]
    async fn an_adapter_that_wedges_mid_reply_ends_the_session_and_says_so() {
        let _guard = SPAWNING.lock().await;
        // The wiring this whole exercise existed to reach. A turn starts,
        // output begins, and then the adapter stops -- which is the case a
        // single per-turn deadline could not tell from a slow model, and the
        // one `SilenceWatch` splits out.
        //
        // The fake stalls far longer than the five-second threshold, so if
        // the wedge never fires this test hangs rather than passing.
        let mut h = Headless::new("connected,await,delta:half a rep,stall:60000").await;
        let err = h
            .drive(typed("hello"))
            .await
            .expect_err("a wedged adapter must not be a clean exit");

        let shown = err.to_string();
        assert!(
            shown.contains("stopped responding"),
            "the reason must name a wedge rather than a crash: {shown}"
        );
    }

    #[tokio::test]
    async fn an_adapter_that_exits_mid_reply_is_reported_as_death_not_as_a_wedge() {
        let _guard = SPAWNING.lock().await;
        // The partner: both end the session non-zero, so "it ended" proves
        // nothing about which happened. The reason has to tell them apart,
        // because they send someone looking in different places.
        let mut h = Headless::new("connected,await,delta:half a rep,exit:1").await;
        let err = h
            .drive(typed("hello"))
            .await
            .expect_err("a dead adapter must not be a clean exit");

        let shown = err.to_string();
        assert!(shown.contains("exited"), "expected a death, got {shown:?}");
        assert!(
            !shown.contains("stopped responding"),
            "a crash was reported as a wedge: {shown}"
        );
    }

    /// Queues a message behind a running turn, then has the model ask a
    /// question and the response complete -- the real sequence, driven
    /// through `apply` rather than by setting `status` by hand, because
    /// setting it by hand is what let the `Connected` bug survive four
    /// commits of review.
    fn queued_behind_a_question() -> (
        App,
        mpsc::UnboundedSender<Command>,
        mpsc::UnboundedReceiver<Command>,
        transcript::Log,
    ) {
        let (tx, rx) = mpsc::unbounded_channel();
        let log = transcript::Log::open(std::path::Path::new("/dev/null/nope"));
        let mut app = App::new(LANE, "b0");
        // The adapter connects, then the user sends a turn and queues behind
        // it -- the real order, so the pane is genuinely `Up` with one
        // request outstanding rather than merely arranged to look that way.
        app.apply_transport(Transport::Connected);
        app.pane_mut().sent_request();
        app.pane_mut().queue.push("the queued one".to_owned());
        app.park_ask(LANE, pending("call_1"));
        app.apply(
            LANE,
            match finished_frame() {
                IncomingFrame::Event { event, .. } => event,
                _ => unreachable!(),
            },
        );
        (app, tx, rx, log)
    }

    #[test]
    fn a_queued_message_waits_while_a_question_is_on_screen() {
        // `response.completed` is terminal, so the pane really is `Ready`
        // here with the panel still up -- that is the state the bug lives in,
        // and it is asserted rather than assumed so the test cannot pass by
        // failing to reach it.
        let (mut app, tx, mut rx, mut log) = queued_behind_a_question();
        assert!(
            app.pane().status() == Status::Ready,
            "the state under test was not reached"
        );
        assert!(app.panel_open(), "the question should still be on screen");

        drain_queues(&mut app, &tx, &mut log);

        assert!(
            rx.try_recv().is_err(),
            "dispatched a turn under an open question"
        );
        assert_eq!(
            app.pane().queue,
            vec!["the queued one".to_owned()],
            "the queued message must still be queued"
        );
    }

    #[test]
    fn answering_the_question_does_not_release_the_queue_while_the_turn_continues() {
        // Answering sends a tool result, and the model then keeps working on
        // the same turn. From the user's side nothing has finished, so their
        // queued message must still wait -- releasing it here would reorder
        // their own messages into a turn that is still running, which is the
        // bug this whole sequence exists to prevent, one step later.
        let (mut app, tx, mut rx, mut log) = queued_behind_a_question();
        for (call_id, answer) in app.submit_panel() {
            resolve_ask(&mut app, &tx, call_id, answer);
        }
        assert!(!app.panel_open(), "answering should close the panel");
        assert!(
            app.pane().status() == Status::Waiting,
            "a tool result is a request outstanding, so the pane is waiting again"
        );

        drain_queues(&mut app, &tx, &mut log);

        let sent: Vec<Command> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(
            sent.iter().all(|c| matches!(c, Command::ToolResult { .. })),
            "something other than the tool result went out: {} commands",
            sent.len()
        );
        assert_eq!(
            app.pane().queue,
            vec!["the queued one".to_owned()],
            "the queue was released"
        );
    }

    #[test]
    fn the_queued_message_goes_once_the_continuation_finishes() {
        // The partner to both of the above. "It did not dispatch" is
        // satisfied perfectly by a queue that never drains at all, so the
        // release has to be shown to work -- otherwise the fix could be a
        // permanent stall and every other assertion here would still pass.
        let (mut app, tx, mut rx, mut log) = queued_behind_a_question();
        for (call_id, answer) in app.submit_panel() {
            resolve_ask(&mut app, &tx, call_id, answer);
        }
        // The continuation the answer triggered now ends, through `apply`
        // rather than by setting the status by hand.
        app.apply(
            LANE,
            match finished_frame() {
                IncomingFrame::Event { event, .. } => event,
                _ => unreachable!(),
            },
        );

        drain_queues(&mut app, &tx, &mut log);

        let sent: Vec<Command> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(
            sent.iter()
                .any(|c| matches!(c, Command::Send { text, .. } if text == "the queued one")),
            "the queued message never went: {} commands sent",
            sent.len()
        );
        assert!(app.pane().queue.is_empty(), "it should have left the queue");
    }

    /// Per-test directory: tests run in parallel in one process, so a shared
    /// path makes them clobber each other non-deterministically.
    fn log_root(name: &str) -> std::path::PathBuf {
        let base = std::env::temp_dir().join(format!("kobold-log-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).expect("a temp dir");
        base
    }

    fn logged_records(root: &std::path::Path) -> Vec<(String, usize, String)> {
        let path = root.join(transcript::DIR).join(transcript::FILE);
        let text = std::fs::read_to_string(path).unwrap_or_default();
        text.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                let r: transcript::Record<'_> =
                    kobold::json::from_slice(l.as_bytes()).expect("a record we wrote");
                (r.role.to_string(), r.seq, r.text.to_string())
            })
            .collect()
    }

    #[tokio::test]
    async fn a_completed_turn_is_written_to_the_log_once_and_numbered_in_order() {
        // Every other test here opens the log on an unopenable path, where it
        // degrades to silence by design -- so the whole persistence block ran
        // uncovered. A real file is the only thing that can see it.
        let root = log_root("ordered");
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut log = transcript::Log::open(&root);
        let mcp = tools::Sources::new();
        let mut app = App::new(LANE, "b0");
        app.apply_transport(Transport::Connected);

        for text in ["first reply", "second reply"] {
            app.push(Who::Model, text.to_owned());
            absorb(&mut app, &tx, &mut log, &mcp, finished_frame()).await;
        }

        // The sequence numbers are the assertion, not just the presence of
        // two lines: `seq` is read from the pane's `logged` counter, so a
        // counter that moved the wrong way or not at all writes two records
        // that both claim to be the same message of the branch, and
        // reconstructing the branch later silently loses one.
        assert_eq!(
            logged_records(&root),
            vec![
                ("model".to_owned(), 0, "first reply".to_owned()),
                ("model".to_owned(), 1, "second reply".to_owned()),
            ]
        );
    }

    #[tokio::test]
    async fn a_turn_that_ended_on_something_other_than_model_text_is_not_logged_as_a_reply() {
        // The partner to the test above, and the reason the filter is there:
        // a turn can complete with the user's own message last -- an empty
        // reply, or one interrupted before any text arrived. Logging that
        // would put the user's words in the transcript a second time, under
        // the model's role.
        let root = log_root("not-a-reply");
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut log = transcript::Log::open(&root);
        let mcp = tools::Sources::new();
        let mut app = App::new(LANE, "b0");
        app.apply_transport(Transport::Connected);
        app.push(Who::User, "what the user said".to_owned());

        absorb(&mut app, &tx, &mut log, &mcp, finished_frame()).await;
        assert!(
            logged_records(&root).is_empty(),
            "the user's own message was logged as a reply"
        );
    }

    #[tokio::test]
    async fn a_completion_for_a_lane_no_pane_owns_is_logged_nowhere() {
        // A split closed mid-turn. The lane lookup is what stops this, and a
        // comparison flipped there would write the turn against whichever
        // pane happened not to match -- the same class as the empty-lane bug
        // that `Update::lane()` no longer makes possible.
        let root = log_root("orphan-lane");
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut log = transcript::Log::open(&root);
        let mcp = tools::Sources::new();
        let mut app = App::new(LANE, "b0");
        app.apply_transport(Transport::Connected);
        app.push(Who::Model, "text on the surviving pane".to_owned());

        absorb(
            &mut app,
            &tx,
            &mut log,
            &mcp,
            finished_frame_for("a-lane-nobody-owns"),
        )
        .await;
        assert!(
            logged_records(&root).is_empty(),
            "a turn was logged against a pane that does not run that lane"
        );
    }

    /// A remote source that answers from memory, standing in for an MCP
    /// server. Deliberately owns a name no local tool has, so reaching it is
    /// the only way the assertion below can pass.
    struct FakeMcp;

    #[async_trait::async_trait]
    impl tools::McpTools for FakeMcp {
        fn owns(&self, name: &str) -> bool {
            name.starts_with("files__")
        }
        async fn call(&self, call: &tools::Call) -> tools::Outcome {
            tools::Outcome::Done(format!("{} answered", call.name))
        }
        fn schemas(&self) -> Vec<(String, String, String)> {
            vec![(
                "files__grep".to_owned(),
                "search".to_owned(),
                "{}".to_owned(),
            )]
        }
    }

    #[tokio::test]
    async fn a_configured_mcp_server_is_reachable_from_a_tool_call() {
        // `absorb` hands `tools::run` a source only when one is configured,
        // so that an unused MCP path costs nothing and is not a second way to
        // reach a local tool. That guard had no test, and the shape of the
        // mistake is quiet in both directions: inverted, a configured server
        // becomes unreachable and every remote call comes back refused, with
        // nothing on screen to say the server was never asked.
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut log = transcript::Log::open(std::path::Path::new("/dev/null/nope"));
        let mut mcp = tools::Sources::new();
        mcp.push(Box::new(FakeMcp));
        let mut app = App::new(LANE, "b0");
        app.apply_transport(Transport::Connected);

        // All three frames: a call is not a call until `TOOL_CALL_END`, and
        // driving only the start would assert about half a fact.
        for frame in tool_call_frames("c1", "files__grep", "{}") {
            absorb(&mut app, &tx, &mut log, &mcp, frame).await;
        }

        let sent: Vec<Command> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        let output = sent.iter().find_map(|c| match c {
            Command::ToolResult { output, .. } => Some(output.clone()),
            _ => None,
        });
        // The server's own words, not merely that something came back: a
        // refusal is also a `ToolResult`, so asserting one was sent would
        // pass with the server never consulted at all.
        assert_eq!(output.as_deref(), Some("files__grep answered"));
    }

    #[tokio::test]
    async fn an_automatic_tool_call_keeps_the_pane_waiting_through_its_continuation() {
        // The case a flag could not express, and the common path: every file
        // read and every MCP call. The tool result goes out, and only then
        // does the `response.completed` for the response that requested it
        // arrive -- which used to set the pane idle and release the queue
        // into a turn still in progress.
        //
        // Driven through `absorb` so the tool really runs and the result
        // really goes out, rather than by arranging the state by hand.
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut log = transcript::Log::open(std::path::Path::new("/dev/null/nope"));
        let mcp = tools::Sources::new();
        let mut app = App::new(LANE, "b0");
        app.apply_transport(Transport::Connected);
        app.pane_mut().sent_request();
        app.pane_mut().queue.push("the queued one".to_owned());

        for frame in tool_call_frames("c1", "file_read", r#"{"path":"definitely-not-here.txt"}"#) {
            absorb(&mut app, &tx, &mut log, &mcp, frame).await;
        }
        let sent: Vec<Command> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(
            sent.iter().any(|c| matches!(c, Command::ToolResult { .. })),
            "the tool result should have gone out -- a refusal is still a result"
        );

        // **The ordering invariant, stated here because it is currently
        // accidental and is now on the common path.** Under AG-UI every tool
        // call ends a run, so a `RUN_FINISHED` follows every `TOOL_CALL_END`
        // -- and the count only stays right because `absorb` awaits
        // `tools::run` before the next frame is applied, and the event loop
        // handles frames one at a time. A `RUN_FINISHED` sitting behind a
        // `TOOL_CALL_END` therefore cannot land until the result has gone out
        // and the count has been raised.
        //
        // Make the frames concurrent, or apply a burst in one batch, and the
        // pane reads idle in the gap: the spinner stops, the silence watch
        // goes blind, and a queued message dispatches onto a lane whose tool
        // call is unanswered. This assertion is the gap.
        assert_eq!(
            app.pane().status(),
            Status::Waiting,
            "the pane read idle between the tool result going out and the run ending"
        );

        // Now the response that made the call completes. Two requests were
        // outstanding; one settles, and the pane is still waiting on the
        // other.
        absorb(&mut app, &tx, &mut log, &mcp, finished_frame()).await;
        assert!(
            app.pane().status() == Status::Waiting,
            "the tool result is still outstanding, so the pane is not idle"
        );
        let sent: Vec<Command> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(
            !sent.iter().any(|c| matches!(c, Command::Send { .. })),
            "the queued message dispatched into a turn still in progress"
        );

        // And when the continuation finishes, it goes.
        absorb(&mut app, &tx, &mut log, &mcp, finished_frame()).await;
        let sent: Vec<Command> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(
            sent.iter()
                .any(|c| matches!(c, Command::Send { text, .. } if text == "the queued one")),
            "the queue never released: {} commands",
            sent.len()
        );
    }

    #[test]
    fn a_configured_adapter_is_used_verbatim_and_nothing_else_is_searched_for() {
        // The setting exists because the whole point of a separate process is
        // that it can be a different one. An override that falls through to
        // the bundled adapter would silently run OpenAI whatever the user
        // asked for.
        let cfg = settings::Settings {
            adapter: "/opt/some/other-adapter".to_owned(),
            ..Default::default()
        };
        assert_eq!(adapter_binary(&cfg), "/opt/some/other-adapter");
    }

    #[test]
    fn with_no_setting_the_bundled_adapter_is_named() {
        // Whatever the search finds, it must end in the adapter's own name --
        // an empty string or some unrelated path would fail to spawn with a
        // message that explains nothing.
        let cfg = settings::Settings::default();
        let found = adapter_binary(&cfg);
        assert!(
            found.ends_with(DEFAULT_ADAPTER),
            "resolved to {found:?}, which is not the bundled adapter"
        );
        assert!(!found.is_empty());
    }

    /// Everything `handle_key` needs, with nothing real behind it.
    ///
    /// No production change was required to get here. `Log::open` on an
    /// unopenable path degrades to a silent no-op by design -- built for
    /// read-only checkouts -- so it is already the fake this needs;
    /// `speaker` is an `Option` the caller may leave empty; and `commands`
    /// is a real channel whose receiving end can be drained to assert on
    /// what was sent.
    struct Keys {
        app: App,
        tx: mpsc::UnboundedSender<Command>,
        rx: mpsc::UnboundedReceiver<Command>,
        log: transcript::Log,
        speaker: Option<Box<dyn tts::StreamingTts>>,
    }

    impl Keys {
        fn new() -> Self {
            let (tx, rx) = mpsc::unbounded_channel();
            // A path that cannot be a directory, so the log opens to `None`.
            let log = transcript::Log::open(std::path::Path::new("/dev/null/nope"));
            let mut app = App::new("main", "b0");
            app.apply_transport(Transport::Connected);
            Keys {
                app,
                tx,
                rx,
                log,
                speaker: None,
            }
        }

        /// One key, with modifiers, through the real entry point.
        fn press(&mut self, code: KeyCode, mods: KeyModifiers) -> bool {
            let ev = TermEvent::Key(crossterm::event::KeyEvent::new(code, mods));
            handle_key(
                &mut self.app,
                &self.tx,
                &mut self.log,
                &mut self.speaker,
                ev,
            )
        }

        fn key(&mut self, code: KeyCode) -> bool {
            self.press(code, KeyModifiers::NONE)
        }

        fn ctrl(&mut self, c: char) -> bool {
            self.press(KeyCode::Char(c), KeyModifiers::CONTROL)
        }

        fn typed(&mut self, text: &str) {
            for ch in text.chars() {
                self.press(KeyCode::Char(ch), KeyModifiers::NONE);
            }
        }

        fn input(&self) -> String {
            self.app.pane().input.clone()
        }

        fn caret(&self) -> usize {
            self.app.pane().cursor
        }

        /// What reached the network, if anything.
        fn sent(&mut self) -> Vec<Command> {
            let mut out = Vec::new();
            while let Ok(c) = self.rx.try_recv() {
                out.push(c);
            }
            out
        }
    }

    /// Step 10 of the cascade: the prompt's own bindings, one row per key.
    ///
    /// Table-driven because the failure mode being guarded is a binding
    /// quietly stopping at the wrong step of the cascade, and that is a
    /// property of the whole set rather than of any one key. A row here is a
    /// starting line, a key, and what the line should look like afterwards --
    /// so a binding that stops firing shows up as its own named row rather
    /// than as one assertion inside a longer test.
    #[test]
    fn every_prompt_binding_edits_the_line_it_claims_to() {
        let n = KeyModifiers::NONE;
        let c = KeyModifiers::CONTROL;
        let a = KeyModifiers::ALT;
        // (name, starting text, caret, key, modifiers, expected text, expected caret)
        let table: &[(&str, &str, usize, KeyCode, KeyModifiers, &str, usize)] = &[
            (
                "left walks one character",
                "abc",
                3,
                KeyCode::Left,
                n,
                "abc",
                2,
            ),
            (
                "right walks one character",
                "abc",
                0,
                KeyCode::Right,
                n,
                "abc",
                1,
            ),
            (
                "ctrl+left walks a word",
                "one two",
                7,
                KeyCode::Left,
                c,
                "one two",
                4,
            ),
            (
                "alt+left walks a word too",
                "one two",
                7,
                KeyCode::Left,
                a,
                "one two",
                4,
            ),
            (
                "ctrl+right walks a word",
                "one two",
                0,
                KeyCode::Right,
                c,
                "one two",
                3,
            ),
            (
                "alt+right walks a word too",
                "one two",
                0,
                KeyCode::Right,
                a,
                "one two",
                3,
            ),
            (
                "alt+b is word-left",
                "one two",
                7,
                KeyCode::Char('b'),
                a,
                "one two",
                4,
            ),
            (
                "alt+f is word-right",
                "one two",
                0,
                KeyCode::Char('f'),
                a,
                "one two",
                3,
            ),
            (
                "home goes to the start",
                "abc",
                3,
                KeyCode::Home,
                n,
                "abc",
                0,
            ),
            ("end goes to the end", "abc", 0, KeyCode::End, n, "abc", 3),
            ("ctrl+a is home", "abc", 3, KeyCode::Char('a'), c, "abc", 0),
            ("ctrl+e is end", "abc", 0, KeyCode::Char('e'), c, "abc", 3),
            (
                "ctrl+u kills to the start",
                "one two",
                4,
                KeyCode::Char('u'),
                c,
                "two",
                0,
            ),
            (
                "ctrl+k kills to the end",
                "one two",
                3,
                KeyCode::Char('k'),
                c,
                "one",
                3,
            ),
            (
                "backspace deletes behind",
                "abc",
                3,
                KeyCode::Backspace,
                n,
                "ab",
                2,
            ),
            (
                "alt+backspace deletes a word",
                "one two",
                7,
                KeyCode::Backspace,
                a,
                "one ",
                4,
            ),
            (
                "delete removes ahead",
                "abc",
                0,
                KeyCode::Delete,
                n,
                "bc",
                0,
            ),
            (
                "shift+enter is a newline",
                "ab",
                2,
                KeyCode::Enter,
                KeyModifiers::SHIFT,
                "ab\n",
                3,
            ),
            (
                "alt+enter is a newline too",
                "ab",
                2,
                KeyCode::Enter,
                a,
                "ab\n",
                3,
            ),
            (
                "ctrl+j is a newline too",
                "ab",
                2,
                KeyCode::Char('j'),
                c,
                "ab\n",
                3,
            ),
            // The bare letters that carry a modifier binding. Each must type
            // itself when pressed alone -- a guard that decayed to
            // always-true would silently steal the letter and run its
            // binding, which no assertion on the modified form can see.
            ("plain a types", "x", 1, KeyCode::Char('a'), n, "xa", 2),
            ("plain e types", "x", 1, KeyCode::Char('e'), n, "xe", 2),
            ("plain u types", "x", 1, KeyCode::Char('u'), n, "xu", 2),
            ("plain k types", "x", 1, KeyCode::Char('k'), n, "xk", 2),
            ("plain b types", "x", 1, KeyCode::Char('b'), n, "xb", 2),
            ("plain f types", "x", 1, KeyCode::Char('f'), n, "xf", 2),
            ("plain j types", "x", 1, KeyCode::Char('j'), n, "xj", 2),
            ("plain d types", "x", 1, KeyCode::Char('d'), n, "xd", 2),
            ("plain w types", "x", 1, KeyCode::Char('w'), n, "xw", 2),
            ("plain c types", "x", 1, KeyCode::Char('c'), n, "xc", 2),
            (
                "plain enter with no modifier is not a newline",
                "",
                0,
                KeyCode::Enter,
                n,
                "",
                0,
            ),
        ];

        for &(name, start, at, code, mods, want, want_caret) in table {
            let mut k = Keys::new();
            k.app.pane_mut().set_input(start.to_owned());
            k.app.pane_mut().cursor = at;
            k.press(code, mods);
            assert_eq!(k.input(), want, "{name}: wrong text");
            assert_eq!(k.caret(), want_caret, "{name}: wrong caret");
            // Nothing on this table should reach the network: every row is a
            // line edit. Without this, a binding that accidentally sent the
            // line would still pass its text assertion on the row that
            // happens to leave the text unchanged.
            assert!(k.sent().is_empty(), "{name}: should not have sent anything");
        }
    }

    #[test]
    fn typing_inserts_and_enter_sends_what_was_typed() {
        let mut k = Keys::new();
        k.typed("hello");
        assert_eq!(k.input(), "hello");
        assert!(k.sent().is_empty(), "nothing goes out until Enter");

        assert!(k.key(KeyCode::Enter));
        assert_eq!(k.input(), "", "the prompt clears once the turn is away");
        let sent = k.sent();
        assert_eq!(sent.len(), 1, "exactly one turn");
        match &sent[0] {
            Command::Send { text, .. } => assert_eq!(text, "hello"),
            _ => panic!("expected a Send"),
        }
    }

    #[test]
    fn enter_on_an_empty_or_blank_prompt_sends_nothing() {
        // The counter-assertion to the test above: "nothing was sent" has to
        // be distinguishable from "the key did nothing at all", so the
        // repaint answer is checked too.
        let mut k = Keys::new();
        assert!(
            !k.key(KeyCode::Enter),
            "an empty prompt should not even repaint"
        );
        assert!(k.sent().is_empty());

        k.typed("   ");
        assert!(!k.key(KeyCode::Enter), "whitespace is not a message");
        assert!(k.sent().is_empty());
        assert_eq!(k.input(), "   ", "and the line is left alone");
    }

    /// Step 5: the panel takes the keyboard whole while it is up.
    ///
    /// Deliberately the same table as the prompt test above, asserted from
    /// the other side. The duplication is the point: the pair is the
    /// precedence property, checked binding by binding, and folding them into
    /// one parameterised test would hide which side each row is asserting.
    ///
    /// Every row asserts both halves. That the key had its effect *on the
    /// panel*, and that the prompt behind it is untouched -- because a
    /// deleted panel arm satisfies the second half all by itself. A key that
    /// did nothing at all would leave the prompt just as pristine as a key
    /// correctly handled by the panel, so the negative assertion alone
    /// measures nothing.
    #[test]
    fn every_editing_key_lands_on_an_open_panel_and_never_on_the_prompt() {
        let n = KeyModifiers::NONE;
        let ctrl = KeyModifiers::CONTROL;
        let alt = KeyModifiers::ALT;
        // (name, start the caret at 0?, key, modifiers, expected text, caret)
        //
        // The caret column exists because the forward-moving keys are
        // no-ops at the end of the line -- and a no-op is exactly what a
        // deleted arm produces, so testing them from the end would assert
        // nothing at all.
        let table: &[(&str, bool, KeyCode, KeyModifiers, &str, usize)] = &[
            (
                "left walks one character",
                false,
                KeyCode::Left,
                n,
                "one two",
                6,
            ),
            (
                "right walks one character",
                true,
                KeyCode::Right,
                n,
                "one two",
                1,
            ),
            (
                "ctrl+left walks a word",
                false,
                KeyCode::Left,
                ctrl,
                "one two",
                4,
            ),
            (
                "alt+left walks a word too",
                false,
                KeyCode::Left,
                alt,
                "one two",
                4,
            ),
            (
                "ctrl+right walks a word",
                true,
                KeyCode::Right,
                ctrl,
                "one two",
                3,
            ),
            (
                "alt+right walks a word too",
                true,
                KeyCode::Right,
                alt,
                "one two",
                3,
            ),
            (
                "alt+b is word-left",
                false,
                KeyCode::Char('b'),
                alt,
                "one two",
                4,
            ),
            (
                "alt+f is word-right",
                true,
                KeyCode::Char('f'),
                alt,
                "one two",
                3,
            ),
            (
                "home goes to the start",
                false,
                KeyCode::Home,
                n,
                "one two",
                0,
            ),
            ("end goes to the end", true, KeyCode::End, n, "one two", 7),
            (
                "ctrl+a is home",
                false,
                KeyCode::Char('a'),
                ctrl,
                "one two",
                0,
            ),
            (
                "ctrl+e is end",
                true,
                KeyCode::Char('e'),
                ctrl,
                "one two",
                7,
            ),
            (
                "ctrl+u kills to the start",
                false,
                KeyCode::Char('u'),
                ctrl,
                "",
                0,
            ),
            (
                "ctrl+k kills to the end",
                true,
                KeyCode::Char('k'),
                ctrl,
                "",
                0,
            ),
            (
                "backspace deletes behind",
                false,
                KeyCode::Backspace,
                n,
                "one tw",
                6,
            ),
            (
                "alt+backspace deletes a word",
                false,
                KeyCode::Backspace,
                alt,
                "one ",
                4,
            ),
            (
                "delete removes ahead",
                true,
                KeyCode::Delete,
                n,
                "ne two",
                0,
            ),
            (
                "a plain character types",
                false,
                KeyCode::Char('z'),
                n,
                "one twoz",
                8,
            ),
            ("a digit types", false, KeyCode::Char('2'), n, "one two2", 8),
            ("space types", false, KeyCode::Char(' '), n, "one two ", 8),
            // Same reasoning as the prompt table: a bare letter carrying a
            // modifier binding must type itself.
            ("plain a types", false, KeyCode::Char('a'), n, "one twoa", 8),
            ("plain e types", false, KeyCode::Char('e'), n, "one twoe", 8),
            ("plain u types", false, KeyCode::Char('u'), n, "one twou", 8),
            ("plain k types", false, KeyCode::Char('k'), n, "one twok", 8),
            ("plain b types", false, KeyCode::Char('b'), n, "one twob", 8),
            ("plain f types", false, KeyCode::Char('f'), n, "one twof", 8),
        ];

        for &(name, from_start, code, mods, want, want_caret) in table {
            let mut k = Keys::new();
            // A half-written message behind the panel: this is what must not
            // be disturbed, and an empty prompt could not detect it.
            k.app.pane_mut().set_input("half written".to_owned());
            k.app.park_ask("main", pending("call_1"));
            // Onto the free-text row, with something in it, so the editing
            // keys have text to act on -- exactly the prompt table's setup.
            k.key(KeyCode::Down);
            k.typed("one two");
            if from_start {
                k.key(KeyCode::Home);
            }

            k.press(code, mods);

            assert_eq!(
                k.app.panel_answer_text().as_deref(),
                Some(want),
                "{name}: wrong text on the panel"
            );
            assert_eq!(
                k.app.panel().and_then(|p| p.text_caret()),
                Some(want_caret),
                "{name}: wrong caret on the panel"
            );
            assert_eq!(
                k.input(),
                "half written",
                "{name}: reached the prompt behind the panel"
            );
            assert_eq!(k.caret(), 12, "{name}: moved the prompt's caret");
            assert!(k.app.panel_open(), "{name}: closed the panel");
            assert!(k.sent().is_empty(), "{name}: sent something");
        }
    }

    /// The keys whose effect on a panel is to move between its rows rather
    /// than to edit a field, so they need a different observable.
    /// `panel_answer_text` is `Some` only while the highlight is on the
    /// free-text row, which makes it a direct read of where the highlight is.
    #[test]
    fn the_arrows_walk_an_open_panels_rows_and_never_the_prompts_history() {
        let mut k = Keys::new();
        k.app.pane_mut().set_input("half written".to_owned());
        k.app.history.push("an earlier message".to_owned());
        k.app.park_ask("main", pending("call_1"));

        // Opens on the single option, so not on the text row.
        assert_eq!(k.app.panel_answer_text(), None, "opens on the choice row");
        assert!(k.key(KeyCode::Down), "down moves onto the text row");
        assert_eq!(k.app.panel_answer_text().as_deref(), Some(""));
        assert!(k.key(KeyCode::Up), "up moves back off it");
        assert_eq!(k.app.panel_answer_text(), None);

        // The counter-assertion that matters here: these same arrows drive
        // history at the prompt, and history is non-empty, so a leak would
        // be visible rather than silent.
        assert_eq!(
            k.input(),
            "half written",
            "the arrows must not reach history"
        );
    }

    #[test]
    fn typing_into_an_open_panel_reaches_the_panel_not_the_prompt() {
        // The counter-assertion to the table above. Every row there checks
        // that a key did NOT reach the prompt -- which a panel that swallowed
        // everything and did nothing would also satisfy. This checks the
        // other half: the keys genuinely land somewhere.
        let mut k = Keys::new();
        k.app.pane_mut().set_input("half written".to_owned());
        k.app.park_ask("main", pending("call_1"));
        // Onto the free-text row, past the single option.
        k.key(KeyCode::Down);

        k.typed("yes please");
        assert_eq!(
            k.app.panel_answer_text().as_deref(),
            Some("yes please"),
            "the panel's field should have taken the typing"
        );
        assert_eq!(
            k.input(),
            "half written",
            "and the prompt is still untouched"
        );

        // Backspace edits the panel too, rather than the line behind it.
        k.key(KeyCode::Backspace);
        assert_eq!(k.app.panel_answer_text().as_deref(), Some("yes pleas"));
        assert_eq!(k.input(), "half written");
    }

    #[test]
    fn enter_on_a_panel_answers_the_call_rather_than_sending_a_turn() {
        let mut k = Keys::new();
        k.app.pane_mut().set_input("half written".to_owned());
        k.app.park_ask("main", pending("call_1"));

        assert!(k.key(KeyCode::Enter));
        assert!(!k.app.panel_open(), "answering closes the panel");
        assert_eq!(
            k.input(),
            "half written",
            "the queued message is still waiting"
        );

        let sent = k.sent();
        assert_eq!(sent.len(), 1, "one result for the one parked call");
        match &sent[0] {
            Command::ToolResult {
                call_id, output, ..
            } => {
                assert_eq!(call_id, "call_1");
                assert_eq!(output, "yes", "the option the panel opened on");
            }
            _ => panic!("expected a ToolResult, not a turn"),
        }
    }

    #[test]
    fn esc_on_a_panel_declines_every_call_it_was_carrying() {
        let mut k = Keys::new();
        k.app.park_ask("main", pending("call_1"));

        assert!(k.key(KeyCode::Esc));
        assert!(!k.app.panel_open());
        let sent = k.sent();
        assert_eq!(sent.len(), 1);
        match &sent[0] {
            Command::ToolResult {
                call_id, output, ..
            } => {
                assert_eq!(call_id, "call_1");
                assert!(
                    output.contains("declined"),
                    "a decline must say so: {output}"
                );
            }
            _ => panic!("expected a ToolResult"),
        }
    }

    /// Step 9's rule, and the subtlest one in the function: an unbound key
    /// in Chat mode leaves Chat and is then re-dispatched as if it had
    /// arrived at the prompt. Not "leaves Chat", not "types" -- both.
    #[test]
    fn an_unbound_key_in_chat_mode_returns_to_the_prompt_and_still_types() {
        let mut k = Keys::new();
        k.app.push(Who::Model, "an answer");
        k.app.mode = Mode::Chat;
        k.app.pane_mut().selected = Some(0);

        assert!(k.press(KeyCode::Char('q'), KeyModifiers::NONE));

        assert_eq!(k.app.mode, Mode::Send, "an unbound key should leave Chat");
        assert!(
            !k.app.should_quit,
            "and must not quit: ^C's guard is not optional"
        );
        // The half that a plain "left Chat" implementation would drop: the
        // keystroke itself must not be swallowed on the way out, or the
        // first character of everything typed from Chat disappears.
        assert_eq!(k.input(), "q", "the key that left Chat must still be typed");
    }

    #[test]
    fn chat_mode_bindings_do_not_type_themselves_into_the_prompt() {
        // The counter-assertion: 'q' types because it is unbound, so the
        // bound letters must be shown NOT to. Without this, an implementation
        // that re-dispatched everything would pass the test above.
        for bound in ['y', 'r', 'f'] {
            let mut k = Keys::new();
            k.app.push(Who::Model, "an answer");
            k.app.mode = Mode::Chat;
            k.app.pane_mut().selected = Some(0);

            k.press(KeyCode::Char(bound), KeyModifiers::NONE);
            assert_eq!(
                k.input(),
                "",
                "{bound:?} is bound in Chat and must not reach the prompt"
            );
        }
    }

    #[test]
    fn esc_leaves_chat_mode_without_falling_through_to_the_prompts_esc() {
        // Both the real arm and the unbound fall-through leave Chat, so mode
        // alone cannot tell them apart. A turn in flight can: the prompt's
        // Esc arms an interrupt, and Chat's must not -- otherwise leaving
        // Chat would silently leave the session one keypress from killing a
        // running turn.
        let mut k = Keys::new();
        k.app.push(Who::Model, "an answer");
        k.app.pane_mut().sent_request();
        k.app.mode = Mode::Chat;

        assert!(k.key(KeyCode::Esc));
        assert_eq!(k.app.mode, Mode::Send, "Esc leaves Chat");
        assert_eq!(k.input(), "", "and types nothing");
        assert!(
            !k.app.esc_armed,
            "Chat's Esc must not fall through to the prompt's, which would arm an interrupt"
        );
    }

    /// Steps 6 and 7: pane management, which takes its keys before any
    /// editing binding sees them.
    #[test]
    fn ctrl_w_closes_a_pane_and_shift_arrows_move_between_them() {
        let mut k = Keys::new();
        assert_eq!(k.app.panes.len(), 1);
        // With one pane there is nothing to close or move to, and both must
        // say so rather than pretending.
        assert!(!k.ctrl('w'), "closing the only pane does nothing");
        assert!(
            !k.press(KeyCode::Left, KeyModifiers::SHIFT),
            "there is nowhere to move focus to"
        );

        // A second pane the way one actually appears: forking a selected
        // entry. Constructing a `Pane` directly is not reachable from here,
        // and going through `fork` exercises the real path anyway.
        k.app.push(Who::Model, "an answer");
        k.app.pane_mut().selected = Some(0);
        assert!(k.app.fork(), "fork should open a second pane");
        assert_eq!(k.app.panes.len(), 2);
        k.app.active = 0;

        // A third pane, because with only two, left and right both toggle
        // between the same pair -- the direction is unobservable, so a
        // reversed comparison would pass unnoticed.
        k.app.active = 0;
        k.app.pane_mut().selected = Some(0);
        assert!(k.app.fork(), "and a third");
        assert_eq!(k.app.panes.len(), 3);
        k.app.active = 0;

        assert!(
            k.press(KeyCode::Right, KeyModifiers::SHIFT),
            "focus should move"
        );
        assert_eq!(k.app.active, 1, "right moves forward");
        assert!(k.press(KeyCode::Right, KeyModifiers::SHIFT));
        assert_eq!(k.app.active, 2, "and keeps going");
        assert!(k.press(KeyCode::Left, KeyModifiers::SHIFT));
        assert_eq!(k.app.active, 1, "left moves back, not forward");
        assert!(k.press(KeyCode::Left, KeyModifiers::SHIFT));
        assert_eq!(k.app.active, 0, "and back to the first");

        assert!(k.ctrl('w'), "now there is a pane to close");
        assert_eq!(k.app.panes.len(), 2);
    }

    /// Step 8: Shift+Up/Down browses the transcript, and must win over the
    /// history bindings on the same arrows.
    #[test]
    fn shift_arrows_browse_the_transcript_rather_than_history() {
        let mut k = Keys::new();
        k.app.push(Who::User, "first");
        k.app.push(Who::Model, "second");

        assert!(k.press(KeyCode::Up, KeyModifiers::SHIFT));
        assert_eq!(k.app.mode, Mode::Chat, "browsing enters Chat mode");
        assert!(k.app.pane().selected.is_some(), "and selects an entry");
        // The counter-assertion: history navigation shares these keys, so
        // check the line was not filled from history instead.
        assert_eq!(k.input(), "", "browsing must not touch the prompt");
    }

    /// Tier 3: the two one-shot arming state machines. Two mutants between
    /// them, but "any other key disarms" is exactly the kind of invariant
    /// that rots -- it is invisible until the day someone quits by accident.
    #[test]
    fn ctrl_d_arms_once_and_any_other_key_disarms_it() {
        let mut k = Keys::new();
        assert!(k.ctrl('d'));
        assert!(k.app.quit_armed, "the first ^D arms");
        assert!(!k.app.should_quit, "but does not quit");

        assert!(k.ctrl('d'));
        assert!(k.app.should_quit, "the second in a row quits");

        // And the disarm, which is the half worth guarding: a stray press
        // must not leave the session one keystroke from exiting.
        let mut k = Keys::new();
        k.ctrl('d');
        assert!(k.app.quit_armed);
        k.press(KeyCode::Char('x'), KeyModifiers::NONE);
        assert!(!k.app.quit_armed, "any other key disarms");
        k.ctrl('d');
        assert!(!k.app.should_quit, "so the next ^D only re-arms");
        assert!(k.app.quit_armed);
    }

    #[test]
    fn esc_arms_an_interrupt_only_while_a_turn_is_in_flight() {
        let mut k = Keys::new();
        k.app.pane_mut().sent_request();

        k.key(KeyCode::Esc);
        assert!(k.app.esc_armed, "the first Esc arms while waiting");
        k.key(KeyCode::Esc);
        assert!(k.app.pane().interrupted, "the second interrupts the turn");

        // Idle, there is nothing to interrupt, so Esc must not arm at all --
        // otherwise a later Esc would interrupt a turn the user never meant
        // to stop.
        let mut k = Keys::new();
        k.key(KeyCode::Esc);
        assert!(!k.app.esc_armed, "an idle Esc has nothing to arm");
    }

    #[test]
    fn esc_arming_is_cleared_by_any_other_key() {
        let mut k = Keys::new();
        k.app.pane_mut().sent_request();
        k.key(KeyCode::Esc);
        assert!(k.app.esc_armed);

        k.press(KeyCode::Char('x'), KeyModifiers::NONE);
        assert!(!k.app.esc_armed, "any other key disarms");
        k.key(KeyCode::Esc);
        assert!(!k.app.pane().interrupted, "so the next Esc only re-arms");
        assert!(k.app.esc_armed);
    }

    /// The event kinds that are not keys at all, which sit above every
    /// binding in the cascade.
    #[test]
    fn paste_goes_to_whichever_field_is_taking_input() {
        let mut k = Keys::new();
        let paste = |k: &mut Keys, text: &str| {
            handle_key(
                &mut k.app,
                &k.tx,
                &mut k.log,
                &mut k.speaker,
                TermEvent::Paste(text.to_owned()),
            )
        };

        assert!(paste(&mut k, "pasted"));
        assert_eq!(k.input(), "pasted");
        // An empty paste is not a repaint: nothing changed on screen.
        assert!(!paste(&mut k, ""), "an empty paste should not cost a frame");
        assert_eq!(k.input(), "pasted");

        // With a panel up it belongs to the field being typed into, not the
        // prompt behind it -- the same precedence the key table asserts.
        let mut k = Keys::new();
        k.app.pane_mut().set_input("half written".to_owned());
        k.app.park_ask("main", pending("call_1"));
        k.key(KeyCode::Down);
        assert!(paste(&mut k, "answer"));
        assert_eq!(k.app.panel_answer_text().as_deref(), Some("answer"));
        assert_eq!(k.input(), "half written", "the prompt behind is untouched");
    }

    #[test]
    fn a_resize_repaints_and_a_key_release_does_not() {
        let mut k = Keys::new();
        assert!(
            handle_key(
                &mut k.app,
                &k.tx,
                &mut k.log,
                &mut k.speaker,
                TermEvent::Resize(80, 24)
            ),
            "a resize has to repaint"
        );

        // Windows terminals send press and release for every key; acting on
        // both would type everything twice.
        let release = TermEvent::Key(crossterm::event::KeyEvent::new_with_kind(
            KeyCode::Char('x'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        ));
        assert!(!handle_key(
            &mut k.app,
            &k.tx,
            &mut k.log,
            &mut k.speaker,
            release
        ));
        assert_eq!(k.input(), "", "a release must not type");
    }

    /// The arrows at the prompt drive history, and the slash menu takes them
    /// away while it is open. Two bindings on one pair of keys, so each has
    /// to be shown working *and* shown not to fire in the other's case.
    #[test]
    fn the_arrows_walk_history_unless_the_slash_menu_has_them() {
        let mut k = Keys::new();
        k.app.history.push("first message".to_owned());
        k.app.history.push("second message".to_owned());

        assert!(k.key(KeyCode::Up), "up should reach back into history");
        assert_eq!(k.input(), "second message", "most recent first");
        assert!(k.key(KeyCode::Up));
        assert_eq!(k.input(), "first message");
        assert!(k.key(KeyCode::Down), "down comes forward again");
        assert_eq!(k.input(), "second message");

        // With the menu open the same keys move the highlight instead, and
        // must not rewrite the line -- which is what makes this observable.
        let mut k = Keys::new();
        k.app.history.push("an earlier message".to_owned());
        k.typed("/");
        assert!(k.app.menu_open(), "a leading slash opens the menu");
        let line = k.input();
        k.key(KeyCode::Up);
        assert_eq!(k.input(), line, "the menu owns Up, history must not fire");
        assert!(k.app.menu_open(), "and the menu is still up");
        k.key(KeyCode::Down);
        assert_eq!(k.input(), line, "the menu owns Down too");
        assert!(k.app.menu_open());
        // Down at the live draft would clear the line if history had it, so
        // press it again from the same state to pin that specifically.
        k.key(KeyCode::Down);
        assert_eq!(k.input(), line, "still the menu's");
    }

    #[test]
    fn tab_completes_the_highlighted_suggestion_and_does_nothing_otherwise() {
        let mut k = Keys::new();
        // No menu open: a tab character in a prompt is never what was meant.
        k.typed("hello");
        assert!(
            !k.key(KeyCode::Tab),
            "tab with nothing to complete is not a repaint"
        );
        assert_eq!(k.input(), "hello", "and inserts nothing");

        let mut k = Keys::new();
        k.typed("/");
        assert!(k.app.menu_open());
        assert!(
            k.key(KeyCode::Tab),
            "tab should complete the highlighted suggestion"
        );
        assert!(
            k.input().len() > 1,
            "completion should have filled the line, got {:?}",
            k.input()
        );
    }

    #[test]
    fn page_keys_scroll_the_transcript_without_editing_the_line() {
        let mut k = Keys::new();
        for i in 0..40 {
            k.app.push(Who::Model, format!("entry {i}"));
        }
        k.typed("half written");

        assert!(k.key(KeyCode::PageUp));
        assert_eq!(k.app.pane().scroll, 10, "page up scrolls back");
        assert!(k.key(KeyCode::PageUp));
        assert_eq!(k.app.pane().scroll, 20);
        assert!(k.key(KeyCode::PageDown));
        assert_eq!(k.app.pane().scroll, 10, "and page down comes forward");
        // Saturating, so the bottom holds rather than wrapping.
        k.key(KeyCode::PageDown);
        k.key(KeyCode::PageDown);
        assert_eq!(
            k.app.pane().scroll,
            0,
            "scrolling past the bottom stops there"
        );

        assert_eq!(
            k.input(),
            "half written",
            "scrolling must not touch the line"
        );
    }

    #[test]
    fn ctrl_c_quits_from_the_prompt_from_chat_mode_and_from_a_panel() {
        // One binding, reached through three different steps of the cascade,
        // so each needs its own case.
        let mut k = Keys::new();
        assert!(k.ctrl('c'));
        assert!(k.app.should_quit, "from the prompt");

        let mut k = Keys::new();
        k.app.push(Who::Model, "an answer");
        k.app.mode = Mode::Chat;
        assert!(k.ctrl('c'));
        assert!(k.app.should_quit, "from Chat mode");
        // The unbound fall-through would also quit, by leaving Chat and
        // re-dispatching into the prompt's own ^C -- so the quit alone does
        // not prove Chat handled it. Staying in Chat is what does.
        assert_eq!(
            k.app.mode,
            Mode::Chat,
            "^C is bound in Chat, so it does not leave it"
        );

        let mut k = Keys::new();
        k.app.park_ask("main", pending("call_1"));
        assert!(k.ctrl('c'));
        assert!(k.app.should_quit, "and from under an open panel");
    }

    /// A question with several options, so the panel has more than two rows.
    fn multi(call_id: &str, multiple: bool) -> tools::Ask {
        tools::Ask {
            call_id: call_id.to_owned(),
            question: "which?".to_owned(),
            options: vec!["a".to_owned(), "b".to_owned(), "c".to_owned()],
            multiple,
        }
    }

    #[test]
    fn the_panels_arrows_move_in_the_direction_they_name() {
        // Three options plus the free-text row, because on a two-row panel
        // up and down are the same move and a reversed one is invisible.
        // `panel_answer_text` is `Some` only on the text row, which is the
        // last one -- so "one Up from the top" landing there proves the wrap
        // went backwards rather than forwards.
        let mut k = Keys::new();
        k.app.park_ask("main", multi("call_1", false));
        assert_eq!(k.app.panel().map(|p| p.row_count()), Some(4));

        assert_eq!(k.app.panel_answer_text(), None, "opens on the first option");
        k.key(KeyCode::Up);
        assert_eq!(
            k.app.panel_answer_text().as_deref(),
            Some(""),
            "up from the first row wraps to the last, which is the text row"
        );
        k.key(KeyCode::Down);
        assert_eq!(
            k.app.panel_answer_text(),
            None,
            "and down wraps forward off it again"
        );
    }

    #[test]
    fn space_on_a_choice_row_toggles_rather_than_typing_a_space() {
        // Space has its own arm precisely so it means something different on
        // a choice row. Without it the generic character arm would take it
        // and type a space into the free-text field -- which is what the
        // user did not press it for.
        let mut k = Keys::new();
        k.app.park_ask("main", multi("call_1", true));

        assert!(
            k.key(KeyCode::Char(' ')),
            "space should toggle the highlighted box"
        );
        // Still on the choice row. The fall-through this arm exists to
        // prevent would have jumped to the free-text field and typed a
        // space there, which reads as `Some(" ")`.
        assert_eq!(
            k.app.panel_answer_text(),
            None,
            "space must not have moved to the free-text field and typed there"
        );
        let answers = k.app.submit_panel();
        assert_eq!(
            answers,
            vec![("call_1".to_owned(), "a".to_owned())],
            "the box is checked"
        );
    }

    #[test]
    fn plain_c_types_rather_than_quitting_wherever_it_is_pressed() {
        // ^C quits from all three of the prompt, Chat mode and an open panel.
        // Each of those guards has to actually be checked, or the letter is
        // stolen and the session exits on a keystroke nobody meant.
        let mut k = Keys::new();
        k.typed("abc");
        assert!(!k.app.should_quit, "plain c at the prompt types");
        assert_eq!(k.input(), "abc");

        let mut k = Keys::new();
        k.app.park_ask("main", pending("call_1"));
        k.key(KeyCode::Down);
        k.press(KeyCode::Char('c'), KeyModifiers::NONE);
        assert!(!k.app.should_quit, "plain c on a panel types");
        assert_eq!(k.app.panel_answer_text().as_deref(), Some("c"));

        let mut k = Keys::new();
        k.app.push(Who::Model, "an answer");
        k.app.mode = Mode::Chat;
        k.press(KeyCode::Char('c'), KeyModifiers::NONE);
        assert!(!k.app.should_quit, "plain c in Chat leaves Chat and types");
        assert_eq!(k.app.mode, Mode::Send);
        assert_eq!(k.input(), "c");
    }

    #[test]
    fn the_menus_arrows_move_in_the_direction_they_name() {
        // Which entry is highlighted is only observable through what Tab
        // completes, so that is how this reads it. Three commands, so up and
        // down are distinguishable rather than the same toggle.
        let complete_after = |keys: &[KeyCode]| -> String {
            let mut k = Keys::new();
            k.typed("/");
            assert!(k.app.menu_open());
            for &code in keys {
                k.key(code);
            }
            k.key(KeyCode::Tab);
            k.input()
        };

        let first = complete_after(&[]);
        let after_down = complete_after(&[KeyCode::Down]);
        let after_up = complete_after(&[KeyCode::Up]);

        assert_ne!(
            first, after_down,
            "down should move the highlight off the first entry"
        );
        assert_ne!(first, after_up, "and up should too");
        assert_ne!(
            after_up, after_down,
            "in opposite directions, so to different entries"
        );
        // Down once then up once returns to where it started, which pins the
        // pair as inverses rather than merely as two moves.
        assert_eq!(complete_after(&[KeyCode::Down, KeyCode::Up]), first);
    }

    fn pending(call_id: &str) -> tools::Ask {
        tools::Ask {
            call_id: call_id.to_owned(),
            question: "continue?".to_owned(),
            options: vec!["yes".to_owned()],
            multiple: false,
        }
    }

    #[test]
    fn disconnect_reason_falls_back_when_nothing_recorded_one() {
        // Pins the pure function's own logic given no Gone pane -- not the
        // real ordering race this stands in for (the socket task panicking
        // before it can send Disconnected). Reaching that race for real
        // needs a live run() loop, which needs a pty; see the exit-path
        // commit for why that infrastructure isn't here yet.
        let app = App::new("main", "b0");
        assert_eq!(disconnect_reason(&app), "connection closed");
    }

    #[test]
    fn disconnect_reason_reads_the_reason_status_recorded() {
        let mut app = App::new("main", "b0");
        app.apply_transport(Transport::Disconnected(
            "websocket: Invalid status code: 401".to_owned(),
        ));

        assert_eq!(
            disconnect_reason(&app),
            "websocket: Invalid status code: 401"
        );
    }

    #[test]
    fn resolving_with_the_matching_token_sends_the_result_and_clears_pending() {
        let mut app = App::new("main", "b0");
        app.park_ask("main", pending("call_1"));
        let (tx, mut rx) = mpsc::unbounded_channel();

        resolve_ask(&mut app, &tx, "call_1".to_owned(), "yes".to_owned());

        assert!(!app.ask_parked_on("main"));
        let Command::ToolResult {
            call_id, output, ..
        } = rx.try_recv().expect("a result was sent")
        else {
            panic!("expected a ToolResult");
        };
        assert_eq!(call_id, "call_1");
        assert_eq!(output, "yes");
    }

    /// A stale or mismatched token must not answer the wrong call, and must
    /// not strand the real one -- this is the property `debug_assert_eq!`
    /// only checked in debug builds, so it is worth pinning explicitly. See
    /// `resolve_ask`'s doc comment for why a release build has to check it
    /// too.
    #[test]
    fn resolving_with_a_mismatched_token_sends_nothing_and_restores_pending() {
        let mut app = App::new("main", "b0");
        app.park_ask("main", pending("call_1"));
        let (tx, mut rx) = mpsc::unbounded_channel();

        resolve_ask(&mut app, &tx, "call_2".to_owned(), "yes".to_owned());

        assert!(
            rx.try_recv().is_err(),
            "no result should have been sent for the wrong call"
        );
        assert!(
            app.ask_parked_on("main"),
            "the real question must still be pending"
        );
    }

    #[test]
    fn resolving_with_nothing_pending_is_a_quiet_no_op() {
        let mut app = App::new("main", "b0");
        let (tx, mut rx) = mpsc::unbounded_channel();

        resolve_ask(&mut app, &tx, "call_1".to_owned(), "yes".to_owned());

        assert!(rx.try_recv().is_err());
        assert!(!app.ask_parked_on("main"));
    }

    // ---- /slash --------------------------------------------------------
    //
    // `slash` had no tests at all: `replace slash with ()` survived, so
    // nothing called it. It is a pure function over a string and the app,
    // needing no runtime, no terminal and no socket -- ten survivors for an
    // hour, which is why it was the cheapest thing left in the survey.

    fn ran(app: &mut App, line: &str) -> String {
        let mut speaker: Option<Box<dyn kobold::tts::StreamingTts>> = None;
        slash(app, &mut speaker, line);
        app.pane()
            .transcript
            .last()
            .map(|e| e.text.clone())
            .unwrap_or_default()
    }

    #[test]
    fn an_unknown_command_says_so_and_names_what_was_typed() {
        // Naming it matters: the realistic cause is a typo, and a bare
        // "unknown command" leaves the user guessing which of their words
        // was wrong.
        let mut app = App::new("main", "b0");
        let said = ran(&mut app, "/nonsense");
        assert!(
            said.contains("nonsense"),
            "did not name the command: {said}"
        );
        assert!(!app.should_quit, "an unknown command must not quit");
    }

    #[test]
    fn quit_and_its_abbreviation_both_quit_and_nothing_else_does() {
        for line in ["/quit", "/q"] {
            let mut app = App::new("main", "b0");
            slash(&mut app, &mut None, line);
            assert!(app.should_quit, "{line} did not quit");
        }
        // The partner. A handler that set `should_quit` unconditionally would
        // satisfy the loop above perfectly, and end the session on a typo.
        let mut app = App::new("main", "b0");
        slash(&mut app, &mut None, "/quitter");
        assert!(
            !app.should_quit,
            "a command merely starting with quit ended the session"
        );
    }

    #[test]
    fn help_lists_every_command_the_completer_offers() {
        // Generated from the same table that drives completion, so the two
        // cannot disagree -- asserted rather than described, since the whole
        // point of sharing the table is that nobody has to remember to.
        let mut app = App::new("main", "b0");
        let said = ran(&mut app, "/help");
        for c in kobold::complete::COMMANDS {
            assert!(said.contains(c.name), "/help omitted /{}: {said}", c.name);
        }
        assert!(
            said.contains("voices:"),
            "the voice list belongs in help too"
        );
    }

    #[test]
    fn a_bare_slash_is_help_rather_than_an_unknown_command() {
        // Reachable by typing `/` and pressing enter, which is what someone
        // does when they know there are commands and not what they are.
        let mut app = App::new("main", "b0");
        let said = ran(&mut app, "/");
        assert!(
            said.contains("quit"),
            "a bare slash should have shown help: {said}"
        );
        assert!(!said.contains("unknown command"), "{said}");
    }

    #[test]
    fn voice_toggles_and_on_off_are_explicit_rather_than_toggling() {
        let mut app = App::new("main", "b0");
        assert!(!app.voice, "voice is off by default");

        slash(&mut app, &mut None, "/voice");
        assert!(app.voice, "a bare /voice did not toggle on");
        slash(&mut app, &mut None, "/voice");
        assert!(!app.voice, "a bare /voice did not toggle back off");

        // Explicit forms must not toggle: `/voice on` twice is still on, and
        // a user who is unsure of the current state types the explicit form
        // precisely so they do not have to know it.
        slash(&mut app, &mut None, "/voice on");
        slash(&mut app, &mut None, "/voice on");
        assert!(app.voice, "/voice on toggled instead of setting");
        slash(&mut app, &mut None, "/voice off");
        slash(&mut app, &mut None, "/voice off");
        assert!(!app.voice, "/voice off toggled instead of setting");
    }

    #[test]
    fn an_unknown_voice_is_refused_by_name_and_does_not_turn_speech_on() {
        // The dangerous half is the second: asking for a specific voice means
        // "speak", so a rejected name that still switched speech on would
        // start talking in a voice the user did not ask for.
        let mut app = App::new("main", "b0");
        let said = ran(&mut app, "/voice nosuchvoice");
        assert!(
            said.contains("nosuchvoice"),
            "did not name the voice: {said}"
        );
        assert!(
            said.contains(kobold::tts::VOICES[0]),
            "did not list the real ones: {said}"
        );
        assert!(!app.voice, "a rejected voice turned speech on anyway");
    }

    #[test]
    fn naming_a_real_voice_turns_speech_on_because_that_is_what_it_means() {
        let mut app = App::new("main", "b0");
        let name = kobold::tts::VOICES[1];
        let said = ran(&mut app, &format!("/voice {name}"));
        assert!(app.voice, "naming a voice did not turn speech on");
        assert!(
            said.contains(name),
            "the note did not name the voice: {said}"
        );
        // With no engine attached it cannot switch live, and the note has to
        // say so rather than implying the change took effect.
        assert!(
            said.contains("restart"),
            "a live switch was implied with no engine: {said}"
        );
    }

    #[test]
    fn voice_command_persists_state_across_sessions() {
        let temp_dir =
            std::env::temp_dir().join(format!("kobold-voice-persist-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();

        let mut app = App::new("main", "b0");
        app.root = Some(temp_dir.clone());

        // Default: voice is disabled by default
        let (s0, _) = settings::Settings::load(&temp_dir);
        assert!(!s0.voice.enabled, "voice must be disabled by default");

        // Turn voice on via slash command
        slash(&mut app, &mut None, "/voice on");
        assert!(app.voice);
        let (s1, _) = settings::Settings::load(&temp_dir);
        assert!(s1.voice.enabled, "voice on must persist to settings.json");

        // Turn voice off via slash command
        slash(&mut app, &mut None, "/voice off");
        assert!(!app.voice);
        let (s2, _) = settings::Settings::load(&temp_dir);
        assert!(!s2.voice.enabled, "voice off must persist to settings.json");

        // Toggle voice on via bare /voice
        slash(&mut app, &mut None, "/voice");
        assert!(app.voice);
        let (s3, _) = settings::Settings::load(&temp_dir);
        assert!(
            s3.voice.enabled,
            "bare /voice toggle must persist to settings.json"
        );

        // Change voice name
        slash(&mut app, &mut None, "/voice cosette");
        assert!(app.voice);
        let (s4, _) = settings::Settings::load(&temp_dir);
        assert!(s4.voice.enabled);
        assert_eq!(
            s4.voice.voice, "cosette",
            "named voice must persist to settings.json"
        );

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
