# TASK-3: kobold-kernel Async Agentic Loop Crate

## 1. Request as understood

Build `crates/kobold-kernel` to implement the async agentic turn loop. The crate coordinates model inference, pre-flight context compaction, sequential tool dispatch, pluggable approval gating, self-correction via error feedback, and lifecycle event emission. It must remain decoupled from specific transport, networking, and filesystem implementations by relying strictly on `kobold-types` contracts and `kobold-context`.

## 2. Facts the decision depended on

- `kobold-kernel` depends only on `kobold-types` and `kobold-context` (`D-11`).
- The kernel coordinates single-turn execution with sequential tool execution (`D-15`).
- Pluggable approval policies gate tool invocations (`D-14`).
- Non-zero tool exits and execution failures feed back to the model as error outputs to enable self-correction (`D-16`).
- Pre-flight two-tier compaction runs before inference (`D-12`, `D-27`).
- Token usage reported by providers reconciles with the hybrid token estimator at turn completion (`D-13`, `D-28`).
- Lifecycle events stream to subscribers via `EventSink` (`D-20`).

## 3. Sources checked

1. `.tasks.md:39-53` - Checked TASK-3 scope and acceptance criteria.
2. `.knowledge/decisions.md:D-11..D-16, D-20` - Verified architectural decisions for the agentic loop.
3. `crates/kobold-types` - Inspected `Backend`, `Tool`, `ApprovalPolicy`, `EventSink`, `Message`, and `KernelEvent`.
4. `crates/kobold-context` - Inspected `compact_context`, `TokenBudget`, `TokenEstimator`, and `ConversationHistory`.

## 4. Observations

- Pre-flight compaction must run before advertising messages to the model backend to keep tokens within the configured budget.
- Backend streaming yields incremental text and reasoning deltas that the kernel forwards directly to the `EventSink`.
- When the model emits tool calls, the kernel checks the configured `ApprovalPolicy` for each call in sequence.
- When an approval policy denies execution, the kernel emits an `ApprovalResolved` event with `approved = false` and feeds the denial reason back to the model as a `ToolOutput::error`.
- When an approval policy requests interactive approval, the kernel emits `ApprovalRequested` and pauses execution.
- If tool execution fails, the error message converts into a `ToolOutput::error` so the model can inspect the failure and self-correct in the subsequent step.
- Runaway tool recursion halts deterministically when step counts exceed `max_tool_steps`.

## 5. Decision

Implemented `crates/kobold-kernel` with four core modules:
- `error.rs`: `KernelError` covering backend, event sink, tool execution, cancellation, and max iteration limits.
- `turn.rs`: `TurnResult` capturing turn index, finish reason, tool calls executed, token usage, final response, and thoughts.
- `orchestrator.rs`: `Kernel` and `KernelBuilder` coordinating the async agentic loop, pre-flight compaction, tool dispatch, approval gating, and event streaming.
- `lib.rs`: Public re-exports and comprehensive unit tests covering single-turn completion, sequential tool dispatch, self-correction on tool errors, approval policy denial, and iteration boundary guards.

Fixed a division-by-zero defect in `kobold-context::budget::TokenEstimator` where `TokenEstimator::default()` initialized `chars_per_token` to 0 instead of 4.

## 6. Verification

Run the verification instrument:

1. Open a terminal in the repository root directory.
2. Execute the verification script:
   ```bash
   .agent-reports/2026-09-09-task-3-kobold-kernel/verify.sh
   ```
3. Confirm that all five unit tests pass.
4. Confirm that `cargo clippy` emits no warnings.
5. Confirm that the dependency check detects no network or websocket libraries in the production dependency tree.

Observed output:
- `cargo check -p kobold-kernel`: exit code 0.
- `cargo test -p kobold-kernel`: 5 passed, 0 failed.
- `cargo clippy -p kobold-kernel`: exit code 0.
- Dependency audit: no network or websocket dependencies detected.

## 7. Unverified and risks

- Interactive user approval resumption will connect to live client IPC in TASK-7 (`kobold-server` and `kobold-client-tui`). In this task, the policy contract returns `ApprovalAction::NeedUserApproval` and yields paused execution events verified via unit tests.

## 8. Out-of-scope findings

- Virtualized microVM execution (`kobold-tool-bash`) and structured filesystem access (`kobold-tool-fs`) belong to TASK-4.
- OpenAI Responses API client (`kobold-backend-openai`) belongs to TASK-5.
