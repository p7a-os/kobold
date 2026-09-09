# TASK-1: Workspace Skeleton & kobold-types Crate

## 1. Request as understood

Configure the Cargo workspace for crates under `crates/*` and create the foundational `kobold-types` crate. The crate must declare pure data structures, interfaces, and trait contracts without operational logic, process spawning, network access, or filesystem I/O.

## 2. Facts the decision depended on

- The repository root `Cargo.toml` controls workspace membership via `workspace.members`.
- `kobold-types` must decouple the agentic execution kernel from concrete backend and tool implementations (`D-10`, `D-11`).
- Dynamic tool dispatch and backend composition require dyn-compatible trait definitions (`Tool`, `Backend`, `EventSink`, `ApprovalPolicy`).
- `Cargo.lock` already resolves `async-trait = "0.1.92"`, `futures-core = "0.3.34"`, `serde = "1.0.229"`, and `thiserror = "2.0.20"`.

## 3. Sources checked

1. Root `Cargo.toml:1-6` - Checked workspace declaration and default members.
2. `Cargo.lock` - Verified available dependency versions.
3. `.tasks.md:13-23` - Checked TASK-1 scope and acceptance criteria.
4. `.knowledge/decisions.md:D-10..D-26` - Checked architectural decisions on crate boundaries and interfaces.

## 4. Observations

- Adding `"crates/*"` to `workspace.members` permits Cargo to automatically discover crates in the subfolder.
- The `kobold-types` crate compiled cleanly in 6.08s with zero warnings under `cargo clippy`.
- Trait object compatibility assertions confirmed that `Box<dyn Backend>`, `Box<dyn Tool>`, `Box<dyn EventSink>`, and `Box<dyn ApprovalPolicy>` compile without dyn-safety violations.
- Dependency inspection via `cargo tree -p kobold-types` showed no network, runtime, or process dependencies.

## 5. Decision

Implemented `crates/kobold-types` with:
- Data models: `Message`, `Role`, `ContentPart`, `ToolDefinition`, `ToolCall`, `ToolOutput`, `KernelEvent`, `BackendEvent`, `TokenUsage`, and `TurnFinishReason`.
- Trait contracts: `Backend` (streaming `BackendEvent`), `Tool` (metadata and invocation), `EventSink` (turn lifecycle observation), and `ApprovalPolicy` (permission check).
- Error types: `ToolError`, `BackendError`, and `EventSinkError`.
- Reference policies and sinks: `AllowAllPolicy`, `DenyAllPolicy`, and `NoopEventSink`.

## 6. Verification

Run the verification instrument:

1. Open a shell in the repository root.
2. Run the script:
   ```bash
   .agent-reports/2026-09-09-task-1-kobold-types/verify.sh
   ```
3. Confirm that four unit tests pass, `cargo clippy` emits no warnings, and no I/O dependencies appear in the crate tree.

Observed output:
- `cargo check -p kobold-types`: exit code 0.
- `cargo test -p kobold-types`: 4 passed, 0 failed.
- `cargo clippy -p kobold-types`: exit code 0.
- Dependency audit: no `tokio`, `hyper`, or `reqwest` entries found.

## 7. Unverified and risks

- Legacy workspace crate `kobold-tts` depends on `sentencepiece-sys`, which references a stale build cache path from the pre-fork repository. Running full workspace commands without package filters (`cargo check --workspace`) touches this legacy build artifact until legacy crates are migrated or removed.

## 8. Out-of-scope findings

- Legacy crates in the repository root (`kobold-core`, `kobold-proto`, `kobold-openai`, `kobold-adapter-acp`, `kobold-adapter-tmux`, `kobold-tts`) remain in place and will be replaced progressively as new crates under `crates/` reach feature parity.
