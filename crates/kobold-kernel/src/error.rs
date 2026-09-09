use kobold_types::{BackendError, EventSinkError, ToolError};
use thiserror::Error;

/// Errors arising during kernel turn orchestration.
#[derive(Debug, Error)]
pub enum KernelError {
    #[error("backend inference error: {0}")]
    Backend(#[from] BackendError),

    #[error("event sink emission error: {0}")]
    EventSink(#[from] EventSinkError),

    #[error("tool execution error: {0}")]
    Tool(#[from] ToolError),

    #[error("turn exceeded maximum tool iterations limit ({0})")]
    MaxIterationsExceeded(usize),

    #[error("turn execution was cancelled")]
    Cancelled,

    #[error("kernel error: {0}")]
    Other(String),
}
