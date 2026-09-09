use kobold_types::Role;

use crate::budget::{TokenBudget, TokenEstimator};
use crate::history::ConversationHistory;
use crate::prune::{prune_tool_outputs, PruningReport};

/// Full summary report of two-tier compaction execution.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CompactionReport {
    /// Report from Tier 1 tool output pruning.
    pub tier1: PruningReport,

    /// Number of intermediate conversational turns slid out in Tier 2.
    pub tier2_turns_slid: usize,

    /// Estimated token count before compaction began.
    pub initial_tokens: usize,

    /// Estimated token count after compaction concluded.
    pub final_tokens: usize,
}

/// Orchestrate two-tier context compaction across conversation history.
///
/// Tier 1: Prunes historical tool outputs based on each output's `PruningPolicy`.
/// Tier 2: Slides intermediate turns if token usage still exceeds target limits,
/// strictly preserving the System message (Turn 0), initial User task (Turn 1),
/// and the active turn.
pub fn compact_context(
    history: &mut ConversationHistory,
    budget: &TokenBudget,
    estimator: &mut TokenEstimator,
) -> CompactionReport {
    let initial_tokens = estimator.estimate_total(history.messages());
    let mut report = CompactionReport {
        tier1: PruningReport::default(),
        tier2_turns_slid: 0,
        initial_tokens,
        final_tokens: initial_tokens,
    };

    if !budget.needs_compaction(initial_tokens) {
        return report;
    }

    // --- Tier 1: Prune historical tool outputs ---
    report.tier1 = prune_tool_outputs(history);
    estimator.invalidate_reconciliation();
    let tokens_after_tier1 = estimator.estimate_total(history.messages());
    report.final_tokens = tokens_after_tier1;

    // If Tier 1 brought us below the compaction threshold, stop early
    if !budget.needs_compaction(tokens_after_tier1) {
        return report;
    }

    // --- Tier 2: Slide intermediate turns ---
    let target_tokens = budget.target_tokens();
    let mut current_tokens = tokens_after_tier1;

    while current_tokens > target_tokens {
        let turn_ranges = history.turn_ranges();
        // We need at least:
        // Turn 0 (System or first User), Turn 1 (Initial task), Turn 2 (Candidate to slide), Turn 3 (Active turn)
        // If there are <= 2 turns total (or <= 3 with System), no intermediate turns can be slid.
        let has_system = !history.messages().is_empty() && history.messages()[0].role == Role::System;
        let min_turns_to_slide = if has_system { 4 } else { 3 };

        if turn_ranges.len() < min_turns_to_slide {
            break;
        }

        // Candidate to slide is the oldest intermediate turn:
        // If has_system: Turn 0 is System, Turn 1 is initial task. Turn 2 is candidate.
        // If no system: Turn 0 is initial task. Turn 1 is candidate.
        let candidate_turn_idx = if has_system { 2 } else { 1 };
        let candidate_range = turn_ranges[candidate_turn_idx].clone();

        // Drain the messages belonging to this intermediate turn
        history.messages_mut().drain(candidate_range);
        report.tier2_turns_slid += 1;

        estimator.invalidate_reconciliation();
        current_tokens = estimator.estimate_total(history.messages());
    }

    report.final_tokens = current_tokens;
    report
}
