use std::collections::HashSet;

use kobold_types::{ContentPart, PruningPolicy, Role};

use crate::history::ConversationHistory;

/// Pruning report summarizing the modifications performed during Tier 1 compaction.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PruningReport {
    /// Number of tool output messages pruned or truncated.
    pub outputs_pruned: usize,

    /// Number of bytes eliminated from message contents.
    pub bytes_freed: usize,
}

/// Execute Tier 1 compaction: prune historical tool outputs in accordance with their PruningPolicy.
///
/// Returns a report detailing the number of outputs modified and bytes saved.
pub fn prune_tool_outputs(history: &mut ConversationHistory) -> PruningReport {
    let turn_ranges = history.turn_ranges();
    let current_turn = history.current_turn_index();
    let messages = history.messages_mut();

    let mut report = PruningReport::default();
    if messages.is_empty() {
        return report;
    }

    // Step 1: Collect latest occurrence for KeepLast keys across all Tool messages
    let mut seen_keep_last_keys: HashSet<String> = HashSet::new();
    let mut latest_indices_for_key: HashSet<usize> = HashSet::new();

    for (idx, msg) in messages.iter().enumerate().rev() {
        if msg.role == Role::Tool {
            if let Some(PruningPolicy::KeepLast { key }) = &msg.tool_pruning {
                if !seen_keep_last_keys.contains(key) {
                    seen_keep_last_keys.insert(key.clone());
                    latest_indices_for_key.insert(idx);
                }
            }
        }
    }

    // Step 2: Iterate over messages and apply respective pruning policies
    for (idx, msg) in messages.iter_mut().enumerate() {
        if msg.role != Role::Tool {
            continue;
        }

        // Determine which turn this message belongs to
        let msg_turn = turn_ranges
            .iter()
            .position(|range| range.contains(&idx))
            .unwrap_or(0);

        let turn_distance = current_turn.saturating_sub(msg_turn);

        // Never prune active turn outputs (turn_distance == 0) unless superseded by KeepLast
        let policy = msg.tool_pruning.clone().unwrap_or_default();
        let original_text = msg.text();
        let original_bytes = original_text.len();

        let maybe_new_text = match policy {
            PruningPolicy::Never => None,

            PruningPolicy::KeepLast { key } => {
                // If this is not the latest occurrence for this key, prune it
                if !latest_indices_for_key.contains(&idx) {
                    Some(format!(
                        "[output pruned: superseded by later call for \"{}\"]",
                        key
                    ))
                } else {
                    None
                }
            }

            PruningPolicy::HeadTail {
                head_lines,
                tail_lines,
            } => {
                if turn_distance > 0 {
                    prune_head_tail(&original_text, head_lines, tail_lines)
                } else {
                    None
                }
            }

            PruningPolicy::CollapseAfterTurns { turns } => {
                if turn_distance > turns {
                    Some(format!(
                        "[output pruned: {} bytes, completed in turn {}]",
                        original_bytes, msg_turn
                    ))
                } else {
                    None
                }
            }

            PruningPolicy::Summary { summary } => {
                if turn_distance > 0 {
                    Some(format!("[output summary: {}]", summary))
                } else {
                    None
                }
            }
        };

        if let Some(new_text) = maybe_new_text {
            let saved = original_bytes.saturating_sub(new_text.len());
            msg.content = vec![ContentPart::text(new_text)];
            report.outputs_pruned += 1;
            report.bytes_freed += saved;
        }
    }

    report
}

/// Truncate text to head and tail lines if total lines exceed head + tail.
fn prune_head_tail(text: &str, head_lines: usize, tail_lines: usize) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= head_lines + tail_lines {
        return None;
    }

    let head = lines[..head_lines].join("\n");
    let tail = lines[lines.len() - tail_lines..].join("\n");
    let omitted = lines.len() - head_lines - tail_lines;

    Some(format!(
        "{}\n[... pruned {} lines ...]\n{}",
        head, omitted, tail
    ))
}
