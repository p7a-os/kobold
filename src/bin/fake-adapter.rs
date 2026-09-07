//! An adapter that does whatever the test told it to, over the real codec.
//!
//! The script is one argument, a comma-separated list of acts. Each is a
//! thing a real adapter might do, including the things a real adapter should
//! not:
//!
//! - `connected`         -- emit `Transport::Connected`
//! - `egress`            -- emit the egress socket path it was given
//! - `delta:TEXT`        -- emit one text delta on the lane last commanded
//! - `complete`          -- emit `Update::Completed`
//! - `fail:CODE`         -- emit `Update::Failed`
//! - `await`             -- block until one `Command` arrives
//! - `blank`             -- write an empty line, which is not a message
//! - `garbage`           -- write a line that is not protocol
//! - `exit:N`            -- exit with that status, mid-turn if so scripted
//! - `silent`            -- read commands forever and answer nothing
//! - `stall:MS`          -- go quiet for MS milliseconds mid-stream
//! - `flood:N`           -- emit N deltas as fast as it can
//!
//! Deliberately synchronous and dependency-free apart from the protocol
//! crate: a fixture that needs its own runtime is a fixture that can fail in
//! ways the thing under test did not cause.
//!
//! A `src/bin` of this crate rather than its own workspace member, and the
//! reason is build ordering rather than taste: `tests/adapter.rs` spawns it,
//! and one package's integration tests do not cause another package's binary
//! to be built. As a member it passed only where someone had built it by
//! hand, which a clean tree caught. `bench` sits here for the same reason.

use std::io::{BufRead, Write};

use kobold::net::{
    agui::{Base, Outgoing},
    Command, Startup, Transport,
};
use kobold_proto::codec;
use kobold_proto::OutgoingFrame;

fn main() {
    let script = std::env::args().nth(1).unwrap_or_default();
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    // Every adapter must read this first, so the fake does too -- otherwise
    // it would not exercise the handshake it exists to stand in for.
    let first = lines.next().and_then(Result::ok).unwrap_or_default();
    let startup: Startup = match codec::decode(&first) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("fake-adapter: {e}");
            std::process::exit(2);
        }
    };
    // Proves the credential arrived on stdin rather than in the environment,
    // and gives a test something to assert on without echoing the secret.
    eprintln!(
        "fake-adapter: configured for {} with {} tools",
        startup.model.name,
        startup.model.tools.len()
    );

    let mut lane = String::new();
    let emit = |out: &mut dyn Write, f: &OutgoingFrame| {
        let _ = out.write_all(codec::encode(f).expect("encode").as_bytes());
        let _ = out.flush();
    };

    for act in script.split(',').filter(|a| !a.is_empty()) {
        let (verb, arg) = act.split_once(':').unwrap_or((act, ""));
        match verb {
            "connected" => emit(&mut out, &OutgoingFrame::Transport(Transport::Connected)),
            // Reports back what the startup frame said its egress socket was.
            // A real adapter dials it; this one only has to prove it was told,
            // which is the half nothing was checking -- the field could be
            // dropped from the `Startup` expression and no test noticed.
            "egress" => emit(
                &mut out,
                &OutgoingFrame::Event {
                    lane: lane.clone(),
                    event: Outgoing::TextMessageContent {
                        base: Base::default(),
                        message_id: lane.clone(),
                        delta: startup
                            .egress
                            .clone()
                            .unwrap_or_else(|| "<none>".to_owned()),
                    },
                },
            ),
            "delta" => emit(
                &mut out,
                &OutgoingFrame::Event {
                    lane: lane.clone(),
                    event: Outgoing::TextMessageContent {
                        base: Base::default(),
                        message_id: lane.clone(),
                        delta: arg.to_owned(),
                    },
                },
            ),
            "complete" => emit(
                &mut out,
                &OutgoingFrame::Event {
                    lane: lane.clone(),
                    event: Outgoing::RunFinished {
                        base: Base::default(),
                        thread_id: lane.clone(),
                        run_id: String::new(),
                        usage: None,
                    },
                },
            ),
            "fail" => emit(
                &mut out,
                &OutgoingFrame::Event {
                    lane: lane.clone(),
                    event: Outgoing::RunError {
                        base: Base::default(),
                        message: "scripted failure".to_owned(),
                        code: Some(arg.to_owned()),
                        usage: None,
                    },
                },
            ),
            "await" => {
                if let Some(Ok(line)) = lines.next() {
                    if let Ok(Command::Send { lane: l, .. }) = codec::decode::<Command>(&line) {
                        lane = l;
                    }
                }
            }
            // Not protocol. A real adapter does this by printing a
            // diagnostic to stdout instead of stderr.
            // Empty lines are noise a pipe can carry; they are not messages
            // and must not be decoded as one.
            "blank" => {
                let _ = out.write_all(b"\n");
                let _ = out.flush();
            }
            "garbage" => {
                let _ = out.write_all(b"Warning: reconnecting to upstream\n");
                let _ = out.flush();
            }
            "exit" => std::process::exit(arg.parse().unwrap_or(0)),
            // A wedge: output started and then stopped, which is the case
            // the short threshold exists for.
            "stall" => {
                let ms: u64 = arg.parse().unwrap_or(10_000);
                std::thread::sleep(std::time::Duration::from_millis(ms));
            }
            "silent" => {
                // Drain commands and answer nothing: a wedged adapter, which
                // must not be indistinguishable from a slow model.
                for _ in lines.by_ref() {}
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(3600));
                }
            }
            "flood" => {
                let n: usize = arg.parse().unwrap_or(1000);
                for i in 0..n {
                    emit(
                        &mut out,
                        &OutgoingFrame::Event {
                            lane: lane.clone(),
                            event: Outgoing::TextMessageContent {
                                base: Base::default(),
                                message_id: lane.clone(),
                                delta: format!("{i} "),
                            },
                        },
                    );
                }
            }
            other => {
                eprintln!("fake-adapter: unknown act {other:?}");
                std::process::exit(2);
            }
        }
    }
    // Falls off the end: stdout closes, which is how a clean exit looks from
    // Kobold's side.
}
