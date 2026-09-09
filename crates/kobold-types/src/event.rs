use serde::{Deserialize, Serialize};

use crate::backend::TokenUsage;
use crate::error::EventSinkError;
use crate::tool::{ToolCall, ToolOutput};

/// Reason why a turn or inference step finished.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnFinishReason {
    /// Normal natural completion by the model.
    Stop,

    /// Model requested one or more tool calls.
    ToolCalls,

    /// Context window limit reached.
    Length,

    /// Turn halted due to an unrecoverable error.
    Error(String),

    /// Turn execution was explicitly cancelled by client or supervisor.
    Cancelled,
}

/// Lifecycle events emitted by `kobold-kernel` during agent turn processing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum KernelEvent {
    /// A new turn has begun.
    TurnStarted { turn_index: usize },

    /// Incremental content delta for assistant response.
    TextDelta { delta: String },

    /// Incremental internal reasoning delta.
    ThoughtDelta { delta: String },

    /// The kernel paused execution waiting for user authorization for a tool call.
    ApprovalRequested { call: ToolCall, reason: String },

    /// An approval request was resolved by the user or policy.
    ApprovalResolved { call_id: String, approved: bool },

    /// A tool invocation has started execution.
    ToolExecutionStarted { call: ToolCall },

    /// A tool invocation has completed execution.
    ToolExecutionCompleted { output: ToolOutput },

    /// The active turn has concluded.
    TurnCompleted {
        finish_reason: TurnFinishReason,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<TokenUsage>,
    },

    /// An error occurred during the turn lifecycle.
    Error { error: String },
}

/// Interface contract for streaming lifecycle events from the kernel.
#[async_trait::async_trait]
pub trait EventSink: Send + Sync {
    /// Emit a kernel event to the observer or subscriber channel.
    async fn emit(&self, event: KernelEvent) -> Result<(), EventSinkError>;
}

/// A no-op event sink that discards all emitted events.
#[derive(Debug, Clone, Default)]
pub struct NoopEventSink;

#[async_trait::async_trait]
impl EventSink for NoopEventSink {
    async fn emit(&self, _event: KernelEvent) -> Result<(), EventSinkError> {
        Ok(())
    }
}
