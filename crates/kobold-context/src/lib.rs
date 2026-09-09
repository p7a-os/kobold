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

    #[test]
    fn test_history_empty_and_no_system() {
        let empty = ConversationHistory::new();
        assert_eq!(empty.turn_count(), 0);
        assert_eq!(empty.current_turn_index(), 0);
        assert!(empty.turn_ranges().is_empty());

        let mut no_system = ConversationHistory::new();
        no_system.append_user("Prompt 1");
        no_system.append_assistant("Reply 1");
        no_system.append_user("Prompt 2");
        assert_eq!(no_system.turn_count(), 2);
        let ranges = no_system.turn_ranges();
        assert_eq!(ranges[0], 0..2);
        assert_eq!(ranges[1], 2..3);
    }

    #[test]
    fn test_prune_head_tail_short_text_unchanged() {
        let mut history = ConversationHistory::new();
        history.append_user("Run small command");
        history.append_tool_output(
            &ToolOutput::success("c1", "Line 1\nLine 2").with_head_tail(3, 3),
        );
        history.append_user("Next turn");

        let report = prune_tool_outputs(&mut history);
        assert_eq!(report.outputs_pruned, 0);
        assert_eq!(history.messages()[1].text(), "Line 1\nLine 2");
    }

    #[test]
    fn test_prune_keep_last_interleaved_keys() {
        let mut history = ConversationHistory::new();
        history.append_system("System");

        // Turn 1: read a.rs
        history.append_user("Read a");
        history.append_tool_output(&ToolOutput::success("c1", "a v1").with_keep_last("a.rs"));

        // Turn 2: read b.rs
        history.append_user("Read b");
        history.append_tool_output(&ToolOutput::success("c2", "b v1").with_keep_last("b.rs"));

        // Turn 3: read a.rs again
        history.append_user("Read a again");
        history.append_tool_output(&ToolOutput::success("c3", "a v2").with_keep_last("a.rs"));

        // Turn 4: read b.rs again
        history.append_user("Read b again");
        history.append_tool_output(&ToolOutput::success("c4", "b v2").with_keep_last("b.rs"));

        let report = prune_tool_outputs(&mut history);
        assert_eq!(report.outputs_pruned, 2);

        let messages = history.messages();
        // Turn 1 a.rs (idx 2) is pruned
        assert!(messages[2].text().contains("superseded by later call for \"a.rs\""));
        // Turn 2 b.rs (idx 4) is pruned
        assert!(messages[4].text().contains("superseded by later call for \"b.rs\""));
        // Turn 3 a.rs (idx 6) and Turn 4 b.rs (idx 8) remain intact
        assert_eq!(messages[6].text(), "a v2");
        assert_eq!(messages[8].text(), "b v2");
    }

    #[test]
    fn test_two_tier_compaction_no_system_prompt_pinning() {
        let mut history = ConversationHistory::new();
        // Turn 0 is User task (no System prompt)
        history.append_user("Initial user prompt with no system message");
        history.append_assistant("Assistant initial response");

        // Turn 1: Intermediate
        history.append_user("Intermediate question");
        history.append_assistant("Intermediate response");

        // Turn 2: Active turn
        history.append_user("Active user request");

        let mut estimator = TokenEstimator::new();
        let budget = TokenBudget {
            max_context_tokens: 60,
            reserved_completion_tokens: 10,
            compaction_threshold_ratio: 0.50,
            target_headroom_ratio: 0.30,
        };

        let report = compact_context(&mut history, &budget, &mut estimator);
        assert_eq!(report.tier2_turns_slid, 1);

        let messages = history.messages();
        // Turn 0 User must still be pinned
        assert_eq!(messages[0].role, Role::User);
        assert!(messages[0].text().contains("Initial user prompt with no system message"));
        // Active turn must be preserved
        assert!(messages.last().unwrap().text().contains("Active user request"));
    }

    #[test]
    fn test_two_tier_compaction_extreme_budget_never_slides_pinned() {
        let mut history = ConversationHistory::new();
        history.append_system("System prompt");
        history.append_user("Initial goal");
        history.append_assistant("Initial reply");
        history.append_user("Active turn");

        let mut estimator = TokenEstimator::new();
        // Budget impossibly small (5 tokens)
        let budget = TokenBudget {
            max_context_tokens: 10,
            reserved_completion_tokens: 5,
            compaction_threshold_ratio: 0.10,
            target_headroom_ratio: 0.10,
        };

        // There are only pinned turns and active turn (no intermediate turns to slide)
        let report = compact_context(&mut history, &budget, &mut estimator);
        assert_eq!(report.tier2_turns_slid, 0);

        // System, Initial Goal, and Active turn remain intact
        assert_eq!(history.messages().len(), 4);
        assert_eq!(history.messages()[0].role, Role::System);
        assert_eq!(history.messages()[1].role, Role::User);
    }

    #[test]
    fn test_agents_md_missing_and_directory_handling() {
        let dir = tempdir().expect("failed to create tempdir");
        // Case 1: No AGENTS.md at all
        let builder = SystemPromptBuilder::new().discover_workspace_rules(dir.path());
        let rendered = builder.render();
        assert!(!rendered.contains("# Project Instructions"));

        // Case 2: AGENTS.md exists but is a directory, not a file
        let agents_dir = dir.path().join("AGENTS.md");
        fs::create_dir(&agents_dir).unwrap();
        let builder2 = SystemPromptBuilder::new().discover_workspace_rules(dir.path());
        let rendered2 = builder2.render();
        assert!(!rendered2.contains("# Project Instructions"));
    }

    #[test]
    fn test_token_estimator_messages_shrink_after_compaction() {
        let mut history = ConversationHistory::new();
        history.append_user("Prompt 1");
        history.append_assistant("Reply 1");

        let mut estimator = TokenEstimator::new();
        estimator.reconcile(
            history.len(),
            kobold_types::TokenUsage {
                prompt_tokens: 100,
                completion_tokens: 50,
                total_tokens: 150,
            },
        );

        // Remove a message (simulating sliding window drain)
        history.messages_mut().pop();
        // Message count is now 1, which is < reconciled_message_count (2)
        let estimate = estimator.estimate_total(history.messages());
        // Must recalculate using heuristics rather than returning stale 100
        assert!(estimate < 100);
    }
}
