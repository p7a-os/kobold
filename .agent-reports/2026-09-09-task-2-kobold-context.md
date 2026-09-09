# TASK-2: kobold-context Conversation & Compaction Crate

## 1. Request as understood

Build `crates/kobold-context` to isolate conversation history, hybrid token budgeting, two-tier compaction, and system prompt generation from the kernel. The crate must prune tool outputs according to tool-attached `PruningPolicy` rules, slide intermediate turns while pinning the system prompt and initial task, and inject workspace `AGENTS.md` while strictly ignoring `CLAUDE.md`.

## 2. Facts the decision depended on

- `kobold-context` depends on `kobold-types` and standard library primitives (`D-12`).
- Compaction must operate in two tiers: provenance-preserving tool pruning followed by intermediate turn sliding (`D-27`).
- Pruning behavior belongs to individual tool outputs via `PruningPolicy` (`D-30`).
- Token estimation combines exact provider `TokenUsage` counts with fast ~4 chars/token heuristics (`D-28`).
- The system prompt builder discovers `AGENTS.md` and ignores `CLAUDE.md` (`D-29`), adding structured environment context and ZDR status (`D-31`).

## 3. Sources checked

1. `.tasks.md:24-38` - Verified TASK-2 scope and acceptance criteria.
2. `.knowledge/decisions.md:D-12, D-27..D-31` - Checked architectural invariants for context management.
3. `crates/kobold-types` - Checked `Message`, `Role`, `ToolOutput`, and `PruningPolicy` definitions.

## 4. Observations

- `ConversationHistory` segments turns by tracking sequences from `Role::User` through subsequent `Role::Assistant` and `Role::Tool` messages.
- `TokenEstimator` reconciles against provider usage by freezing the token count of the reconciled prefix and estimating only newly appended turns.
- `prune_tool_outputs` evaluates `KeepLast`, `HeadTail`, `CollapseAfterTurns`, and `Summary` without mutating active turn outputs unless superseded.
- `compact_context` pins Turn 0 (system instructions) and Turn 1 (initial user goal), draining intermediate turns only when token usage exceeds the target threshold.
- `SystemPromptBuilder` cleanly reads `AGENTS.md` when present and skips `CLAUDE.md`.

## 5. Decision

Implemented `crates/kobold-context` with five core modules:
- `history.rs`: `ConversationHistory` managing deterministic turn boundaries and message sequences.
- `budget.rs`: `TokenBudget` defining limits and compaction thresholds, paired with hybrid `TokenEstimator`.
- `prune.rs`: `prune_tool_outputs` executing Tier 1 compaction across `PruningPolicy` variants.
- `compact.rs`: `compact_context` driving two-tier compaction with strict Turn 0 / Turn 1 pinning.
- `prompt.rs`: `SystemPromptBuilder` rendering structured base prompt, workspace rules, and environment metadata.

## 6. Verification

Run the verification instrument:

1. Open a terminal in the workspace root.
2. Execute the test script:
   ```bash
   .agent-reports/2026-09-09-task-2-kobold-context/verify.sh
   ```
3. Confirm that all nine unit tests pass and `cargo clippy` emits zero warnings.
4. Confirm that the dependency check finds no `tokio`, `hyper`, or `reqwest` production dependencies.

Observed output:
- `cargo check -p kobold-context`: exit code 0.
- `cargo test -p kobold-context`: 16 passed, 0 failed.
- `cargo clippy -p kobold-context`: exit code 0.
- Dependency audit: no network or process runtime dependencies detected.

## 7. Unverified and risks

- Token heuristic assumes an average of four characters per token. Non-Latin scripts or dense JSON payloads may experience slight drift before provider reconciliation reconciles the exact count at turn completion.

## 8. Out-of-scope findings

- Shell and microVM process execution (`kobold-tool-bash`) and filesystem operations (`kobold-tool-fs`) remain to be implemented in subsequent tasks (TASK-4).
