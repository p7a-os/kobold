use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors arising during tool execution or argument validation.
#[derive(Debug, Clone, PartialEq, Eq, Error, Serialize, Deserialize)]
#[serde(tag = "type", content = "message", rename_all = "snake_case")]
pub enum ToolError {
    #[error("tool execution failed: {0}")]
    Execution(String),

    #[error("invalid tool arguments: {0}")]
    InvalidArguments(String),

    #[error("tool permission denied: {0}")]
    PermissionDenied(String),

    #[error("tool execution timed out after {timeout_secs}s")]
    Timeout { timeout_secs: u64 },

    #[error("tool not found: {0}")]
    NotFound(String),

    #[error("internal tool error: {0}")]
    Internal(String),
}

/// Errors arising during model inference or backend streaming.
#[derive(Debug, Clone, PartialEq, Eq, Error, Serialize, Deserialize)]
#[serde(tag = "type", content = "message", rename_all = "snake_case")]
pub enum BackendError {
    #[error("connection error: {0}")]
    Connection(String),

    #[error("authentication error: {0}")]
    Authentication(String),

    #[error("rate limited: {0}")]
    RateLimit(String),

    #[error("protocol error: {0}")]
    Protocol(String),

    #[error("context length exceeded: {0}")]
    ContextLengthExceeded(String),

    #[error("internal backend error: {0}")]
    Internal(String),
}

/// Errors arising when emitting events to an event sink.
#[derive(Debug, Clone, PartialEq, Eq, Error, Serialize, Deserialize)]
#[serde(tag = "type", content = "message", rename_all = "snake_case")]
pub enum EventSinkError {
    #[error("event sink receiver disconnected")]
    Closed,

    #[error("event emission error: {0}")]
    Other(String),
}
