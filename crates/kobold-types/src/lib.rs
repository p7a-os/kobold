//! Pure data models, interfaces, and trait contracts for the Kobold thin agent harness.
//!
//! This crate contains zero operational logic, zero network I/O, and zero process/filesystem dependencies.

pub mod backend;
pub mod error;
pub mod event;
pub mod message;
pub mod policy;
pub mod tool;

// Public re-exports for convenient top-level access
pub use backend::{Backend, BackendEvent, BackendStream, TokenUsage};
pub use error::{BackendError, EventSinkError, ToolError};
pub use event::{EventSink, KernelEvent, NoopEventSink, TurnFinishReason};
pub use message::{ContentPart, Message, Role};
pub use policy::{AllowAllPolicy, ApprovalAction, ApprovalPolicy, DenyAllPolicy};
pub use tool::{PruningPolicy, Tool, ToolCall, ToolDefinition, ToolOutput};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_serialization_roundtrip() {
        let msg = Message::assistant_with_thought("planning step", "I will list the directory.");
        let serialized = serde_json::to_string(&msg).expect("failed to serialize message");
        let deserialized: Message =
            serde_json::from_str(&serialized).expect("failed to deserialize message");

        assert_eq!(msg, deserialized);
        assert_eq!(deserialized.text(), "I will list the directory.");
        assert_eq!(deserialized.thought(), Some("planning step".to_string()));
    }

    #[test]
    fn tool_call_and_output_roundtrip() {
        let call = ToolCall::new("call_123", "bash", r#"{"command":"cargo check"}"#);
        let serialized = serde_json::to_string(&call).expect("failed to serialize tool call");
        let deserialized: ToolCall =
            serde_json::from_str(&serialized).expect("failed to deserialize tool call");
        assert_eq!(call, deserialized);

        let output = ToolOutput::error("call_123", "command failed with exit code 1")
            .with_head_tail(5, 5);
        let serialized_output =
            serde_json::to_string(&output).expect("failed to serialize tool output");
        let deserialized_output: ToolOutput =
            serde_json::from_str(&serialized_output).expect("failed to deserialize tool output");
        assert_eq!(output, deserialized_output);
        assert!(deserialized_output.is_error);
        assert_eq!(
            deserialized_output.pruning,
            PruningPolicy::HeadTail {
                head_lines: 5,
                tail_lines: 5
            }
        );

        let read_output = ToolOutput::success("call_456", "content of main.rs")
            .with_keep_last("src/main.rs");
        assert_eq!(
            read_output.pruning,
            PruningPolicy::KeepLast {
                key: "src/main.rs".to_string()
            }
        );
    }

    #[test]
    fn kernel_event_serialization_roundtrip() {
        let event = KernelEvent::ApprovalRequested {
            call: ToolCall::new("call_1", "bash", r#"{"command":"rm -rf /"}"#),
            reason: "destructive command requires user confirmation".to_string(),
        };
        let serialized = serde_json::to_string(&event).expect("failed to serialize event");
        let deserialized: KernelEvent =
            serde_json::from_str(&serialized).expect("failed to deserialize event");
        assert_eq!(event, deserialized);
    }

    #[test]
    fn backend_event_serialization_roundtrip() {
        let event = BackendEvent::Finished {
            finish_reason: TurnFinishReason::Stop,
            usage: Some(TokenUsage::new(150, 42)),
        };
        let serialized = serde_json::to_string(&event).expect("failed to serialize backend event");
        let deserialized: BackendEvent =
            serde_json::from_str(&serialized).expect("failed to deserialize backend event");
        assert_eq!(event, deserialized);
    }

    // Compile-time test confirming all core traits are dyn-compatible (object-safe)
    #[allow(dead_code)]
    fn assert_dyn_compatible(
        _backend: Box<dyn Backend>,
        _tool: Box<dyn Tool>,
        _sink: Box<dyn EventSink>,
        _policy: Box<dyn ApprovalPolicy>,
    ) {
    }
}
