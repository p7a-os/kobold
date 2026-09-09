use std::pin::Pin;
use futures_core::Stream;
use serde::{Deserialize, Serialize};

use crate::error::BackendError;
use crate::event::TurnFinishReason;
use crate::message::Message;
use crate::tool::{ToolCall, ToolDefinition};

/// Exact token counts reported by the inference provider.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

impl TokenUsage {
    pub fn new(prompt_tokens: u32, completion_tokens: u32) -> Self {
        Self {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens.saturating_add(completion_tokens),
        }
    }
}

/// An incremental event emitted by a model backend stream during turn inference.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BackendEvent {
    /// Incremental content delta for the assistant reply.
    TextDelta { delta: String },

    /// Incremental internal reasoning delta.
    ThoughtDelta { delta: String },

    /// Incremental streaming chunk for a tool call invocation.
    ToolCallChunk {
        index: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        arguments_delta: String,
    },

    /// A fully assembled tool call produced by the model.
    ToolCallComplete { call: ToolCall },

    /// Final event emitted when model inference concludes for the turn.
    Finished {
        finish_reason: TurnFinishReason,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<TokenUsage>,
    },
}

/// Boxed pinned stream of backend events.
pub type BackendStream = Pin<Box<dyn Stream<Item = Result<BackendEvent, BackendError>> + Send>>;

/// Trait contract for model inference backends (e.g. OpenAI Realtime / WebSocket).
#[async_trait::async_trait]
pub trait Backend: Send + Sync {
    /// Stream inference events given the active message context and available tools.
    async fn stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> Result<BackendStream, BackendError>;
}
