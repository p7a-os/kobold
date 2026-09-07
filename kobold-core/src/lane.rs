//! Pure lane execution state for the Kobold Kernel.
//!
//! A lane represents an independent execution thread (or pane in the UI)
//! carrying a conversation branch, in-flight turns, tool call assembly,
//! and human interruption questions.

use serde::{Deserialize, Serialize};

use kobold_proto::agui::Incoming;
use kobold_proto::northbound::{AskRecord, LaneStatus, MessageRole, TranscriptRecord};
use kobold_proto::Transport;

use crate::tools;

/// Which party spoke a transcript entry.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Who {
    User,
    Model,
    System,
}

impl Who {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Model => "assistant",
            Self::System => "system",
        }
    }

    pub fn to_message_role(self) -> MessageRole {
        match self {
            Self::User => MessageRole::User,
            Self::Model => MessageRole::Model,
            Self::System => MessageRole::System,
        }
    }
}

/// One entry in a lane's conversation transcript.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct TranscriptEntry {
    pub who: Who,
    pub text: String,
    pub response_id: Option<String>,
}

impl TranscriptEntry {
    pub fn new(who: Who, text: impl Into<String>) -> Self {
        Self {
            who,
            text: text.into(),
            response_id: None,
        }
    }

    pub fn to_record(&self) -> TranscriptRecord {
        TranscriptRecord {
            role: self.who.to_message_role(),
            text: self.text.clone(),
            response_id: self.response_id.clone(),
        }
    }
}

/// A tool call whose arguments are being streamed across multiple events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// Transport link status for a lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Link {
    Connecting,
    Ready,
    Dead,
}

/// Effect produced by applying an event to a lane.
#[derive(Debug, Clone, PartialEq)]
pub enum LaneEffect {
    None,
    Delta(String),
    RunTool(tools::Call),
    Finished,
    Error(Option<String>),
}

/// An isolated conversation lane.
pub struct Lane {
    pub name: String,
    pub branch: String,
    pub parent: Option<(String, usize)>,
    pub transcript: Vec<TranscriptEntry>,
    pub open_calls: Vec<PendingCall>,
    pub outstanding: usize,
    pub link: Link,
    pub failed: bool,
    pub interrupted: bool,
    pub queue: Vec<String>,
    pub questions: Option<tools::Ask>,
    pub logged: usize,
    pub last_response_id: Option<String>,
    open_model: Option<usize>,
}

impl Lane {
    pub fn new(name: impl Into<String>, branch: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            branch: branch.into(),
            parent: None,
            transcript: Vec::new(),
            open_calls: Vec::new(),
            outstanding: 0,
            link: Link::Connecting,
            failed: false,
            interrupted: false,
            queue: Vec::new(),
            questions: None,
            logged: 0,
            last_response_id: None,
            open_model: None,
        }
    }

    pub fn fork(
        new_name: impl Into<String>,
        new_branch: impl Into<String>,
        parent_branch: impl Into<String>,
        parent_idx: usize,
        inherited_transcript: Vec<TranscriptEntry>,
    ) -> Self {
        let parent_b = parent_branch.into();
        Self {
            name: new_name.into(),
            branch: new_branch.into(),
            parent: Some((parent_b, parent_idx)),
            transcript: inherited_transcript,
            open_calls: Vec::new(),
            outstanding: 0,
            link: Link::Ready,
            failed: false,
            interrupted: false,
            queue: Vec::new(),
            questions: None,
            logged: 0,
            last_response_id: None,
            open_model: None,
        }
    }

    pub fn status(&self) -> LaneStatus {
        if self.link == Link::Dead || self.failed {
            LaneStatus::Gone
        } else if self.outstanding > 0 {
            LaneStatus::Waiting
        } else if self.link == Link::Ready {
            LaneStatus::Ready
        } else {
            LaneStatus::Connecting
        }
    }

    pub fn apply_transport(&mut self, transport: &Transport) {
        match transport {
            Transport::Connected => {
                self.link = Link::Ready;
                self.failed = false;
            }
            Transport::Disconnected(_) => {
                self.link = Link::Dead;
                self.open_model = None;
                self.outstanding = 0;
            }
        }
    }

    pub fn apply_event(&mut self, event: Incoming) -> LaneEffect {
        let event = match event {
            Incoming::TextMessageChunk {
                base,
                message_id,
                delta: Some(delta),
                ..
            } => Incoming::TextMessageContent {
                base,
                message_id: message_id.unwrap_or_default(),
                delta,
            },
            other => other,
        };

        match event {
            Incoming::TextMessageContent { delta, .. } => {
                if self.interrupted {
                    return LaneEffect::None;
                }
                match self.open_model.and_then(|i| self.transcript.get_mut(i)) {
                    Some(e) if e.who == Who::Model => e.text.push_str(&delta),
                    _ => {
                        self.open_model = Some(self.transcript.len());
                        self.transcript
                            .push(TranscriptEntry::new(Who::Model, delta.clone()));
                    }
                }
                LaneEffect::Delta(delta)
            }
            Incoming::ToolCallStart {
                tool_call_id,
                tool_call_name,
                ..
            } => {
                self.open_calls.push(PendingCall {
                    id: tool_call_id,
                    name: tool_call_name,
                    arguments: String::new(),
                });
                LaneEffect::None
            }
            Incoming::ToolCallArgs {
                tool_call_id,
                delta,
                ..
            } => {
                if let Some(c) = self.open_calls.iter_mut().find(|c| c.id == tool_call_id) {
                    c.arguments.push_str(&delta);
                }
                LaneEffect::None
            }
            Incoming::ToolCallEnd { tool_call_id, .. } => {
                let Some(at) = self.open_calls.iter().position(|c| c.id == tool_call_id) else {
                    return LaneEffect::None;
                };
                let call = self.open_calls.remove(at);
                LaneEffect::RunTool(tools::Call {
                    id: call.id,
                    name: call.name,
                    arguments: call.arguments,
                })
            }
            Incoming::RunFinished { run_id, .. } => {
                let response_id = (!run_id.is_empty()).then_some(run_id);
                if self.interrupted {
                    self.interrupted = false;
                    self.last_response_id = None;
                } else {
                    self.last_response_id = response_id.clone();
                    if let Some(e) = self.open_model.and_then(|i| self.transcript.get_mut(i)) {
                        e.response_id = response_id;
                    }
                }
                self.open_model = None;
                self.failed = false;
                self.request_settled();
                LaneEffect::Finished
            }
            Incoming::RunError { code, message, .. } => {
                self.request_settled();
                self.failed = true;
                self.transcript.push(TranscriptEntry::new(
                    Who::System,
                    format!("failed: {} {}", code.as_deref().unwrap_or("?"), message),
                ));
                LaneEffect::Error(code)
            }
            _ => LaneEffect::None,
        }
    }

    pub fn push_user(&mut self, text: impl Into<String>) {
        self.transcript.push(TranscriptEntry::new(Who::User, text));
    }

    pub fn push_system(&mut self, text: impl Into<String>) {
        self.transcript
            .push(TranscriptEntry::new(Who::System, text));
    }

    pub fn sent_request(&mut self) {
        self.outstanding += 1;
        self.interrupted = false;
    }

    pub fn request_settled(&mut self) {
        self.outstanding = self.outstanding.saturating_sub(1);
    }

    pub fn replay(&self) -> Vec<(String, String)> {
        self.transcript
            .iter()
            .filter(|e| e.who != Who::System)
            .map(|e| (e.who.as_str().to_owned(), e.text.clone()))
            .collect()
    }

    pub fn active_ask_record(&self) -> Option<AskRecord> {
        self.questions.as_ref().map(|ask| AskRecord {
            call_id: ask.call_id.clone(),
            question: ask.question.clone(),
            options: ask.options.clone(),
            multi_select: ask.multiple,
        })
    }

    pub fn snapshot_records(&self) -> Vec<TranscriptRecord> {
        self.transcript
            .iter()
            .map(TranscriptEntry::to_record)
            .collect()
    }

    pub fn cursor(&self) -> crate::transcript::Cursor<'_> {
        self.cursor_at(self.transcript.len())
    }

    pub fn cursor_at(&self, seq: usize) -> crate::transcript::Cursor<'_> {
        crate::transcript::Cursor {
            branch: &self.branch,
            seq,
            parent: self.parent.as_ref().map(|(b, n)| (b.as_str(), *n)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kobold_proto::agui;

    #[test]
    fn test_lane_lifecycle_and_status() {
        let mut lane = Lane::new("main", "main-br");
        assert_eq!(lane.name, "main");
        assert_eq!(lane.branch, "main-br");
        assert_eq!(lane.status(), LaneStatus::Connecting);

        lane.link = Link::Ready;
        assert_eq!(lane.status(), LaneStatus::Ready);

        lane.push_user("hello");
        lane.sent_request();
        assert_eq!(lane.status(), LaneStatus::Waiting);
        assert_eq!(lane.outstanding, 1);

        lane.request_settled();
        assert_eq!(lane.status(), LaneStatus::Ready);
        assert_eq!(lane.outstanding, 0);

        lane.failed = true;
        assert_eq!(lane.status(), LaneStatus::Gone);

        lane.failed = false;
        lane.link = Link::Dead;
        assert_eq!(lane.status(), LaneStatus::Gone);
    }

    #[test]
    fn test_lane_fork() {
        let mut parent = Lane::new("main", "main-br");
        parent.push_user("msg1");
        parent.push_system("sys1");

        let child = Lane::fork("sub", "sub-br", "main-br", 1, parent.transcript.clone());
        assert_eq!(child.name, "sub");
        assert_eq!(child.branch, "sub-br");
        assert_eq!(child.parent, Some(("main-br".to_string(), 1)));
        assert_eq!(child.transcript.len(), 2);
        assert_eq!(child.status(), LaneStatus::Ready);
    }

    #[test]
    fn test_lane_text_message_stream() {
        let mut lane = Lane::new("main", "main-br");
        lane.link = Link::Ready;
        lane.sent_request();

        let incoming_start = agui::Incoming::TextMessageContent {
            base: agui::Base::default(),
            message_id: "m-1".into(),
            delta: "Hel".into(),
        };
        assert_eq!(
            lane.apply_event(incoming_start),
            LaneEffect::Delta("Hel".into())
        );
        assert_eq!(lane.transcript.len(), 1);
        assert_eq!(lane.transcript[0].text, "Hel");

        let incoming_delta = agui::Incoming::TextMessageContent {
            base: agui::Base::default(),
            message_id: "m-1".into(),
            delta: "lo world".into(),
        };
        assert_eq!(
            lane.apply_event(incoming_delta),
            LaneEffect::Delta("lo world".into())
        );
        assert_eq!(lane.transcript.len(), 1);
        assert_eq!(lane.transcript[0].text, "Hello world");

        let incoming_finish = agui::Incoming::RunFinished {
            base: agui::Base::default(),
            thread_id: "main".into(),
            run_id: "run-1".into(),
            usage: None,
            result: None,
            outcome: None,
        };
        assert_eq!(lane.apply_event(incoming_finish), LaneEffect::Finished);
        assert_eq!(lane.last_response_id.as_deref(), Some("run-1"));
        assert_eq!(lane.status(), LaneStatus::Ready);
    }

    #[test]
    fn test_lane_tool_call_streaming() {
        let mut lane = Lane::new("main", "main-br");
        lane.link = Link::Ready;

        let start = agui::Incoming::ToolCallStart {
            base: agui::Base::default(),
            parent_message_id: None,
            tool_call_id: "c-1".into(),
            tool_call_name: "bash".into(),
        };
        assert_eq!(lane.apply_event(start), LaneEffect::None);
        assert_eq!(lane.open_calls.len(), 1);

        let args = agui::Incoming::ToolCallArgs {
            base: agui::Base::default(),
            tool_call_id: "c-1".into(),
            delta: "{\"command\": \"ls\"}".into(),
        };
        assert_eq!(lane.apply_event(args), LaneEffect::None);
        assert_eq!(lane.open_calls[0].arguments, "{\"command\": \"ls\"}");

        let end = agui::Incoming::ToolCallEnd {
            base: agui::Base::default(),
            tool_call_id: "c-1".into(),
        };
        let effect = lane.apply_event(end);
        assert_eq!(
            effect,
            LaneEffect::RunTool(tools::Call {
                id: "c-1".into(),
                name: "bash".into(),
                arguments: "{\"command\": \"ls\"}".into(),
            })
        );
        assert_eq!(lane.open_calls.len(), 0);
    }

    #[test]
    fn test_lane_run_error() {
        let mut lane = Lane::new("main", "main-br");
        lane.link = Link::Ready;
        lane.sent_request();

        let incoming_err = agui::Incoming::RunError {
            base: agui::Base::default(),
            code: Some("RATE_LIMIT".into()),
            message: "Too many requests".into(),
            usage: None,
        };
        let effect = lane.apply_event(incoming_err);
        assert_eq!(effect, LaneEffect::Error(Some("RATE_LIMIT".into())));
        assert!(lane.failed);
        assert_eq!(lane.status(), LaneStatus::Gone);
        assert_eq!(lane.transcript.last().unwrap().who, Who::System);
        assert!(lane.transcript.last().unwrap().text.contains("RATE_LIMIT"));
    }
}
