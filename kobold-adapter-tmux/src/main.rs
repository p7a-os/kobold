//! Southbound PTY & tmux meta-harness adapter executable (`kobold-adapter-tmux`).
//!
//! Connects an interactive shell, terminal CLI agent, or tmux session to
//! the Kobold Kernel as a native Southbound adapter over standard I/O.

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use kobold_proto::codec;
use kobold_proto::{Command, OutgoingFrame, Startup, Transport};

use kobold_adapter_tmux::bridge::TmuxBridge;
use kobold_adapter_tmux::pty::PtySession;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let argv: Vec<String> = std::env::args().skip(1).collect();

    let mut tmux_session: Option<String> = None;
    let mut custom_cmd: Option<String> = None;
    let mut custom_args: Vec<String> = Vec::new();

    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--tmux" if i + 1 < argv.len() => {
                tmux_session = Some(argv[i + 1].clone());
                i += 2;
            }
            "--cmd" if i + 1 < argv.len() => {
                custom_cmd = Some(argv[i + 1].clone());
                i += 2;
            }
            "--args" => {
                custom_args = argv[i + 1..].to_vec();
                break;
            }
            "--version" | "-v" | "-V" => {
                println!("kobold-adapter-tmux {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--help" | "-h" => {
                eprintln!("Usage: kobold-adapter-tmux [OPTIONS]");
                eprintln!("Options:");
                eprintln!("      --tmux <NAME>     Attach to/manage an external tmux session");
                eprintln!(
                    "      --cmd <CMD>       Target command to spawn inside PTY (default: $SHELL)"
                );
                eprintln!("      --args ...        Arguments forwarded to the target command");
                eprintln!("  -V, --version         Print version");
                eprintln!("  -h, --help            Print help");
                return Ok(());
            }
            _ => {
                i += 1;
            }
        }
    }

    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin);
    let mut line = String::new();

    // 1. Read Startup frame from stdin
    if reader.read_line(&mut line).await? == 0 {
        return Err("EOF before Startup frame".into());
    }

    let _startup: Startup =
        codec::decode(&line).map_err(|e| format!("invalid startup frame: {e}"))?;

    // 2. Announce connection to kernel
    let (frame_tx, mut frame_rx) = mpsc::unbounded_channel::<OutgoingFrame>();

    tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(frame) = frame_rx.recv().await {
            if let Ok(encoded) = codec::encode(&frame) {
                if stdout.write_all(encoded.as_bytes()).await.is_err() {
                    break;
                }
                if stdout.flush().await.is_err() {
                    break;
                }
            }
        }
    });

    let _ = frame_tx.send(OutgoingFrame::Transport(Transport::Connected));

    // 3. Spawn PTY session
    let session = if let Some(sess) = tmux_session {
        let cmd = custom_cmd.unwrap_or_else(|| "bash".to_string());
        PtySession::spawn_in_tmux(&sess, &cmd, &custom_args)?
    } else {
        let shell = custom_cmd
            .or_else(|| std::env::var("SHELL").ok())
            .unwrap_or_else(|| "bash".to_string());
        PtySession::spawn_in_pty(&shell, &custom_args)?
    };

    let mut bridge = TmuxBridge::new(session, "main");

    // 4. Command loop
    line.clear();
    while reader.read_line(&mut line).await? != 0 {
        if let Ok(cmd) = codec::decode::<Command>(&line) {
            let keep_running = bridge.handle_command(cmd, &frame_tx).await?;
            if !keep_running {
                break;
            }
        }
        line.clear();
    }

    Ok(())
}
