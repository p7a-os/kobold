use kobold_types::{TokenUsage, TurnFinishReason};

/// The final outcome of an executed turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnResult {
    /// Zero-based index of the completed turn.
    pub turn_index: usize,

    /// Reason why the turn finished.
    pub finish_reason: TurnFinishReason,

    /// Number of tool invocations executed during this turn.
    pub tool_calls_executed: usize,

    /// Token usage reported by provider for this turn, if available.
    pub usage: Option<TokenUsage>,

    /// Final accumulated text response from the model.
    pub final_response: String,

    /// Accumulated internal thought / reasoning from the model, if any.
    pub thought: Option<String>,
}
