# Decisions

## Current

### D-1
- **ID**: D-1
- **Statement**: Decouple frontends and execution backends using a dual-seam architecture (Northbound Seam for frontends, Southbound Seam for adapters).
- **Alternatives rejected**: Monolithic CLI embedding both model execution and rendering in one process.
- **Reason**: Allows frontends (Terminal User Interface, Web Companion) and execution adapters (ACP Adapter, Tmux Adapter, OpenAI Adapter) to evolve, test, and run independently.
- **Status**: active
- **Valid**: permanent
- **Source**: `docs/architecture/seams.md:1-5`
- **Date**: 2026-09-08

### D-2
- **ID**: D-2
- **Statement**: Split AG-UI Protocol framing into serialize-only `OutgoingFrame` and deserialize-only `IncomingFrame` with an `Unmodelled` catch-all variant on the Southbound Seam.
- **Alternatives rejected**: Single bidirectional enum deriving both `Serialize` and `Deserialize`.
- **Reason**: A catch-all variant on a serialized enum can emit non-conforming JSON on the wire, breaking external agent parsers.
- **Status**: active
- **Valid**: permanent
- **Source**: `docs/agui.md:342-362`, `kobold-proto/src/lib.rs:18-30`
- **Date**: 2026-09-08

### D-3
- **ID**: D-3
- **Statement**: Discard server-side history snapshots (`MESSAGES_SNAPSHOT`, `STATE_SNAPSHOT`) in favor of client-owned transcript replay via the Transcript DAG.
- **Alternatives rejected**: Relying on provider-managed session summaries or server-side snapshot events.
- **Reason**: Guarantees client ownership over conversation branches and deterministic DAG consistency across reconnects.
- **Status**: active
- **Valid**: permanent
- **Source**: `docs/agui.md:84-88`
- **Date**: 2026-09-08

### D-4
- **ID**: D-4
- **Statement**: Retain the standard system allocator by default and keep `mimalloc` as an opt-in Cargo feature (`fast-alloc`).
- **Alternatives rejected**: Enabling `mimalloc` by default across all release builds.
- **Reason**: Benchmark measurements showed `mimalloc` saved ~15 µs per frame but increased resident memory from 5.1 MB to 12.8 MB due to upfront arena commitment.
- **Status**: active
- **Valid**: permanent
- **Source**: `Cargo.toml:75-83`
- **Date**: 2026-09-08

### D-5
- **ID**: D-5
- **Statement**: Enforce credential and network isolation for Model Context Protocol servers and adapters using OS sandboxing (`bwrap` / `sandbox-exec`) and an Egress Broker.
- **Alternatives rejected**: Unrestricted subprocess execution inheriting ambient environment variables.
- **Reason**: Prevents unauthorized network dials, environment variable snooping, and cloud credential leakage from third-party tools.
- **Status**: active
- **Valid**: permanent
- **Source**: `docs/configuration/mcp.md:8-34`, `kobold-core/src/childenv.rs`
- **Date**: 2026-09-08

### D-6
- **ID**: D-6
- **Statement**: Freeze the Prompt Queue during parked tool questions and interrupts until the full multi-turn interaction completes.
- **Alternatives rejected**: Clearing the queue automatically; prompting the user interactively on every interrupt.
- **Reason**: Preserves user input and prevents premature execution of queued tasks before tool approvals are resolved.
- **Status**: active
- **Valid**: permanent
- **Source**: Human decision via interview tool
- **Date**: 2026-09-08

### D-7
- **ID**: D-7
- **Statement**: Implement AG-UI encrypted reasoning pass-through (`REASONING_ENCRYPTED_VALUE`) to preserve multi-turn reasoning continuity under strict zero cloud retention (`store: false`).
- **Alternatives rejected**: Full transcript replay without reasoning continuity; enabling cloud-side retention (`store: true`).
- **Reason**: Preserves reasoning context across turns while maintaining zero cloud data retention.
- **Status**: active
- **Valid**: permanent
- **Source**: Human decision via interview tool
- **Date**: 2026-09-08

### D-8
- **ID**: D-8
- **Statement**: Prioritize the ACP Adapter as the primary interface for autonomous coding agents, restricting direct WebSocket adapters to OpenAI and OpenRouter.
- **Alternatives rejected**: Building native Anthropic and Google streaming adapters; deprecating direct WebSocket adapters entirely.
- **Reason**: Leverages external agent ecosystems (Claude Code, Antigravity, Grok, Codex) via standardized ACP while avoiding maintenance of proprietary vendor streaming protocols.
- **Status**: active
- **Valid**: permanent
- **Source**: Human decision via interview tool
- **Date**: 2026-09-08

### D-9
- **ID**: D-9
- **Statement**: Retain conversation forks in the current workspace directory by default, creating an isolated git worktree only when explicitly requested.
- **Alternatives rejected**: Automatic git worktree creation on every lane fork.
- **Reason**: Prevents workspace fragmentation for simple conversational branching while reserving worktree isolation for explicit concurrency.
- **Status**: active
- **Valid**: permanent
- **Source**: Human decision via interview tool
- **Date**: 2026-09-08

### D-10
- **ID**: D-10
- **Statement**: Design `kobold-kernel` as an async orchestrator driven over abstract `Backend` and `Tool` traits from `kobold-types`, decoupling the agentic loop from concrete network, process, or filesystem I/O.
- **Alternatives rejected**: Pure synchronous state machine with outer runner; direct dependency on concrete backend and tool crates.
- **Reason**: Enables flexible composition and unit testing with mock backends and tools while avoiding the boilerplate of a purely synchronous effect-passing state machine.
- **Status**: active
- **Valid**: permanent
- **Source**: Human decision via interview tool
- **Date**: 2026-09-08

### D-11
- **ID**: D-11
- **Statement**: Organize the Rust monorepo with workspace member packages located under `crates/` (e.g. `crates/kobold-types`, `crates/kobold-kernel`).
- **Alternatives rejected**: Placing all crates directly in the repository root directory.
- **Reason**: Keeps the repository root clean, separating workspace crates from repository configuration, documentation, and metadata directories.
- **Status**: active
- **Valid**: permanent
- **Source**: Human decision via interview tool
- **Date**: 2026-09-08

### D-12
- **ID**: D-12
- **Statement**: Isolate conversation history, token budgeting, prompt construction, and context compaction into a dedicated `crates/kobold-context` crate.
- **Alternatives rejected**: Bundling conversation management inside `kobold-kernel`; delegating state exclusively to `kobold-server`.
- **Reason**: Keeps `kobold-kernel` focused purely on agentic turn loop coordination while allowing context management and compaction algorithms to be tested and evolved independently.
- **Status**: active
- **Valid**: permanent
- **Source**: Human decision via interview tool
- **Date**: 2026-09-08

### D-13
- **ID**: D-13
- **Statement**: Model event observation via an abstract `EventSink` trait in `kobold-types` (`async fn emit(&self, event: KernelEvent)`), allowing `kobold-kernel` to stream turn events without direct coupling to Tokio or concrete channel types.
- **Alternatives rejected**: Direct Tokio `mpsc::Sender` binding in `kobold-kernel`; polling-based event logs.
- **Reason**: Achieves zero external runtime dependencies for `kobold-types` and `kobold-kernel` while allowing consumers (e.g. `kobold-server`) to back the sink with Tokio bounded channels for sub-microsecond latency and backpressure.
- **Status**: active
- **Valid**: permanent
- **Source**: Human decision via interview tool
- **Date**: 2026-09-08

### D-14
- **ID**: D-14
- **Statement**: Mediate tool execution permissions via a pluggable `ApprovalPolicy` trait in `kobold-types` (`Approve`, `Deny`, or `NeedUserApproval`), pausing the turn and emitting an approval event when confirmation is required.
- **Alternatives rejected**: Global binary boolean flag; executing all tools unconditionally.
- **Reason**: Provides fine-grained control over sensitive actions (e.g. destructive shell commands) while allowing automated execution for safe tools without modifying the kernel.
- **Status**: active
- **Valid**: permanent
- **Source**: Human decision via interview tool
- **Date**: 2026-09-08

### D-15
- **ID**: D-15
- **Statement**: Execute multi-tool calls sequentially within a turn rather than concurrently.
- **Alternatives rejected**: Concurrent tool execution partitioned by read-only and mutating flags.
- **Reason**: Guarantees deterministic transcript ordering, eliminates filesystem race conditions, and simplifies state management in a thin harness.
- **Status**: active
- **Valid**: permanent
- **Source**: Human decision via interview tool
- **Date**: 2026-09-08

### D-16
- **ID**: D-16
- **Statement**: Return tool execution failures (non-zero exits, I/O errors) back to the model as error-flagged tool results instead of aborting the turn.
- **Alternatives rejected**: Aborting the turn immediately on any tool error.
- **Reason**: Enables the model to inspect command output, stack traces, and error messages to self-correct during the active turn.
- **Status**: active
- **Valid**: permanent
- **Source**: Human decision via interview tool
- **Date**: 2026-09-08

### D-17
- **ID**: D-17
- **Statement**: Expose a unified `bash` tool in `kobold-tool-bash` supporting both isolated one-shot execution (default) and persistent stateful execution when an optional `session_id` argument is provided.
- **Alternatives rejected**: Two separate tool definitions (`bash_run` and `bash_session`); one-shot execution only.
- **Reason**: Keeps the tool schema compact for the model while enabling persistent shell state across turns when requested.
- **Status**: active
- **Valid**: permanent
- **Source**: Human decision
- **Date**: 2026-09-08

### D-18
- **ID**: D-18
- **Statement**: Implement `kobold-tool-bash` with a macOS Virtualization.framework microVM and APFS CoW checkpoints (`clonefile`) as the primary execution engine from the start, using quiesced snapshots (`FIFREEZE` / `F_FULLFSYNC` / `clonefile` / `FITHAW`).
- **Alternatives rejected**: Host-process subshell execution as baseline.
- **Reason**: Delivers true transactional execution, complete host isolation, and whole-environment rollback (including installed packages and build caches) on Apple Silicon.
- **Status**: active
- **Valid**: permanent
- **Source**: Human decision via interview tool
- **Date**: 2026-09-09

### D-19
- **ID**: D-19
- **Statement**: Implement `kobold-backend-openai` using the OpenAI Realtime / Responses WebSocket protocol for bidirectional streaming.
- **Alternatives rejected**: Standard HTTP Chat Completions with Server-Sent Events (SSE).
- **Reason**: Minimizes turn latency and supports bidirectional streaming directly with OpenAI endpoints.
- **Status**: active
- **Valid**: permanent
- **Source**: Human decision via interview tool
- **Date**: 2026-09-09

### D-20
- **ID**: D-20
- **Statement**: Support dual IPC transport in `kobold-server` over both Unix Domain Sockets (for fast local terminal CLI/TUI clients) and WebSockets (for web companion interfaces).
- **Alternatives rejected**: Unix Domain Sockets exclusively.
- **Reason**: Accommodates both sub-millisecond local terminal clients and browser-based frontends without altering the underlying protocol framing.
- **Status**: active
- **Valid**: permanent
- **Source**: Human decision via interview tool
- **Date**: 2026-09-09

### D-21
- **ID**: D-21
- **Statement**: Boot the Linux microVM using a minimal static Rust PID-1 agent inside `initramfs` paired with a Debian/Ubuntu ARM64 root filesystem on the persistent ext4 disk image.
- **Alternatives rejected**: Full Linux distro systemd init; Alpine musl-only root filesystem.
- **Reason**: Delivers sub-second boot without background service jitter, shields the supervisory agent in RAM, and ensures broad binary compatibility for developer toolchains (cargo, node, python).
- **Status**: active
- **Valid**: permanent
- **Source**: Human decision via interview tool
- **Date**: 2026-09-09

### D-22
- **ID**: D-22
- **Statement**: Transfer codebase changes between host and guest microVM using `git bundle`, importing into the VM on start and exporting to a dedicated host branch upon commit.
- **Alternatives rejected**: Direct host filesystem mounts; patch application to host working directory.
- **Reason**: Keeps host working tree completely clean, eliminates accidental file corruption, and presents reviews as standard git branch diffs.
- **Status**: active
- **Valid**: permanent
- **Source**: Human decision via interview tool
- **Date**: 2026-09-09

### D-23
- **ID**: D-23
- **Statement**: Communicate between host controller and guest PID-1 agent over vsock using length-prefixed JSON framing.
- **Alternatives rejected**: gRPC over vsock; unformatted raw TCP streams.
- **Reason**: Minimizes dependencies and serialization overhead while providing strongly typed command dispatch and streaming output chunks.
- **Status**: active
- **Valid**: permanent
- **Source**: Human decision via interview tool
- **Date**: 2026-09-09

### D-24
- **ID**: D-24
- **Statement**: Manage the `kobold-backend-openai` WebSocket connection with lazy connection on typing and a 5-minute idle timeout, closing idle sockets and reconnecting automatically.
- **Alternatives rejected**: Always-on 24/7 warm connection; per-turn ephemeral connection.
- **Reason**: Conserves network resources and battery, avoids idle disconnects during host sleep, while eliminating latency during active user typing.
- **Status**: active
- **Valid**: permanent
- **Source**: Human decision via interview tool
- **Date**: 2026-09-09

### D-25
- **ID**: D-25
- **Statement**: Set OpenAI server-side data retention (`store`) to `true` by default, toggleable via `/zdr on|off`. When Zero Data Retention (ZDR) is enabled (`store: false`), display a visible `[ZDR]` indicator in the client status line.
- **Alternatives rejected**: Hardcoded `store: false`; hardcoded `store: true` with no privacy toggle.
- **Reason**: Leverages provider prompt caching by default for lower costs and latency, while giving users full control over zero cloud retention with visual feedback.
- **Status**: active
- **Valid**: permanent
- **Source**: Human decision via interview tool
- **Date**: 2026-09-09

### D-26
- **ID**: D-26
- **Statement**: Define the `Backend` trait to yield an asynchronous `Stream` of typed `BackendEvent` items (e.g. `TextDelta`, `ThoughtDelta`, `ToolCall`, `Finished`) to `kobold-kernel`.
- **Alternatives rejected**: Direct `EventSink` injection into `Backend`; monolithic non-streaming turn result.
- **Reason**: Decouples backend implementations from kernel event sink internals and allows kernel to process deltas and tool calls reactively.
- **Status**: active
- **Valid**: permanent
- **Source**: Human decision via interview tool
- **Date**: 2026-09-09

### D-27
- **ID**: D-27
- **Statement**: Implement context compaction in `kobold-context` via provenance-preserving tool output pruning (collapsing old tool outputs to concise status and reference markers) before sliding intermediate conversational turns.
- **Alternatives rejected**: Lossy LLM summarization of conversation history; blind sliding window discarding task context.
- **Reason**: Avoids context dilution and token exhaustion while preventing loss of original user constraints and instruction provenance.
- **Status**: active
- **Valid**: permanent
- **Source**: Human decision and YC Paper Club analysis
- **Date**: 2026-09-09

### D-28
- **ID**: D-28
- **Statement**: Track token budgeting in `kobold-context` using a hybrid model: exact provider token counts from API response `Usage` for completed turns, and fast character heuristics (~4 characters per token) for in-flight buffers.
- **Alternatives rejected**: Heavy in-crate BPE tokenizer dependency (`tiktoken-rs`); purely heuristic counting without provider reconciliation.
- **Reason**: Eliminates heavy tokenizer lookup tables while maintaining budget accuracy reconciled on every model turn.
- **Status**: active
- **Valid**: permanent
- **Source**: Human decision via interview tool
- **Date**: 2026-09-09

### D-29
- **ID**: D-29
- **Statement**: Configure `kobold-context` to automatically discover and inject `AGENTS.md` from the project workspace root into the system prompt, strictly ignoring `CLAUDE.md`.
- **Alternatives rejected**: Supporting `CLAUDE.md` as fallback; requiring explicit CLI configuration for project rules.
- **Reason**: Standardizes Kobold on `AGENTS.md` as the canonical project instruction file.
- **Status**: active
- **Valid**: permanent
- **Source**: Human decision
- **Date**: 2026-09-09

## History
