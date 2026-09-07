//! Standalone Southbound adapter binary for ACP (`kobold-adapter-acp`).
//!
//! Connects `koboldd` (via Southbound stdio protocol) to an external autonomous
//! agent speaking the Agent Client Protocol (ACP) over JSON-RPC 2.0.

use kobold_adapter_acp::{AcpBridge, AgentProcess};
use kobold_proto::{codec, Command, OutgoingFrame, Startup};
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
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
    let mut mock_mode = false;
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
                eprintln!("      --agent-cmd <CMD>   ACP agent server executable (default: $ACP_AGENT_CMD)");
                eprintln!("      --agent-args <ARGS> Arguments for ACP agent server (default: $ACP_AGENT_ARGS)");
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
    let _startup: Startup = codec::decode::<Startup>(&first)?;

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

    let session_id = uuid::Uuid::now_v7().to_string();
    let mut bridge = AcpBridge::new(session_id);

    if mock_mode || agent_cmd.is_none() {
        // Internal mock loop for tests or when no agent command configured
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

    let cmd_str = agent_cmd.unwrap();
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
                Ok(None) => {
                    // Agent closed stdout
                    break;
                }
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
    Ok(())
}
