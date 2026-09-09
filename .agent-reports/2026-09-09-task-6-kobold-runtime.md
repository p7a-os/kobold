# TASK-6: kobold-runtime In-Process Agent Session SDK Crate

## 1. Request as understood

Build `crates/kobold-runtime` as an in-process Rust SDK delivering session assembly, workspace binding, and persistent transcript DAG recording. The runtime binds a workspace root, automatically discovers `AGENTS.md` and environment metadata through `SystemPromptBuilder`, manages `ConversationHistory` and token budgets, configures `kobold-tool-fs` and `kobold-tool-bash` (with `HostProcess` or `MicroVm` sandboxes and APFS CoW checkpoints), defaults to `OpenAiBackend` using `gpt-5.6-luna` with low reasoning effort, and records all turns and tool calls to append-only `.kobold/transcript.jsonl`.

## 2. Facts the decision depended on

- `kobold-runtime` is an in-process SDK crate with zero socket, IPC, or daemon dependencies (`D-32`).
- `AGENTS.md` is discovered in the workspace root and strictly loaded into Turn 0, while `CLAUDE.md` is ignored (`D-29`).
- `gpt-5.6-luna` with low reasoning effort (`effort: "low"`) is the standard model and reasoning configuration (`D-40`, `I-7`).
- All tool executions report mutation side-effects (`is_mutating`) to preserve host read safety (`D-39`).
- File modifications and bash execution run in the same sandbox environment (`D-35`).
- The transcript DAG stores immutable turns in `.kobold/transcript.jsonl` with support for branching, forking, and crash recovery (`D-32`, `D-37`).
- APFS copy-on-write directory snapshots enable sub-second checkpoint rollback on Apple Silicon (`D-35`, `D-36`).

## 3. Sources checked

1. `.tasks.md:90-104` - Inspected TASK-6 scope, dependencies, and deliverables.
2. `.knowledge/decisions.md:D-32, D-35, D-36, D-37, D-39, D-40` - Checked architecture decisions for SDK session isolation, sandboxes, and model configurations.
3. `docs/architecture/transcripts.md:1-47` and `kobold-core/src/transcript.rs:1-263` - Verified append-only JSONL format and branch cursor semantics.
4. `crates/kobold-kernel/src/orchestrator.rs:1-357` - Inspected `Kernel` and `KernelBuilder` turn loop API.

## 4. Observations

- `AgentSession` coordinates turn execution by calling `kernel.step(prompt)` and streaming new turns into `.kobold/transcript.jsonl` atomically.
- Replaying a branch from the transcript reconstructs the exact `ConversationHistory` without re-running model inference or tools.
- APFS directory cloning via `libc::clonefile` creates instant checkpoints and rollbacks without duplicating unchanged file blocks.
- When an approval policy denies a tool execution, the refusal is recorded in the transcript DAG and returned to the model for self-correction without panics.

## 5. Decision

Implemented `crates/kobold-runtime`:
- `error.rs`: `RuntimeError` enum consolidating kernel, backend, I/O, config, and transcript errors.
- `transcript.rs`: `TranscriptLog` and `TranscriptRecord` providing append-only JSONL logging, branch cursor tracking, parent-linked DAG branch reading, crash-resilient parsing, and user history extraction.
- `session.rs`: `AgentSession` encapsulating session ID, workspace root, active branch, kernel orchestrator, transcript persistence, sandbox mode, and APFS checkpoint controls.
- `builder.rs`: `SessionBuilder` providing fluent construction, workspace rule discovery (`AGENTS.md`), git branch detection, default or custom tool configuration, OpenAI backend initialization (`gpt-5.6-luna`), and token budgeting.
- `lib.rs`: Public SDK re-exports and comprehensive integration test suite covering defaults, multi-turn continuation, tool execution, transcript branching, checkpoint rollback, approval denials, and live OpenAI streaming.

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

Observed output:
- `cargo check -p kobold-runtime`: exit code 0.
- `cargo clippy -p kobold-runtime -- -D warnings`: exit code 0 (0 warnings).
- `cargo test -p kobold-runtime`: 8 passed, 0 failed.
- Full workspace thin harness test suite: 49 passed, 0 failed, 1 ignored.

## 7. Unverified and risks

- Apple Silicon microVM execution was verified via the host-process fallback and mock command executor; production microVM guest initialization over vsock requires a running hypervisor guest image.

## 8. Out-of-scope findings

- Supervisor daemon (`kobold-server`) spawning child worker processes per workspace belongs to TASK-7.
- CLI client (`kobold-client-cli`) and 3-pane Ratatui TUI client (`kobold-client-tui`) belong to TASK-7.
