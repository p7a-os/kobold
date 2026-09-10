# TASK-6: kobold-runtime In-Process Agent Session SDK Crate & End-to-End Live Provider Testing

## 1. Request as understood

Build `crates/kobold-runtime` as an in-process Rust SDK delivering session assembly, workspace binding, and persistent transcript DAG recording. The runtime binds a workspace root, automatically discovers `AGENTS.md` and environment metadata through `SystemPromptBuilder`, manages `ConversationHistory` and token budgets, configures `kobold-tool-fs` and `kobold-tool-bash` (with `HostProcess` or `MicroVm` sandboxes and APFS CoW checkpoints), defaults to `OpenAiBackend` using `gpt-5.6-luna` with low reasoning effort, and records all turns and tool calls to append-only `.kobold/transcript.jsonl`.

Furthermore, provide end-to-end integration tests (`tests/e2e_live.rs`) testing the full functional flow from the SDK through to the live LLM provider (`wss://api.openai.com/v1/responses`) and back, covering happy paths, error paths, and edge cases.

## 2. Facts the decision depended on

- `kobold-runtime` is an in-process SDK crate with zero socket, IPC, or daemon dependencies (`D-32`).
- `AGENTS.md` is discovered in the workspace root and strictly loaded into Turn 0, while `CLAUDE.md` is ignored (`D-29`).
- `gpt-5.6-luna` with low reasoning effort (`effort: "low"`) is the standard model and reasoning configuration (`D-40`, `I-7`).
- All tool executions report mutation side-effects (`is_mutating`) to preserve host read safety (`D-39`).
- File modifications and bash execution run in the same sandbox environment (`D-35`).
- The transcript DAG stores immutable turns in `.kobold/transcript.jsonl` with support for branching, forking, and crash recovery (`D-32`, `D-37`).
- APFS copy-on-write directory snapshots enable sub-second checkpoint rollback on Apple Silicon (`D-35`, `D-36`).
- OpenAI's Responses WebSocket protocol emits `response.function_call_arguments.done` followed by `response.output_item.done`. Emitting `ToolCallComplete` from both causes duplicate executions with empty `call_id`s; only `response.output_item.done` carries the complete `call_id`, `name`, and `arguments` together.

## 3. Sources checked

1. `.tasks.md:90-104` - Inspected TASK-6 scope, dependencies, and deliverables.
2. `.knowledge/decisions.md:D-32, D-35, D-36, D-37, D-39, D-40` - Checked architecture decisions for SDK session isolation, sandboxes, and model configurations.
3. `docs/architecture/transcripts.md:1-47` and `kobold-core/src/transcript.rs:1-263` - Verified append-only JSONL format and branch cursor semantics.
4. `crates/kobold-kernel/src/orchestrator.rs:1-357` - Inspected `Kernel` and `KernelBuilder` turn loop API.
5. `kobold-openai/src/net.rs:455-465` - Inspected live OpenAI Responses WebSocket framing and function call item handling.

## 4. Observations

- `AgentSession` coordinates turn execution by calling `kernel.step(prompt)` and streaming new turns into `.kobold/transcript.jsonl` atomically.
- In OpenAI's Responses WebSocket protocol, `response.function_call_arguments.done` carries arguments without the tool name, while `response.output_item.done` provides the complete item tuple (`id`, `call_id`, `name`, `arguments`). Eliminating duplicate dispatch in `kobold-backend-openai` resolved empty `call_id` validation errors.
- Guarding against serializing empty `call_id` values in `wire.rs` prevents upstream API rejection when tool calls or outputs are converted.
- Replaying a branch from the transcript reconstructs the exact `ConversationHistory` without re-running model inference or tools.
- APFS directory cloning via `libc::clonefile` creates instant checkpoints and rollbacks without duplicating unchanged file blocks.
- When an approval policy denies a tool execution, the refusal is recorded in the transcript DAG and returned to the model for self-correction without panics.

## 5. Decision

Implemented `crates/kobold-runtime` and live E2E test suite:
- `error.rs`: `RuntimeError` enum consolidating kernel, backend, I/O, config, and transcript errors.
- `transcript.rs`: `TranscriptLog` and `TranscriptRecord` providing append-only JSONL logging, branch cursor tracking, parent-linked DAG branch reading, crash-resilient parsing, and user history extraction.
- `session.rs`: `AgentSession` encapsulating session ID, workspace root, active branch, kernel orchestrator, transcript persistence, sandbox mode, APFS checkpoint controls, and `fork(branch, seq)`.
- `builder.rs`: `SessionBuilder` providing fluent construction, workspace rule discovery (`AGENTS.md`), git branch detection, default or custom tool configuration, OpenAI backend initialization (`gpt-5.6-luna`), and token budgeting.
- `tests/e2e_live.rs`: Comprehensive end-to-end integration test suite exercising the live SDK against `wss://api.openai.com/v1/responses` using `gpt-5.6-luna`:
  1. `test_live_e2e_happy_path_file_tool_loop`: Model creates Python file, reads back to verify, and confirms.
  2. `test_live_e2e_happy_path_multi_turn_bash_and_context`: Multi-turn bash directory creation and state memory verification.
  3. `test_live_e2e_error_path_tool_failure_and_self_correction`: Tool error recovery when reading missing file, recovering secret.
  4. `test_live_e2e_error_path_approval_policy_denial`: `ApprovalPolicy` intercepting and denying execution with model explanation.
  5. `test_live_e2e_edge_case_path_cone_escape_refusal`: Refusal and safety confinement when model attempts path traversal.
  6. `test_live_e2e_edge_case_zdr_and_checkpoint_rollback`: Zero Data Retention mode combined with instant APFS CoW rollback.
  7. `test_live_e2e_edge_case_transcript_dag_forking`: Branch forking preserving independent parallel conversation lanes.

## 6. Verification

Follow these steps to run the verification instrument:

1. Open a terminal in the repository root directory.
2. Execute the verification script:
   ```bash
   .agent-reports/2026-09-09-task-6-kobold-runtime/verify.sh
   ```
3. Confirm that `cargo check -p kobold-runtime` completes with code 0.
4. Confirm that `cargo clippy -p kobold-runtime -- -D warnings` emits no warnings.
5. Confirm that all 8 unit and integration tests in `kobold-runtime` pass.
6. Confirm that all 49 unit and integration tests across all thin harness crates pass.
7. Confirm that all 7 live provider end-to-end tests in `tests/e2e_live.rs` pass.

Observed output:
- `cargo check -p kobold-runtime`: exit code 0.
- `cargo clippy -p kobold-runtime -- -D warnings`: exit code 0 (0 warnings).
- `cargo test -p kobold-runtime`: 8 passed, 0 failed.
- Full workspace thin harness test suite: 49 passed, 0 failed, 1 ignored.
- Live provider E2E suite (`tests/e2e_live.rs`): 7 passed, 0 failed, finished in 37.75s.

## 7. Unverified and risks

- Apple Silicon microVM execution was verified via the host-process fallback and mock command executor; production microVM guest initialization over vsock requires a running hypervisor guest image.

## 8. Out-of-scope findings

- Supervisor daemon (`kobold-server`) spawning child worker processes per workspace belongs to TASK-7.
- CLI client (`kobold-client-cli`) and 3-pane Ratatui TUI client (`kobold-client-tui`) belong to TASK-7.
