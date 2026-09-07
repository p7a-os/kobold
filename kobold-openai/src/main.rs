//! The adapter as a process: `kobold-proto` on stdin and stdout, one message
//! per line, with the OpenAI Responses API on the other side.
//!
//! Stdout is the protocol and nothing else. Anything this program wants to
//! say to a human goes to stderr, which Kobold leaves inherited so it lands
//! in the terminal the user is already looking at. A stray `println!` here is
//! a torn conversation, not a cosmetic bug.

use kobold_proto::{codec, Command, OutgoingFrame, Startup};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv
        .iter()
        .any(|a| a == "--version" || a == "-v" || a == "-V")
    {
        println!("kobold-openai {}", env!("CARGO_PKG_VERSION"));
        return std::process::ExitCode::SUCCESS;
    }
    if argv.iter().any(|a| a == "--help" || a == "-h") {
        eprintln!("Usage: kobold-openai [OPTIONS]");
        eprintln!("Southbound OpenAI Responses WebSocket streaming adapter.");
        eprintln!("Options:");
        eprintln!("  -V, --version         Print version");
        eprintln!("  -h, --help            Print help");
        return std::process::ExitCode::SUCCESS;
    }

    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        // To stderr and non-zero, so a failure here is something Kobold and
        // the user both find out about. The parent turns this into a message
        // on the normal screen; see the dead-socket handling it mirrors.
        Err(e) => {
            eprintln!("kobold-openai: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();

    // Exactly one, before anything else. That ordering is the reason
    // `Startup` is its own frame rather than a `Command` variant -- see its
    // definition.
    let first = lines
        .next_line()
        .await?
        .ok_or("stdin closed before the startup frame arrived")?;
    let Startup {
        api_key,
        model,
        egress,
    } = codec::decode::<Startup>(&first)?;

    let (update_tx, mut update_rx) = mpsc::unbounded_channel::<OutgoingFrame>();
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<Command>();

    // The provider task. `Dial` is the real connector; `run_with` is the
    // reconnect loop, and it takes the connector as an argument precisely so
    // the loop can be driven by a scripted one in tests without a network.
    let provider = tokio::spawn(kobold_openai::net::run_with(
        kobold_openai::net::Dial { api_key, egress },
        model,
        update_tx,
        cmd_rx,
    ));

    // Updates out. A separate task because reading stdin and writing stdout
    // both block, and a turn produces updates while commands are still
    // arriving -- serialising the two would deadlock the moment a tool call
    // and a delta crossed.
    let pump = tokio::spawn(async move {
        let mut out = tokio::io::stdout();
        while let Some(frame) = update_rx.recv().await {
            let line = match codec::encode(&frame) {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("kobold-openai: could not encode a frame: {e}");
                    continue;
                }
            };
            // Two checks rather than one `||`: either failure means Kobold
            // has gone and there is nothing left to report to, and short-
            // circuiting the flush behind the write reads as a subtlety when
            // it is just the same answer twice.
            if out.write_all(line.as_bytes()).await.is_err() {
                break;
            }
            if out.flush().await.is_err() {
                break;
            }
        }
    });

    // Commands in, until stdin closes -- which is how Kobold says it is done,
    // and the only shutdown signal that survives Kobold being killed.
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let command = codec::decode::<Command>(&line)?;
        let quit = matches!(command, Command::Quit);
        if cmd_tx.send(command).is_err() {
            break;
        }
        if quit {
            break;
        }
    }

    // Dropping the sender ends the provider task's receive loop, which drops
    // its update sender, which ends the pump. Awaiting both rather than
    // exiting under them, so a final update still reaches Kobold.
    drop(cmd_tx);
    let _ = provider.await;
    let _ = pump.await;
    Ok(())
}
