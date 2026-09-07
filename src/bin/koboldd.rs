//! Headless daemon binary for Kobold (`koboldd`).
//!
//! Owns the `Kernel`, launches and supervises adapter subprocesses,
//! opens a Unix Domain Socket, and serves Northbound AG-UI frames to attached frontends.

use std::path::PathBuf;

use kobold::daemon::{default_socket_path, Daemon, DaemonConfig};
use kobold::net::{Command, IncomingFrame, Startup};
use kobold::settings::Settings;
use kobold::tools;
use tokio::sync::{mpsc, watch};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let argv: Vec<String> = std::env::args().skip(1).collect();

    let mut socket_path: Option<PathBuf> = None;
    let mut session_id = String::from("default");
    let mut workdir = std::env::current_dir()?;
    let mut adapter_override: Option<String> = None;
    let mut mock_mode = false;
    let mut unconfined = false;
    let mut ws_port: Option<u16> = None;

    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--socket" | "-s" if i + 1 < argv.len() => {
                socket_path = Some(PathBuf::from(&argv[i + 1]));
                i += 2;
            }
            "--session" if i + 1 < argv.len() => {
                session_id = argv[i + 1].clone();
                i += 2;
            }
            "--workdir" | "-w" if i + 1 < argv.len() => {
                workdir = PathBuf::from(&argv[i + 1]);
                i += 2;
            }
            "--adapter" | "-a" if i + 1 < argv.len() => {
                adapter_override = Some(argv[i + 1].clone());
                i += 2;
            }
            "--mock" => {
                mock_mode = true;
                i += 1;
            }
            "--unconfined" | "--no-sandbox" => {
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
            "--version" | "-v" | "-V" => {
                println!("koboldd {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--help" | "-h" => {
                eprintln!("Usage: koboldd [OPTIONS]");
                eprintln!("Options:");
                eprintln!("  -s, --socket <PATH>   Path to Unix Domain Socket");
                eprintln!("      --session <ID>    Session identifier (default: 'default')");
                eprintln!("  -w, --workdir <PATH>  Working directory");
                eprintln!("  -a, --adapter <CMD>   Adapter executable (default: 'kobold-openai')");
                eprintln!("      --mock            Run with mock adapter channels for testing");
                eprintln!(
                    "      --unconfined      Run adapter unconfined (without sandbox or broker)"
                );
                eprintln!("      --ws-port <PORT>  Enable WebSocket companion on specified port");
                eprintln!("  -V, --version         Print version");
                eprintln!("  -h, --help            Print help");
                return Ok(());
            }
            _ => {
                i += 1;
            }
        }
    }

    let existing = kobold::session::SessionRegistry::find_for_workdir(&workdir);
    if !existing.is_empty() {
        eprintln!(
            "koboldd: warning: potential conflict: {} session(s) already active in {}",
            existing.len(),
            workdir.display()
        );
        for s in &existing {
            eprintln!("  - active session '{}' (pid {})", s.session_id, s.pid);
        }
    }

    let socket = socket_path.unwrap_or_else(|| default_socket_path(&session_id));
    let config = if let Some(port) = ws_port {
        let token = kobold::ws::generate_token();
        if let Ok(path) = kobold::ws::save_auth_token(&session_id, &token) {
            eprintln!("koboldd: auth token saved to {}", path.display());
        }
        eprintln!("koboldd web companion listening on http://127.0.0.1:{port}/?token={token}");
        DaemonConfig::new(&socket, &workdir).with_ws(port, token)
    } else {
        DaemonConfig::new(&socket, &workdir)
    };
    let daemon = Daemon::bind(config)?;
    eprintln!("koboldd listening on {}", daemon.socket_path().display());

    let meta = kobold::session::SessionMetadata::new(
        &session_id,
        std::process::id(),
        daemon.socket_path(),
        &workdir,
        adapter_override.as_deref().unwrap_or("kobold-openai"),
    );
    if let Err(e) = meta.save() {
        eprintln!("warning: could not write session metadata: {e}");
    }

    struct MetaGuard(kobold::session::SessionMetadata);
    impl Drop for MetaGuard {
        fn drop(&mut self) {
            let _ = self.0.remove();
        }
    }
    let _meta_guard = MetaGuard(meta);

    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    // Trap SIGINT / SIGTERM
    let shutdown_signal_tx = shutdown_tx.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        eprintln!("\nReceived shutdown signal, terminating koboldd...");
        let _ = shutdown_signal_tx.send(true);
    });

    if mock_mode {
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<Command>();
        let (_adapter_tx, adapter_rx) = mpsc::channel::<IncomingFrame>(32);
        tokio::spawn(async move { while let Some(_cmd) = cmd_rx.recv().await {} });
        daemon.run(cmd_tx, adapter_rx, shutdown_rx).await?;
        return Ok(());
    }

    // Normal mode: spawn adapter subprocess
    let (cfg, _) = Settings::load(&workdir);
    let mut api_key = std::env::var("LLM_API_KEY")
        .or_else(|_| std::env::var("OPENAI_API_KEY"))
        .or_else(|_| std::env::var("OPENROUTER_API_KEY"))
        .unwrap_or_default();

    if api_key.is_empty() {
        if let Some(p) = cfg.providers.get("openai") {
            if !p.api_key.is_empty() {
                api_key = p.api_key.clone();
            }
        }
    }
    if api_key.is_empty() {
        if let Some(p) = cfg.providers.get("openrouter") {
            if !p.api_key.is_empty() {
                api_key = p.api_key.clone();
            }
        }
    }

    let mcp = tools::Sources::new();
    let startup = Startup {
        egress: None,
        api_key,
        model: kobold::net::Model {
            name: cfg.model.clone(),
            effort: cfg.reasoning_effort.clone(),
            server_tools: cfg.server_tools.clone(),
            tools: tools::schemas()
                .into_iter()
                .map(|(n, d, s)| (n.to_owned(), d.to_owned(), s))
                .chain(mcp.all_schemas())
                .collect(),
        },
    };

    let (adapter_cmd, extra_args) = match adapter_override {
        Some(cmd) => {
            let mut parts = cmd.split_whitespace();
            let bin = parts.next().unwrap_or("").to_string();
            let args: Vec<String> = parts.map(|s| s.to_string()).collect();
            (bin, args)
        }
        None => (
            if !cfg.adapter.is_empty() {
                cfg.adapter.clone()
            } else if cfg.agents.values().any(|a| a.enabled) {
                "kobold-adapter-acp".to_string()
            } else {
                "kobold-openai".to_string()
            },
            Vec::new(),
        ),
    };

    let mut adapter_args = cfg.adapter_args.clone();
    adapter_args.extend(extra_args);
    if adapter_cmd.contains("acp") {
        if let Ok(cmd) = std::env::var("ACP_AGENT_CMD") {
            if !adapter_args.iter().any(|a| a == "--agent-cmd") {
                adapter_args.push("--agent-cmd".into());
                adapter_args.push(cmd);
            }
        }
        if let Ok(args) = std::env::var("ACP_AGENT_ARGS") {
            if !adapter_args.iter().any(|a| a == "--agent-args") {
                adapter_args.push("--agent-args".into());
                adapter_args.push(args);
            }
        }

        if !adapter_args.iter().any(|a| a == "--agent-cmd" || a == "--mock") {
            let active = cfg.active_harness();
            let agent_exe = match active {
                kobold::catalog::HARNESS_CLAUDE_CODE => Some("claude"),
                kobold::catalog::HARNESS_GROK_BUILD => Some("grok"),
                kobold::catalog::HARNESS_CODEX => Some("codex"),
                kobold::catalog::HARNESS_OPENCODE => Some("opencode"),
                kobold::catalog::HARNESS_ANTIGRAVITY => Some("agy"),
                _ => cfg.agents.iter().find(|(_, a)| a.enabled).and_then(|(k, _)| {
                    match k.as_str() {
                        "claude-code" | "claude" => Some("claude"),
                        "grok-build" | "grok" => Some("grok"),
                        "codex" => Some("codex"),
                        "opencode" => Some("opencode"),
                        "antigravity" | "agy" => Some("agy"),
                        _ => None,
                    }
                }),
            };
            if let Some(exe) = agent_exe {
                adapter_args.push("--agent-cmd".into());
                adapter_args.push(exe.into());
            }
        }
        if !adapter_args.iter().any(|a| a == "--model") {
            let model_to_pass = if cfg.current_harness.is_some() || cfg.default_agent.is_some() {
                cfg.model.clone()
            } else {
                let active = cfg.active_harness();
                kobold::catalog::default_model_for_harness(active).to_string()
            };
            if !model_to_pass.is_empty() {
                adapter_args.push("--model".into());
                adapter_args.push(model_to_pass);
            }
        }
    }

    let is_unconfined = unconfined
        || adapter_cmd.contains("tmux")
        || adapter_cmd.contains("pty")
        || adapter_cmd.contains("acp");
    let (_adapter, cmd_tx, adapter_rx) = if is_unconfined {
        kobold::adapter::Adapter::spawn_unconfined(&adapter_cmd, &adapter_args, &startup)
            .await?
    } else {
        let allow_hosts = if cfg.adapter_allow.is_empty() {
            vec!["api.openai.com".to_owned()]
        } else {
            cfg.adapter_allow.clone()
        };
        kobold::adapter::Adapter::spawn(&adapter_cmd, &adapter_args, &startup, &allow_hosts)
            .await?
    };

    daemon.run(cmd_tx, adapter_rx, shutdown_rx).await?;
    Ok(())
}
