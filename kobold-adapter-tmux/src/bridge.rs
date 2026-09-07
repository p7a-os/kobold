//! Southbound adapter bridge translating between Kobold Commands and PTY events.

use std::time::Duration;
use tokio::sync::mpsc;

use kobold_proto::agui;
use kobold_proto::{Command, OutgoingFrame};

use crate::ansi::strip_ansi;
use crate::prompts::{detect_prompt, translate_answer_to_keystrokes, DetectedPrompt};
use crate::pty::PtySession;

/// Bridges Southbound Commands with an interactive PTY session.
pub struct TmuxBridge {
    session: PtySession,
    lane: String,
    active_prompt: Option<(String, DetectedPrompt)>, // (call_id, prompt)
}

impl TmuxBridge {
    pub fn new(session: PtySession, initial_lane: &str) -> Self {
        Self {
            session,
            lane: initial_lane.to_string(),
            active_prompt: None,
        }
    }

    /// Handles an incoming Southbound Command from `koboldd`.
    pub async fn handle_command(
        &mut self,
        cmd: Command,
        out_tx: &mpsc::UnboundedSender<OutgoingFrame>,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        match cmd {
            Command::Send { lane, text, .. } => {
                self.lane = lane.clone();
                let run_id = uuid::Uuid::now_v7().to_string();

                let start_frame = OutgoingFrame::Event {
                    lane: self.lane.clone(),
                    event: agui::Outgoing::RunStarted {
                        base: agui::Base::default(),
                        run_id: run_id.clone(),
                        thread_id: self.lane.clone(),
                        parent_run_id: None,
                    },
                };
                let _ = out_tx.send(start_frame);

                // Write text to the PTY
                self.session.write_all(&format!("{text}\n"))?;

                // Read output deltas and check for prompts
                self.drain_turn(&run_id, out_tx).await?;
                Ok(true)
            }
            Command::ToolResult {
                lane,
                call_id,
                output,
                ..
            } => {
                self.lane = lane;
                if let Some((active_id, _)) = self.active_prompt.take() {
                    if active_id == call_id {
                        let keystrokes = translate_answer_to_keystrokes(&output);
                        self.session.write_all(&keystrokes)?;

                        let run_id = uuid::Uuid::now_v7().to_string();
                        self.drain_turn(&run_id, out_tx).await?;
                    }
                }
                Ok(true)
            }
            Command::Cancel { lane } => {
                self.lane = lane;
                self.active_prompt = None;
                let _ = self.session.write_all("\x03");
                Ok(true)
            }
            Command::Quit => {
                self.session.terminate();
                Ok(false)
            }
        }
    }

    /// Drains output from the PTY until an idle period occurs, a prompt is intercepted, or EOF.
    async fn drain_turn(
        &mut self,
        run_id: &str,
        out_tx: &mpsc::UnboundedSender<OutgoingFrame>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut accumulated_tail = String::new();
        let idle_timeout = std::env::var("KOBOLD_TMUX_IDLE_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .map(Duration::from_millis)
            .unwrap_or(Duration::from_millis(1000));

        loop {
            tokio::select! {
                maybe_bytes = self.session.read_bytes() => {
                    match maybe_bytes {
                        Some(bytes) => {
                            let text = String::from_utf8_lossy(&bytes);
                            let clean = strip_ansi(&text);
                            if !clean.is_empty() {
                                accumulated_tail.push_str(&clean);

                                let _ = out_tx.send(OutgoingFrame::Event {
                                    lane: self.lane.clone(),
                                    event: agui::Outgoing::TextMessageContent {
                                        base: agui::Base::default(),
                                        message_id: uuid::Uuid::now_v7().to_string(),
                                        delta: clean,
                                    },
                                });

                                // Check if the tail contains an interactive confirmation prompt
                                if let Some(prompt) = detect_prompt(&accumulated_tail) {
                                    let call_id = format!("ask-{}", uuid::Uuid::now_v7());
                                    self.active_prompt = Some((call_id.clone(), prompt.clone()));

                                    let args = sonic_rs::to_string(&sonic_rs::json!({
                                        "question": prompt.question,
                                        "options": prompt.options,
                                    }))?;

                                    let _ = out_tx.send(OutgoingFrame::Event {
                                        lane: self.lane.clone(),
                                        event: agui::Outgoing::ToolCallStart {
                                            base: agui::Base::default(),
                                            tool_call_id: call_id.clone(),
                                            tool_call_name: "ask".into(),
                                            parent_message_id: None,
                                        },
                                    });
                                    let _ = out_tx.send(OutgoingFrame::Event {
                                        lane: self.lane.clone(),
                                        event: agui::Outgoing::ToolCallArgs {
                                            base: agui::Base::default(),
                                            tool_call_id: call_id.clone(),
                                            delta: args,
                                        },
                                    });
                                    let _ = out_tx.send(OutgoingFrame::Event {
                                        lane: self.lane.clone(),
                                        event: agui::Outgoing::ToolCallEnd {
                                            base: agui::Base::default(),
                                            tool_call_id: call_id,
                                        },
                                    });

                                    // Pause draining: human answer required via ToolResult
                                    return Ok(());
                                }
                            }
                        }
                        None => {
                            // PTY closed / child exited
                            break;
                        }
                    }
                }
                _ = tokio::time::sleep(idle_timeout) => {
                    // Output became idle; turn is complete
                    break;
                }
            }
        }

        // Emit RunFinished
        let _ = out_tx.send(OutgoingFrame::Event {
            lane: self.lane.clone(),
            event: agui::Outgoing::RunFinished {
                base: agui::Base::default(),
                run_id: run_id.to_string(),
                thread_id: self.lane.clone(),
                usage: None,
            },
        });

        Ok(())
    }
}
