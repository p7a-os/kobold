//! Northbound IPC protocol between the Kobold Kernel daemon (`koboldd`)
//! and frontends (TUI, Web, Desktop, Mobile, Headless).
//!
//! # Architecture
//!
//! - **Client $\to$ Kernel (`ClientFrame`)**: Commands issued by a frontend to
//!   the kernel (prompts, interrupt resolutions, forks, rewinds, sync requests).
//! - **Kernel $\to$ Client (`ServerFrame` / `ClientServerFrame`)**: Events
//!   streamed from the kernel to frontends. `ServerFrame` is the serialize-only
//!   type written by the kernel (carrying `agui::Outgoing`), while
//!   `ClientServerFrame` is the deserialize-only type read by frontends
//!   (carrying `agui::Incoming` with a forward-compatible `Other` catch-all).

use serde::{Deserialize, Serialize};

use crate::agui;

/// Status of an execution lane as seen by connected frontends.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneStatus {
    Connecting,
    Ready,
    Waiting,
    Gone,
}

/// Role of a message in a conversation transcript.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageRole {
    User,
    Model,
    System,
}

/// One turn in a lane's history, for snapshot hydration.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct TranscriptRecord {
    pub role: MessageRole,
    pub text: String,
    pub response_id: Option<String>,
}

/// An active question/interrupt waiting for human input.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct AskRecord {
    pub call_id: String,
    pub question: String,
    pub options: Vec<String>,
    pub multi_select: bool,
}

/// Client $\to$ Kernel command frame.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum ClientFrame {
    /// Submit a user prompt to a lane.
    Prompt { lane: String, text: String },
    /// Submit answers to an active interrupt (`Ask` / `session/request_permission`).
    SubmitInterrupt {
        lane: String,
        call_id: String,
        answers: Vec<String>,
    },
    /// Decline/cancel an active interrupt.
    CancelInterrupt { lane: String, call_id: String },
    /// Fork a new lane from an existing branch at a specific message index.
    Fork {
        new_lane: String,
        parent_branch: String,
        parent_at: usize,
    },
    /// Cancel an in-flight turn on a lane.
    CancelTurn { lane: String },
    /// Request full state snapshot for lane hydration.
    SyncRequest { lane: String },
    /// Graceful detach notification (frontend closing, kernel keeps running).
    Detach,
}

/// Kernel $\to$ Client frame, written by the kernel.
#[derive(Serialize, Debug, Clone, PartialEq)]
pub enum ServerFrame {
    /// Multiplexed AG-UI event belonging to a specific lane.
    Event { lane: String, event: agui::Outgoing },
    /// Link/connection status update for a lane.
    StatusChange { lane: String, status: LaneStatus },
    /// Full hydration snapshot sent upon connection or sync request.
    Snapshot {
        lane: String,
        branch: String,
        messages: Vec<TranscriptRecord>,
        active_interrupt: Option<AskRecord>,
        status: LaneStatus,
    },
    /// Kernel-level notice (e.g. silence warnings, sandbox notifications).
    Notice { text: String },
    /// Fatal or session-level error.
    Error { message: String },
}

/// Kernel $\to$ Client frame, read by frontends.
#[derive(Deserialize, Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum ClientServerFrame {
    /// Multiplexed AG-UI event belonging to a specific lane.
    Event { lane: String, event: agui::Incoming },
    /// Link/connection status update for a lane.
    StatusChange { lane: String, status: LaneStatus },
    /// Full hydration snapshot sent upon connection or sync request.
    Snapshot {
        lane: String,
        branch: String,
        messages: Vec<TranscriptRecord>,
        active_interrupt: Option<AskRecord>,
        status: LaneStatus,
    },
    /// Kernel-level notice.
    Notice { text: String },
    /// Fatal or session-level error.
    Error { message: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec;

    #[test]
    fn client_frame_round_trip() {
        let cases = vec![
            ClientFrame::Prompt {
                lane: "main".into(),
                text: "hello".into(),
            },
            ClientFrame::SubmitInterrupt {
                lane: "lane-1".into(),
                call_id: "c-123".into(),
                answers: vec!["yes".into(), "option-2".into()],
            },
            ClientFrame::CancelInterrupt {
                lane: "lane-1".into(),
                call_id: "c-123".into(),
            },
            ClientFrame::Fork {
                new_lane: "fork-a".into(),
                parent_branch: "b0".into(),
                parent_at: 5,
            },
            ClientFrame::CancelTurn {
                lane: "main".into(),
            },
            ClientFrame::SyncRequest {
                lane: "main".into(),
            },
            ClientFrame::Detach,
        ];

        for c in cases {
            let encoded = codec::encode(&c).expect("encode client frame");
            let decoded: ClientFrame = codec::decode(&encoded).expect("decode client frame");
            assert_eq!(c, decoded);
        }
    }

    #[test]
    fn server_frame_serializes_and_deserializes_into_client_frame() {
        let server_frame = ServerFrame::StatusChange {
            lane: "main".into(),
            status: LaneStatus::Ready,
        };
        let encoded = codec::encode(&server_frame).expect("encode server frame");
        let client_frame: ClientServerFrame =
            codec::decode(&encoded).expect("decode client server frame");
        assert_eq!(
            client_frame,
            ClientServerFrame::StatusChange {
                lane: "main".into(),
                status: LaneStatus::Ready,
            }
        );
    }

    #[test]
    fn server_snapshot_round_trips_intact() {
        let snapshot = ServerFrame::Snapshot {
            lane: "lane-2".into(),
            branch: "branch-b".into(),
            messages: vec![
                TranscriptRecord {
                    role: MessageRole::User,
                    text: "read file".into(),
                    response_id: None,
                },
                TranscriptRecord {
                    role: MessageRole::Model,
                    text: "file contents".into(),
                    response_id: Some("resp-1".into()),
                },
            ],
            active_interrupt: Some(AskRecord {
                call_id: "call-9".into(),
                question: "overwrite?".into(),
                options: vec!["yes".into(), "no".into()],
                multi_select: false,
            }),
            status: LaneStatus::Waiting,
        };

        let encoded = codec::encode(&snapshot).expect("encode snapshot");
        let decoded: ClientServerFrame = codec::decode(&encoded).expect("decode snapshot");

        if let ClientServerFrame::Snapshot {
            lane,
            branch,
            messages,
            active_interrupt,
            status,
        } = decoded
        {
            assert_eq!(lane, "lane-2");
            assert_eq!(branch, "branch-b");
            assert_eq!(messages.len(), 2);
            assert_eq!(messages[0].text, "read file");
            assert_eq!(messages[1].response_id.as_deref(), Some("resp-1"));
            assert_eq!(active_interrupt.unwrap().options, vec!["yes", "no"]);
            assert_eq!(status, LaneStatus::Waiting);
        } else {
            panic!("expected Snapshot variant, got {decoded:?}");
        }
    }

    #[test]
    fn server_event_round_trips_to_incoming_agui_event() {
        let frame = ServerFrame::Event {
            lane: "main".into(),
            event: agui::Outgoing::TextMessageContent {
                base: agui::Base::default(),
                message_id: "msg-1".into(),
                delta: "hello world".into(),
            },
        };

        let encoded = codec::encode(&frame).expect("encode event");
        let decoded: ClientServerFrame = codec::decode(&encoded).expect("decode event");

        if let ClientServerFrame::Event { lane, event } = decoded {
            assert_eq!(lane, "main");
            if let agui::Incoming::TextMessageContent {
                message_id, delta, ..
            } = event
            {
                assert_eq!(message_id, "msg-1");
                assert_eq!(delta, "hello world");
            } else {
                panic!("expected TextMessageContent, got {event:?}");
            }
        } else {
            panic!("expected Event variant, got {decoded:?}");
        }
    }
}
