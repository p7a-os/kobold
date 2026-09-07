//! Standalone Southbound adapter binary for ACP (`kobold-adapter-acp`).
//!
//! Connects `koboldd` (via Southbound stdio protocol) to an external autonomous
//! agent speaking the Agent Client Protocol (ACP) over JSON-RPC 2.0.

use kobold_adapter_acp::{AcpBridge, AgentProcess};
use kobold_proto::{codec, Command, OutgoingFrame, Startup};
use std::path::{Path, PathBuf};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("kobold-adapter-acp: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut agent_cmd: Option<String> = std::env::var("ACP_AGENT_CMD").ok();
    let mut agent_args: Vec<String> = Vec::new();
    if let Ok(env_args) = std::env::var("ACP_AGENT_ARGS") {
        for arg in env_args.split_whitespace() {
            agent_args.push(arg.to_string());
        }
    }
    let mut mock_mode = std::env::var("KOBOLD_MOCK").is_ok() || std::env::var("ACP_MOCK").is_ok();
    let mut model_override: Option<String> = std::env::var("ACP_MODEL").ok();
    let mut force_acp = false;
    let mut workdir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--agent-cmd" if i + 1 < argv.len() => {
                agent_cmd = Some(argv[i + 1].clone());
                i += 2;
            }
            "--agent-args" if i + 1 < argv.len() => {
                for arg in argv[i + 1].split_whitespace() {
                    agent_args.push(arg.to_string());
                }
                i += 2;
            }
            "--model" if i + 1 < argv.len() => {
                model_override = Some(argv[i + 1].clone());
                i += 2;
            }
            "--acp" => {
                force_acp = true;
                i += 1;
            }
            "--mock" => {
                mock_mode = true;
                i += 1;
            }
            "--workdir" if i + 1 < argv.len() => {
                workdir = PathBuf::from(&argv[i + 1]);
                i += 2;
            }
            "--version" | "-v" | "-V" => {
                println!("kobold-adapter-acp {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--help" | "-h" => {
                eprintln!("Usage: kobold-adapter-acp [OPTIONS]");
                eprintln!("Options:");
                eprintln!("      --agent-cmd <CMD>   Agent executable (e.g. claude, grok, codex, opencode, agy)");
                eprintln!("      --agent-args <ARGS> Arguments for ACP agent server (default: $ACP_AGENT_ARGS)");
                eprintln!("      --model <MODEL>     Model identifier for session");
                eprintln!("      --acp               Force ACP JSON-RPC mode");
                eprintln!("      --mock              Run in mock mode without external agent");
                eprintln!("      --workdir <PATH>    Working directory for session");
                eprintln!("  -V, --version           Print version");
                eprintln!("  -h, --help              Print help");
                return Ok(());
            }
            _ => {
                agent_args.push(argv[i].clone());
                i += 1;
            }
        }
    }

    let mut stdin_lines = BufReader::new(tokio::io::stdin()).lines();

    // 1. Ingest Startup frame
    let first = stdin_lines
        .next_line()
        .await?
        .ok_or("stdin closed before startup frame arrived")?;
    let startup: Startup = codec::decode::<Startup>(&first)?;
    let active_model = model_override.unwrap_or_else(|| startup.model.name.clone());

    let (frame_tx, mut frame_rx) = mpsc::unbounded_channel::<OutgoingFrame>();
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<Command>();

    // Stdout pump: sends OutgoingFrame to koboldd
    let stdout_pump = tokio::spawn(async move {
        let mut out = tokio::io::stdout();
        while let Some(frame) = frame_rx.recv().await {
            if let Ok(line) = codec::encode(&frame) {
                if out.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
                if out.flush().await.is_err() {
                    break;
                }
            }
        }
    });

    // Stdin reader: reads Command from koboldd
    let stdin_reader = tokio::spawn(async move {
        while let Ok(Some(line)) = stdin_lines.next_line().await {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(cmd) = codec::decode::<Command>(trimmed) {
                let quit = matches!(cmd, Command::Quit);
                let _ = cmd_tx.send(cmd);
                if quit {
                    break;
                }
            }
        }
    });

    // 1. If explicit mock mode was requested, run mock loop
    if mock_mode {
        let _ = frame_tx.send(OutgoingFrame::Transport(kobold_proto::Transport::Connected));

        while let Some(cmd) = cmd_rx.recv().await {
            match cmd {
                Command::Send { lane, text, .. } => {
                    let run_id = "mock-run-1".to_string();
                    let _ = frame_tx.send(OutgoingFrame::Event {
                        lane: lane.clone(),
                        event: kobold_proto::agui::Outgoing::RunStarted {
                            base: kobold_proto::agui::Base::default(),
                            thread_id: lane.clone(),
                            run_id: run_id.clone(),
                            parent_run_id: None,
                        },
                    });
                    let _ = frame_tx.send(OutgoingFrame::Event {
                        lane: lane.clone(),
                        event: kobold_proto::agui::Outgoing::TextMessageContent {
                            base: kobold_proto::agui::Base::default(),
                            delta: format!("mock reply to: {text}"),
                            message_id: "msg-1".into(),
                        },
                    });
                    let _ = frame_tx.send(OutgoingFrame::Event {
                        lane: lane.clone(),
                        event: kobold_proto::agui::Outgoing::RunFinished {
                            base: kobold_proto::agui::Base::default(),
                            thread_id: lane,
                            run_id,
                            usage: None,
                        },
                    });
                }
                Command::ToolResult { .. } => {}
                Command::Cancel { .. } => {}
                Command::Quit => break,
            }
        }

        let _ = stdout_pump.await;
        let _ = stdin_reader.await;
        return Ok(());
    }

    // 2. Check whether to run in ACP JSON-RPC mode
    let is_acp_mode = force_acp
        || agent_args.iter().any(|a| a == "acp" || a == "stdio" || a == "--agent")
        || agent_cmd.as_deref().map(|c| c.contains("mock-acp-agent")).unwrap_or(false);

    if is_acp_mode {
        let session_id = uuid::Uuid::now_v7().to_string();
        let mut bridge = AcpBridge::new(session_id);
        let cmd_str = agent_cmd.unwrap_or_else(|| "mock-acp-agent".to_string());
        let mut agent = AgentProcess::spawn(&cmd_str, &agent_args, &workdir)?;

        // Send initialize request
        let init_req = bridge.create_initialize_request();
        agent.send(&init_req).await?;

        // Event pump between ACP agent and Kobold
        loop {
            tokio::select! {
                cmd = cmd_rx.recv() => match cmd {
                    Some(cmd) => {
                        let quit = matches!(cmd, Command::Quit);
                        let (rpc_msg, frames) = bridge.handle_kobold_command(cmd);
                        for f in frames {
                            let _ = frame_tx.send(f);
                        }
                        if let Some(msg) = rpc_msg {
                            let _ = agent.send(&msg).await;
                        }
                        if quit {
                            break;
                        }
                    }
                    None => break,
                },
                agent_msg = agent.recv() => match agent_msg {
                    Ok(Some(msg)) => {
                        let (reply_opt, frames) = bridge.handle_agent_message(msg);
                        for f in frames {
                            let _ = frame_tx.send(f);
                        }
                        if let Some(reply) = reply_opt {
                            let _ = agent.send(&reply).await;
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        eprintln!("kobold-adapter-acp: error reading agent: {e}");
                        break;
                    }
                }
            }
        }

        let _ = agent.kill();
        drop(frame_tx);
        let _ = stdout_pump.await;
        let _ = stdin_reader.await;
        return Ok(());
    }

    // 3. Direct Coder Agent CLI Execution Mode (claude, grok, codex, opencode, agy)
    let resolved_agent_cmd = agent_cmd.or_else(|| {
        for candidate in ["claude", "grok", "codex", "opencode", "agy"] {
            if kobold_core::sandbox::resolve(candidate).is_some() {
                return Some(candidate.to_string());
            }
        }
        None
    });

    let Some(cmd_str) = resolved_agent_cmd else {
        // Fallback only if no agent command provided and no coder agent binary exists on PATH
        eprintln!("kobold-adapter-acp: warning: no agent command specified and none found on PATH, falling back to mock reply");
        let _ = frame_tx.send(OutgoingFrame::Transport(kobold_proto::Transport::Connected));
        while let Some(cmd) = cmd_rx.recv().await {
            match cmd {
                Command::Send { lane, text, .. } => {
                    let run_id = "mock-run-1".to_string();
                    let _ = frame_tx.send(OutgoingFrame::Event {
                        lane: lane.clone(),
                        event: kobold_proto::agui::Outgoing::RunStarted {
                            base: kobold_proto::agui::Base::default(),
                            thread_id: lane.clone(),
                            run_id: run_id.clone(),
                            parent_run_id: None,
                        },
                    });
                    let _ = frame_tx.send(OutgoingFrame::Event {
                        lane: lane.clone(),
                        event: kobold_proto::agui::Outgoing::TextMessageContent {
                            base: kobold_proto::agui::Base::default(),
                            delta: format!("mock reply to: {text}"),
                            message_id: "msg-1".into(),
                        },
                    });
                    let _ = frame_tx.send(OutgoingFrame::Event {
                        lane: lane.clone(),
                        event: kobold_proto::agui::Outgoing::RunFinished {
                            base: kobold_proto::agui::Base::default(),
                            thread_id: lane,
                            run_id,
                            usage: None,
                        },
                    });
                }
                Command::Quit => break,
                _ => {}
            }
        }
        let _ = stdout_pump.await;
        let _ = stdin_reader.await;
        return Ok(());
    };

    let _ = frame_tx.send(OutgoingFrame::Transport(kobold_proto::Transport::Connected));

    while let Some(cmd) = cmd_rx.recv().await {
        match cmd {
            Command::Send { lane, text, .. } => {
                let run_id = format!("run-{}", uuid::Uuid::now_v7());
                let _ = frame_tx.send(OutgoingFrame::Event {
                    lane: lane.clone(),
                    event: kobold_proto::agui::Outgoing::RunStarted {
                        base: kobold_proto::agui::Base::default(),
                        thread_id: lane.clone(),
                        run_id: run_id.clone(),
                        parent_run_id: None,
                    },
                });

                let bin_name = Path::new(&cmd_str)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or(&cmd_str);

                let mut args: Vec<String> = Vec::new();
                match bin_name {
                    "claude" => {
                        args.push("-p".to_string());
                        args.push(text.clone());
                        if !active_model.is_empty() {
                            args.push("--model".to_string());
                            args.push(active_model.clone());
                        }
                        args.push("--output-format".to_string());
                        args.push("text".to_string());
                    }
                    "grok" => {
                        args.push("-p".to_string());
                        args.push(text.clone());
                        if !active_model.is_empty() {
                            args.push("-m".to_string());
                            args.push(active_model.clone());
                        }
                    }
                    "codex" => {
                        args.push("exec".to_string());
                        if !active_model.is_empty() {
                            args.push("-m".to_string());
                            args.push(active_model.clone());
                        }
                        args.push(text.clone());
                    }
                    "opencode" => {
                        args.push("run".to_string());
                        args.push(text.clone());
                        if !active_model.is_empty() {
                            args.push("-m".to_string());
                            args.push(active_model.clone());
                        }
                    }
                    "agy" => {
                        args.push("-p".to_string());
                        args.push(text.clone());
                        if !active_model.is_empty() {
                            args.push("--model".to_string());
                            args.push(active_model.clone());
                        }
                    }
                    _ => {
                        if !agent_args.is_empty() {
                            args.extend(agent_args.clone());
                            args.push(text.clone());
                        } else {
                            args.push("-p".to_string());
                            args.push(text.clone());
                        }
                    }
                }

                let mut child = match tokio::process::Command::new(&cmd_str)
                    .args(&args)
                    .current_dir(&workdir)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = frame_tx.send(OutgoingFrame::Event {
                            lane: lane.clone(),
                            event: kobold_proto::agui::Outgoing::TextMessageContent {
                                base: kobold_proto::agui::Base::default(),
                                delta: format!("Failed to spawn {cmd_str}: {e}"),
                                message_id: format!("msg-{}", uuid::Uuid::now_v7()),
                            },
                        });
                        let _ = frame_tx.send(OutgoingFrame::Event {
                            lane: lane.clone(),
                            event: kobold_proto::agui::Outgoing::RunFinished {
                                base: kobold_proto::agui::Base::default(),
                                thread_id: lane,
                                run_id,
                                usage: None,
                            },
                        });
                        continue;
                    }
                };

                let mut stdout = child.stdout.take().expect("stdout");
                let mut buf = [0u8; 1024];
                let mut total_output = String::new();
                let msg_id = format!("msg-{}", uuid::Uuid::now_v7());

                loop {
                    tokio::select! {
                        n_res = stdout.read(&mut buf) => match n_res {
                            Ok(0) => break,
                            Ok(n) => {
                                let chunk = String::from_utf8_lossy(&buf[..n]).to_string();
                                total_output.push_str(&chunk);
                                let _ = frame_tx.send(OutgoingFrame::Event {
                                    lane: lane.clone(),
                                    event: kobold_proto::agui::Outgoing::TextMessageContent {
                                        base: kobold_proto::agui::Base::default(),
                                        delta: chunk,
                                        message_id: msg_id.clone(),
                                    },
                                });
                            }
                            Err(_) => break,
                        },
                        Some(next_cmd) = cmd_rx.recv() => {
                            match next_cmd {
                                Command::Cancel { .. } => {
                                    let _ = child.kill().await;
                                    break;
                                }
                                Command::Quit => {
                                    let _ = child.kill().await;
                                    return Ok(());
                                }
                                _ => {}
                            }
                        }
                    }
                }

                let _ = child.wait().await;

                if total_output.trim().is_empty() {
                    if let Some(mut stderr) = child.stderr.take() {
                        let mut err_str = String::new();
                        let _ = stderr.read_to_string(&mut err_str).await;
                        if !err_str.trim().is_empty() {
                            let _ = frame_tx.send(OutgoingFrame::Event {
                                lane: lane.clone(),
                                event: kobold_proto::agui::Outgoing::TextMessageContent {
                                    base: kobold_proto::agui::Base::default(),
                                    delta: format!("{cmd_str} error output:\n{err_str}"),
                                    message_id: msg_id.clone(),
                                },
                            });
                        }
                    }
                }

                let _ = frame_tx.send(OutgoingFrame::Event {
                    lane: lane.clone(),
                    event: kobold_proto::agui::Outgoing::RunFinished {
                        base: kobold_proto::agui::Base::default(),
                        thread_id: lane,
                        run_id,
                        usage: None,
                    },
                });
            }
            Command::ToolResult { .. } => {}
            Command::Cancel { .. } => {}
            Command::Quit => break,
        }
    }

    let _ = stdout_pump.await;
    let _ = stdin_reader.await;
    Ok(())
}
