//! The Kobold Kernel engine.
//!
//! Owns the execution loop, multi-lane management, adapter subprocess supervision,
//! transcript persistence, tool execution, and broadcasting to frontends over the
//! Northbound AG-UI seam.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use kobold_proto::agui;
use kobold_proto::northbound::{ClientFrame, LaneStatus, ServerFrame};
use kobold_proto::{Command, IncomingFrame, Transport};

use crate::lane::{Lane, LaneEffect, Who};
use crate::tools;
use crate::transcript;

/// How long to wait before warning the user that a model has not produced its first delta.
pub const FIRST_UPDATE_NOTICE: Duration = Duration::from_secs(435);

/// How long mid-stream silence may last before considering the adapter wedged.
pub const STREAM_STALL: Duration = Duration::from_secs(5);

/// Classification of silence from an adapter during an in-flight turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Silence {
    Fine,
    Notice,
    Wedged,
}

/// Tracks turn timing and silence thresholds.
#[derive(Debug)]
pub struct SilenceWatch {
    last_update: Instant,
    streaming: bool,
    noticed: bool,
    was_busy: bool,
}

impl SilenceWatch {
    pub fn new(now: Instant) -> Self {
        Self {
            last_update: now,
            streaming: false,
            noticed: false,
            was_busy: false,
        }
    }

    pub fn turn_boundary(&mut self, busy: bool, now: Instant) {
        if busy && !self.was_busy {
            self.last_update = now;
            self.streaming = false;
            self.noticed = false;
        }
        self.was_busy = busy;
    }

    pub fn saw_update(&mut self, was_delta: bool, now: Instant) {
        self.streaming |= was_delta;
        self.last_update = now;
    }

    pub fn check(&self, busy: bool, now: Instant) -> Silence {
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

    pub fn silent_for(&self, now: Instant) -> Duration {
        now.duration_since(self.last_update)
    }
}

/// Headless Kernel orchestrating multi-lane execution.
pub struct Kernel {
    pub lanes: Vec<Lane>,
    pub active_lane: usize,
    pub log: transcript::Log,
    pub mcp: tools::Sources,
    pub root: PathBuf,
    pub silence: SilenceWatch,
}

impl Kernel {
    pub fn new(
        initial_lane: impl Into<String>,
        initial_branch: impl Into<String>,
        root: impl AsRef<Path>,
    ) -> Self {
        let lane_name = initial_lane.into();
        let branch_name = initial_branch.into();
        let log_dir = root.as_ref().join(transcript::DIR);
        let log = transcript::Log::open(&log_dir.join(transcript::FILE));

        Self {
            lanes: vec![Lane::new(lane_name, branch_name)],
            active_lane: 0,
            log,
            mcp: tools::Sources::new(),
            root: root.as_ref().to_path_buf(),
            silence: SilenceWatch::new(Instant::now()),
        }
    }

    pub fn lane(&self, name: &str) -> Option<&Lane> {
        self.lanes.iter().find(|l| l.name == name)
    }

    pub fn lane_mut(&mut self, name: &str) -> Option<&mut Lane> {
        self.lanes.iter_mut().find(|l| l.name == name)
    }

    pub fn active_lane(&self) -> &Lane {
        &self.lanes[self.active_lane]
    }

    pub fn active_lane_mut(&mut self) -> &mut Lane {
        &mut self.lanes[self.active_lane]
    }

    pub fn is_busy(&self) -> bool {
        self.lanes.iter().any(|l| l.status() == LaneStatus::Waiting)
    }

    pub fn apply_transport(&mut self, transport: Transport) -> Vec<ServerFrame> {
        let mut frames = Vec::new();
        for lane in &mut self.lanes {
            lane.apply_transport(&transport);
            frames.push(ServerFrame::StatusChange {
                lane: lane.name.clone(),
                status: lane.status(),
            });
        }
        frames
    }

    /// Handles a client command frame from a connected frontend.
    pub async fn handle_client_frame(
        &mut self,
        frame: ClientFrame,
        commands: &mpsc::UnboundedSender<Command>,
    ) -> Vec<ServerFrame> {
        let mut outbound = Vec::new();

        match frame {
            ClientFrame::Prompt { lane, text } => {
                if let Some(l) = self.lane_mut(&lane) {
                    if l.status() == LaneStatus::Waiting {
                        l.queue.push(text);
                    } else {
                        outbound.extend(self.dispatch_prompt(&lane, text, commands));
                    }
                }
            }
            ClientFrame::SubmitInterrupt {
                lane,
                call_id,
                answers,
            } => {
                if let Some(l) = self.lane_mut(&lane) {
                    l.questions = None;
                    let output = answers.join(", ");
                    l.sent_request();
                    let _ = commands.send(Command::ToolResult {
                        lane: lane.clone(),
                        call_id,
                        output,
                        error: false,
                    });
                    outbound.push(ServerFrame::StatusChange {
                        lane,
                        status: l.status(),
                    });
                }
            }
            ClientFrame::CancelInterrupt { lane, call_id } => {
                if let Some(l) = self.lane_mut(&lane) {
                    l.questions = None;
                    l.sent_request();
                    let _ = commands.send(Command::ToolResult {
                        lane: lane.clone(),
                        call_id,
                        output: "User declined or cancelled the prompt.".into(),
                        error: true,
                    });
                    outbound.push(ServerFrame::StatusChange {
                        lane,
                        status: l.status(),
                    });
                }
            }
            ClientFrame::Fork {
                new_lane,
                parent_branch,
                parent_at,
            } => {
                let inherited = self
                    .lanes
                    .iter()
                    .find(|l| l.branch == parent_branch)
                    .map(|l| l.transcript[..parent_at.min(l.transcript.len())].to_vec())
                    .unwrap_or_default();

                let new_branch = format!("{parent_branch}-fork-{}", self.lanes.len());
                let forked =
                    Lane::fork(&new_lane, &new_branch, &parent_branch, parent_at, inherited);
                let snapshot = ServerFrame::Snapshot {
                    lane: forked.name.clone(),
                    branch: forked.branch.clone(),
                    messages: forked.snapshot_records(),
                    active_interrupt: forked.active_ask_record(),
                    status: forked.status(),
                };
                self.lanes.push(forked);
                outbound.push(snapshot);
            }
            ClientFrame::CancelTurn { lane } => {
                if let Some(l) = self.lane_mut(&lane) {
                    l.interrupted = true;
                    l.request_settled();
                    let _ = commands.send(Command::Cancel { lane: lane.clone() });
                    outbound.push(ServerFrame::StatusChange {
                        lane,
                        status: l.status(),
                    });
                }
            }
            ClientFrame::SyncRequest { lane } => {
                if let Some(l) = self.lane(&lane) {
                    outbound.push(ServerFrame::Snapshot {
                        lane: l.name.clone(),
                        branch: l.branch.clone(),
                        messages: l.snapshot_records(),
                        active_interrupt: l.active_ask_record(),
                        status: l.status(),
                    });
                }
            }
            ClientFrame::Detach => {}
        }

        outbound
    }

    /// Dispatches a prompt to an idle lane.
    fn dispatch_prompt(
        &mut self,
        lane_name: &str,
        text: String,
        commands: &mpsc::UnboundedSender<Command>,
    ) -> Vec<ServerFrame> {
        let mut outbound = Vec::new();
        let (replay, prev) = {
            let Some(lane) = self.lanes.iter_mut().find(|l| l.name == lane_name) else {
                return outbound;
            };
            let cursor = lane.cursor();
            self.log.append(cursor, Who::User, &text);
            lane.logged += 1;
            let replay = lane.replay();
            let prev = lane.last_response_id.clone();
            lane.push_user(&text);
            lane.sent_request();
            (replay, prev)
        };

        let _ = commands.send(Command::Send {
            lane: lane_name.to_owned(),
            text,
            previous_response_id: prev,
            replay,
        });

        if let Some(lane) = self.lane(lane_name) {
            outbound.push(ServerFrame::StatusChange {
                lane: lane_name.to_owned(),
                status: lane.status(),
            });
        }

        outbound
    }

    /// Ingests an incoming frame from the adapter subprocess.
    pub async fn handle_adapter_frame(
        &mut self,
        frame: IncomingFrame,
        commands: &mpsc::UnboundedSender<Command>,
    ) -> Vec<ServerFrame> {
        let mut outbound = Vec::new();

        match frame {
            IncomingFrame::Transport(transport) => {
                outbound.extend(self.apply_transport(transport));
            }
            IncomingFrame::Event { lane, event } => {
                if !self.lanes.iter().any(|l| l.name == lane) {
                    return outbound;
                }

                let was_delta = matches!(event, agui::Incoming::TextMessageContent { .. });
                self.silence.saw_update(was_delta, Instant::now());

                let effect = {
                    let Some(l) = self.lane_mut(&lane) else {
                        return outbound;
                    };
                    l.apply_event(event)
                };

                match effect {
                    LaneEffect::None => {}
                    LaneEffect::Delta(delta) => {
                        outbound.push(ServerFrame::Event {
                            lane: lane.clone(),
                            event: agui::Outgoing::TextMessageContent {
                                base: agui::Base::default(),
                                message_id: lane.clone(),
                                delta,
                            },
                        });
                    }
                    LaneEffect::RunTool(call) => {
                        let source =
                            (!self.mcp.is_empty()).then_some(&self.mcp as &dyn tools::McpTools);
                        let outcome = tools::run(&self.root, &call, source).await;
                        match tools::decide(outcome) {
                            tools::Dispatch::Reply { output, error } => {
                                if let Some(l) = self.lane_mut(&lane) {
                                    l.sent_request();
                                }
                                let _ = commands.send(Command::ToolResult {
                                    lane: lane.clone(),
                                    call_id: call.id,
                                    output,
                                    error,
                                });
                            }
                            tools::Dispatch::Park(ask) => {
                                if let Some(l) = self.lane_mut(&lane) {
                                    l.questions = Some(ask.clone());
                                    outbound.push(ServerFrame::Snapshot {
                                        lane: l.name.clone(),
                                        branch: l.branch.clone(),
                                        messages: l.snapshot_records(),
                                        active_interrupt: l.active_ask_record(),
                                        status: l.status(),
                                    });
                                }
                            }
                        }
                    }
                    LaneEffect::Finished => {
                        let maybe_model =
                            self.lanes.iter().find(|l| l.name == lane).and_then(|l| {
                                l.transcript
                                    .last()
                                    .filter(|e| e.who == Who::Model)
                                    .map(|last| {
                                        (
                                            l.branch.clone(),
                                            l.parent.clone(),
                                            l.transcript.len().saturating_sub(1),
                                            last.text.clone(),
                                        )
                                    })
                            });
                        if let Some((branch, parent, seq, text)) = maybe_model {
                            let parent_ref = parent.as_ref().map(|(b, n)| (b.as_str(), *n));
                            let cursor = crate::transcript::Cursor {
                                branch: &branch,
                                seq,
                                parent: parent_ref,
                            };
                            self.log.append(cursor, Who::Model, &text);
                            if let Some(l) = self.lanes.iter_mut().find(|l| l.name == lane) {
                                l.logged += 1;
                            }
                        }
                        outbound.push(ServerFrame::Event {
                            lane: lane.clone(),
                            event: agui::Outgoing::RunFinished {
                                base: agui::Base::default(),
                                thread_id: lane.clone(),
                                run_id: String::new(),
                                usage: None,
                            },
                        });
                        outbound.push(ServerFrame::StatusChange {
                            lane: lane.clone(),
                            status: self
                                .lane(&lane)
                                .map(|l| l.status())
                                .unwrap_or(LaneStatus::Ready),
                        });
                        // Drain queued messages
                        outbound.extend(self.drain_lane_queue(&lane, commands));
                    }
                    LaneEffect::Error(code) => {
                        outbound.push(ServerFrame::Event {
                            lane: lane.clone(),
                            event: agui::Outgoing::RunError {
                                base: agui::Base::default(),
                                message: "turn failed".into(),
                                code,
                                usage: None,
                            },
                        });
                        outbound.push(ServerFrame::StatusChange {
                            lane: lane.clone(),
                            status: self
                                .lane(&lane)
                                .map(|l| l.status())
                                .unwrap_or(LaneStatus::Gone),
                        });
                    }
                }
            }
        }

        outbound
    }

    fn drain_lane_queue(
        &mut self,
        lane_name: &str,
        commands: &mpsc::UnboundedSender<Command>,
    ) -> Vec<ServerFrame> {
        let mut outbound = Vec::new();
        let Some(lane) = self.lane_mut(lane_name) else {
            return outbound;
        };

        if lane.status() == LaneStatus::Ready && !lane.queue.is_empty() {
            let next_text = lane.queue.remove(0);
            outbound.extend(self.dispatch_prompt(lane_name, next_text, commands));
        }

        outbound
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_silence_watch() {
        let start = Instant::now();
        let mut watch = SilenceWatch::new(start);

        // Not busy -> Fine
        assert_eq!(watch.check(false, start), Silence::Fine);

        // Turn boundary: start turn
        watch.turn_boundary(true, start);
        assert_eq!(watch.check(true, start), Silence::Fine);

        // Before first update notice
        let t1 = start + Duration::from_secs(100);
        assert_eq!(watch.check(true, t1), Silence::Fine);

        // Past first update notice
        let t2 = start + FIRST_UPDATE_NOTICE + Duration::from_secs(1);
        assert_eq!(watch.check(true, t2), Silence::Notice);

        // Delta arrives, transitions to streaming
        watch.saw_update(true, t2);
        assert_eq!(watch.check(true, t2), Silence::Fine);

        // Mid-stream stall
        let t3 = t2 + STREAM_STALL + Duration::from_millis(100);
        assert_eq!(watch.check(true, t3), Silence::Wedged);
    }

    #[tokio::test]
    async fn test_kernel_initialization_and_sync() {
        let dir = tempdir().unwrap();
        let mut kernel = Kernel::new("main", "main", dir.path());

        assert_eq!(kernel.lanes.len(), 1);
        assert_eq!(kernel.lanes[0].name, "main");
        assert_eq!(kernel.lanes[0].branch, "main");

        let (cmd_tx, _cmd_rx) = mpsc::unbounded_channel();
        let frames = kernel
            .handle_client_frame(
                ClientFrame::SyncRequest {
                    lane: "main".into(),
                },
                &cmd_tx,
            )
            .await;
        assert_eq!(frames.len(), 1);
        match &frames[0] {
            ServerFrame::Snapshot {
                lane,
                branch,
                status,
                ..
            } => {
                assert_eq!(lane, "main");
                assert_eq!(branch, "main");
                assert_eq!(*status, LaneStatus::Connecting);
            }
            other => panic!("expected Snapshot, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_kernel_fork_lane() {
        let dir = tempdir().unwrap();
        let mut kernel = Kernel::new("main", "main", dir.path());
        let (cmd_tx, _cmd_rx) = mpsc::unbounded_channel();

        // Fork lane
        let frames = kernel
            .handle_client_frame(
                ClientFrame::Fork {
                    new_lane: "experiment".into(),
                    parent_branch: "main".into(),
                    parent_at: 0,
                },
                &cmd_tx,
            )
            .await;

        assert_eq!(kernel.lanes.len(), 2);
        assert_eq!(kernel.lanes[1].name, "experiment");
        assert_eq!(frames.len(), 1);
        assert!(matches!(&frames[0], ServerFrame::Snapshot { lane, .. } if lane == "experiment"));
    }

    #[tokio::test]
    async fn test_kernel_prompt_dispatch_and_queue() {
        let dir = tempdir().unwrap();
        let mut kernel = Kernel::new("main", "main", dir.path());
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();

        // Mark main lane as Ready
        kernel.lanes[0].link = crate::lane::Link::Ready;

        // Dispatch first prompt
        let frames1 = kernel
            .handle_client_frame(
                ClientFrame::Prompt {
                    lane: "main".into(),
                    text: "First request".into(),
                },
                &cmd_tx,
            )
            .await;

        assert_eq!(frames1.len(), 1);
        assert!(matches!(
            &frames1[0],
            ServerFrame::StatusChange {
                status: LaneStatus::Waiting,
                ..
            }
        ));
        assert_eq!(kernel.lanes[0].status(), LaneStatus::Waiting);

        // Command should have been sent to adapter
        let cmd1 = cmd_rx.recv().await.expect("received command");
        match cmd1 {
            Command::Send { lane, text, .. } => {
                assert_eq!(lane, "main");
                assert_eq!(text, "First request");
            }
            other => panic!("expected Command::Send, got {:?}", other),
        }

        // Dispatch second prompt while lane is Waiting (should be queued)
        let frames2 = kernel
            .handle_client_frame(
                ClientFrame::Prompt {
                    lane: "main".into(),
                    text: "Second request queued".into(),
                },
                &cmd_tx,
            )
            .await;

        assert_eq!(frames2.len(), 0);
        assert_eq!(kernel.lanes[0].queue.len(), 1);
        assert_eq!(kernel.lanes[0].queue[0], "Second request queued");

        // Simulate adapter incoming streaming & finish
        let chunk = IncomingFrame::Event {
            lane: "main".into(),
            event: agui::Incoming::TextMessageContent {
                base: agui::Base::default(),
                message_id: "m-1".into(),
                delta: "Response chunk".into(),
            },
        };
        let frames_chunk = kernel.handle_adapter_frame(chunk, &cmd_tx).await;
        assert_eq!(frames_chunk.len(), 1);

        let finish = IncomingFrame::Event {
            lane: "main".into(),
            event: agui::Incoming::RunFinished {
                base: agui::Base::default(),
                thread_id: "main".into(),
                run_id: "resp-1".into(),
                usage: None,
                result: None,
                outcome: None,
            },
        };
        let frames_finish = kernel.handle_adapter_frame(finish, &cmd_tx).await;
        // Finished event + StatusChange(Ready) + drained prompt dispatch (StatusChange(Waiting))
        assert!(frames_finish
            .iter()
            .any(|f| matches!(f, ServerFrame::Event { .. })));

        // Queue should now be drained and sent to adapter!
        assert_eq!(kernel.lanes[0].queue.len(), 0);
        let cmd2 = cmd_rx.recv().await.expect("second command drained");
        match cmd2 {
            Command::Send { lane, text, .. } => {
                assert_eq!(lane, "main");
                assert_eq!(text, "Second request queued");
            }
            other => panic!("expected Command::Send, got {:?}", other),
        }
    }
}
