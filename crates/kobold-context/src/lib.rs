//! Conversation history, token budgeting, prompt construction, and two-tier compaction for Kobold thin harness.

pub mod budget;
pub mod compact;
pub mod history;
pub mod prompt;
pub mod prune;

pub use budget::{TokenBudget, TokenEstimator};
pub use compact::{compact_context, CompactionReport};
pub use history::ConversationHistory;
pub use prompt::{SystemPromptBuilder, DEFAULT_BASE_PROMPT};
pub use prune::{prune_tool_outputs, PruningReport};

#[cfg(test)]
mod tests {
    use super::*;
    use kobold_types::{PruningPolicy, Role, ToolCall, ToolOutput};
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_history_turn_boundaries() {
        let mut history = ConversationHistory::new();
        history.append_system("System prompt");
        assert_eq!(history.turn_count(), 1);

        // Turn 1: User asks, Assistant calls tool, Tool responds, Assistant answers
        history.append_user("First user task");
        history.append_assistant_tool_calls(vec![ToolCall::new("c1", "read_file", "{}")]);
        history.append_tool_output(&ToolOutput::success("c1", "file content"));
        history.append_assistant("Task complete");
        assert_eq!(history.turn_count(), 2);

        // Turn 2: Second user request
        history.append_user("Second user task");
        history.append_assistant("Second reply");
        assert_eq!(history.turn_count(), 3);
        assert_eq!(history.current_turn_index(), 2);
    }

    #[test]
    fn test_hybrid_token_estimator_reconciliation() {
        let mut history = ConversationHistory::new();
        history.append_system("System instructions");
        history.append_user("Short task");

        let mut estimator = TokenEstimator::new();
        let initial_estimate = estimator.estimate_total(history.messages());
        assert!(initial_estimate > 0);

        // Reconcile with exact provider usage (e.g. provider reports 25 prompt tokens)
        estimator.reconcile(
            history.len(),
            kobold_types::TokenUsage {
                prompt_tokens: 25,
                completion_tokens: 10,
                total_tokens: 35,
            },
        );

        // Exact reconciled tokens should be 25
        assert_eq!(estimator.estimate_total(history.messages()), 25);

        // Append a new turn
        history.append_assistant("Here is the answer to your request.");
        let new_estimate = estimator.estimate_total(history.messages());
        // Should be 25 + heuristic estimate of the new assistant message
        assert!(new_estimate > 25);
    }

    #[test]
    fn test_prune_keep_last_strategy() {
        let mut history = ConversationHistory::new();
        history.append_system("System prompt");

        // Turn 1: read main.rs
        history.append_user("Check main.rs");
        history.append_tool_output(
            &ToolOutput::success("c1", "fn main() { v1 }").with_keep_last("src/main.rs"),
        );

        // Turn 2: read main.rs again
        history.append_user("Check main.rs again");
        history.append_tool_output(
            &ToolOutput::success("c2", "fn main() { v2 }").with_keep_last("src/main.rs"),
        );

        let report = prune_tool_outputs(&mut history);
        assert_eq!(report.outputs_pruned, 1);

        let messages = history.messages();
        // First read (Turn 1) must be pruned as superseded
        assert!(messages[2].text().contains("superseded by later call"));
        // Second read (Turn 2) must remain intact
        assert_eq!(messages[4].text(), "fn main() { v2 }");
    }

    #[test]
    fn test_prune_head_tail_strategy() {
        let mut history = ConversationHistory::new();
        history.append_system("System prompt");

        // Turn 1: long compiler output
        history.append_user("Build project");
        let long_output = (1..=20)
            .map(|i| format!("Line {}", i))
            .collect::<Vec<_>>()
            .join("\n");
        history
            .append_tool_output(&ToolOutput::success("c1", long_output).with_head_tail(3, 3));

        // Turn 2: subsequent turn
        history.append_user("Next task");

        let report = prune_tool_outputs(&mut history);
        assert_eq!(report.outputs_pruned, 1);

        let pruned_text = history.messages()[2].text();
        assert!(pruned_text.starts_with("Line 1\nLine 2\nLine 3"));
        assert!(pruned_text.contains("[... pruned 14 lines ...]"));
        assert!(pruned_text.ends_with("Line 18\nLine 19\nLine 20"));
    }

    #[test]
    fn test_prune_collapse_after_turns() {
        let mut history = ConversationHistory::new();
        history.append_system("System prompt");

        // Turn 1: tool output with 1-turn retention
        history.append_user("Run check");
        history.append_tool_output(
            &ToolOutput::success("c1", "Detailed test output from turn 1")
                .with_pruning(PruningPolicy::CollapseAfterTurns { turns: 1 }),
        );

        // Turn 2: only 1 turn has elapsed (distance = 1, threshold = 1) -> not pruned
        history.append_user("Turn 2 user");
        prune_tool_outputs(&mut history);
        assert_eq!(
            history.messages()[2].text(),
            "Detailed test output from turn 1"
        );

        // Turn 3: 2 turns have elapsed (distance = 2 > 1) -> pruned
        history.append_user("Turn 3 user");
        let report = prune_tool_outputs(&mut history);
        assert_eq!(report.outputs_pruned, 1);
        assert!(history.messages()[2].text().contains("[output pruned:"));
    }

    #[test]
    fn test_prune_summary_strategy() {
        let mut history = ConversationHistory::new();
        history.append_system("System");

        // Turn 1: tool with summary
        history.append_user("List files");
        history.append_tool_output(
            &ToolOutput::success("c1", "a.txt\nb.txt\nc.txt\nd.txt")
                .with_summary("Found 4 files"),
        );

        // Turn 2: next turn
        history.append_user("Next");
        let report = prune_tool_outputs(&mut history);
        assert_eq!(report.outputs_pruned, 1);
        assert_eq!(history.messages()[2].text(), "[output summary: Found 4 files]");
    }

    #[test]
    fn test_two_tier_compaction_sliding_window() {
        let mut history = ConversationHistory::new();
        // Turn 0: System prompt (must be pinned)
        history.append_system("System prompt: you are an expert engineer.");

        // Turn 1: First user prompt (must be pinned)
        history.append_user("Initial user goal: build a web server.");
        history.append_assistant("Acknowledged, starting server design.");

        // Turn 2: Intermediate turn (eligible to slide)
        history.append_user("User follow-up question 1");
        history.append_assistant("Assistant reply 1 with long verbose response content.");

        // Turn 3: Intermediate turn (eligible to slide)
        history.append_user("User follow-up question 2");
        history.append_assistant("Assistant reply 2 with long verbose response content.");

        // Turn 4: Active turn (must not be slid)
        history.append_user("Current active user prompt.");

        let mut estimator = TokenEstimator::new();
        let budget = TokenBudget {
            max_context_tokens: 100,
            reserved_completion_tokens: 20,
            compaction_threshold_ratio: 0.50, // 40 tokens threshold
            target_headroom_ratio: 0.30,      // 24 tokens target
        };

        let report = compact_context(&mut history, &budget, &mut estimator);
        assert!(report.tier2_turns_slid > 0);

        let messages = history.messages();
        // Check Turn 0 and Turn 1 pinning
        assert_eq!(messages[0].role, Role::System);
        assert!(messages[0].text().contains("System prompt"));
        assert_eq!(messages[1].role, Role::User);
        assert!(messages[1].text().contains("Initial user goal"));

        // Check active turn preserved at the end
        assert!(messages.last().unwrap().text().contains("Current active user prompt."));
    }

    #[test]
    fn test_agents_md_discovered_and_claude_md_ignored() {
        let dir = tempdir().expect("failed to create tempdir");
        let agents_path = dir.path().join("AGENTS.md");
        let claude_path = dir.path().join("CLAUDE.md");

        fs::write(&agents_path, "# Rules for Agents\n- Be verified.").unwrap();
        fs::write(&claude_path, "# Legacy Claude Rules\n- Ignore this.").unwrap();

        let builder = SystemPromptBuilder::new()
            .discover_workspace_rules(dir.path())
            .with_cwd("/workspace")
            .with_git_branch("feat/thin-harness")
            .with_zdr(true);

        let rendered = builder.render();
        assert!(rendered.contains("# Rules for Agents\n- Be verified."));
        assert!(!rendered.contains("Legacy Claude Rules"));
        assert!(!rendered.contains("Ignore this."));
        assert!(rendered.contains("Working Directory: /workspace"));
        assert!(rendered.contains("Git Branch: feat/thin-harness"));
        assert!(rendered.contains("[ZDR: ON]"));
    }

    #[test]
    fn test_system_prompt_zdr_off() {
        let builder = SystemPromptBuilder::new().with_zdr(false);
        let rendered = builder.render();
        assert!(rendered.contains("[ZDR: OFF]"));
    }
}
