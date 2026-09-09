use kobold_types::{Message, Role, TokenUsage};
use serde::{Deserialize, Serialize};

/// Configuration parameters for context window token limits and compaction thresholds.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TokenBudget {
    /// Maximum context window supported by the model (e.g. 128,000 or 200,000).
    pub max_context_tokens: usize,

    /// Tokens reserved exclusively for model completion/generation (e.g. 4,096).
    pub reserved_completion_tokens: usize,

    /// Ratio of usable budget that triggers context compaction (e.g. 0.75 for 75%).
    pub compaction_threshold_ratio: f32,

    /// Target ratio to compact down to once compaction is triggered (e.g. 0.60 for 60%).
    pub target_headroom_ratio: f32,
}

impl Default for TokenBudget {
    fn default() -> Self {
        Self {
            max_context_tokens: 128_000,
            reserved_completion_tokens: 4_096,
            compaction_threshold_ratio: 0.75,
            target_headroom_ratio: 0.60,
        }
    }
}

impl TokenBudget {
    pub fn new(max_context_tokens: usize, reserved_completion_tokens: usize) -> Self {
        Self {
            max_context_tokens,
            reserved_completion_tokens,
            ..Default::default()
        }
    }

    /// The net budget available for input messages after reserving completion space.
    pub fn usable_budget(&self) -> usize {
        self.max_context_tokens
            .saturating_sub(self.reserved_completion_tokens)
    }

    /// The token count threshold that triggers compaction.
    pub fn compaction_threshold_tokens(&self) -> usize {
        ((self.usable_budget() as f32) * self.compaction_threshold_ratio) as usize
    }

    /// The target token count to reach after compaction completes.
    pub fn target_tokens(&self) -> usize {
        ((self.usable_budget() as f32) * self.target_headroom_ratio) as usize
    }

    /// Check if the estimated token count exceeds the compaction threshold.
    pub fn needs_compaction(&self, estimated_tokens: usize) -> bool {
        estimated_tokens >= self.compaction_threshold_tokens()
    }
}

/// Hybrid token estimator tracking exact provider counts and fast character heuristics.
#[derive(Debug, Clone, Default)]
pub struct TokenEstimator {
    /// Number of messages included in the last provider reconciliation.
    reconciled_message_count: usize,

    /// Exact token count reported by provider for the reconciled message prefix.
    reconciled_tokens: usize,

    /// Approximate characters per token (default: 4).
    chars_per_token: usize,
}

impl TokenEstimator {
    pub fn new() -> Self {
        Self {
            reconciled_message_count: 0,
            reconciled_tokens: 0,
            chars_per_token: 4,
        }
    }

    /// Estimate token count for an individual message using character heuristics.
    pub fn estimate_message(&self, message: &Message) -> usize {
        // Base envelope overhead per message (role tag, delimiters)
        let mut tokens = match message.role {
            Role::System => 4,
            Role::User => 4,
            Role::Assistant => 4,
            Role::Tool => 6,
        };

        // Name tag overhead if present
        if let Some(name) = &message.name {
            tokens += (name.len() + 3) / self.chars_per_token;
        }

        // Content parts
        for part in &message.content {
            match part {
                kobold_types::ContentPart::Text { text } => {
                    tokens += (text.len() + 3) / self.chars_per_token;
                }
                kobold_types::ContentPart::Thought { thought } => {
                    tokens += (thought.len() + 3) / self.chars_per_token;
                }
            }
        }

        // Tool calls requested by assistant
        for call in &message.tool_calls {
            tokens += 8; // Tool call envelope
            tokens += (call.name.len() + 3) / self.chars_per_token;
            tokens += (call.arguments.len() + 3) / self.chars_per_token;
        }

        // Tool call ID reference
        if let Some(call_id) = &message.tool_call_id {
            tokens += (call_id.len() + 3) / self.chars_per_token;
        }

        tokens
    }

    /// Estimate total tokens across a slice of conversation messages using hybrid tracking.
    pub fn estimate_total(&self, messages: &[Message]) -> usize {
        if messages.is_empty() {
            return 0;
        }

        // If no reconciliation baseline exists or history shrunk below baseline, calculate directly
        if self.reconciled_message_count == 0 || messages.len() < self.reconciled_message_count {
            return messages.iter().map(|m| self.estimate_message(m)).sum();
        }

        // Start with reconciled exact provider token baseline
        let mut total = self.reconciled_tokens;

        // Apply heuristic estimation only to un-reconciled trailing messages
        for message in &messages[self.reconciled_message_count..] {
            total += self.estimate_message(message);
        }

        total
    }

    /// Reconcile estimator with exact provider usage reported at the conclusion of a turn.
    pub fn reconcile(&mut self, message_count: usize, usage: TokenUsage) {
        self.reconciled_message_count = message_count;
        // Prompt tokens cover the input messages up to this turn
        self.reconciled_tokens = usage.prompt_tokens as usize;
    }

    /// Invalidate reconciliation baseline (e.g. after compaction modifies historical messages).
    pub fn invalidate_reconciliation(&mut self) {
        self.reconciled_message_count = 0;
        self.reconciled_tokens = 0;
    }
}
