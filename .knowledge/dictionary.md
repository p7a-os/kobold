# Dictionary

## Current

### T-kobold
- **ID**: T-kobold
- **Term**: Kobold
- **Definition**: The autonomous coding agent supervisor and multi-lane execution harness. Decouples user-facing frontends from backend agent runtimes.
- **Rejected aliases**: `kobold-rs`, `kobold-supervisor`
- **Status**: active
- **Source**: `Cargo.toml:8`, `README.md:14`
- **Date**: 2026-09-08

### T-koboldd
- **ID**: T-koboldd
- **Term**: koboldd
- **Definition**: The headless background daemon process supervising agent sessions, execution lanes, and child adapter processes. Listens on Unix Domain Sockets and WebSockets.
- **Rejected aliases**: `kobold-daemon`, `server`
- **Status**: active
- **Source**: `src/bin/koboldd.rs:1-12`, `docs/architecture/kernel.md:3`
- **Date**: 2026-09-08

### T-northbound-seam
- **ID**: T-northbound-seam
- **Term**: Northbound Seam
- **Definition**: The client-to-kernel communication boundary connecting frontends to `koboldd`. Carries multiplexed AG-UI frames over Unix Domain Sockets or WebSockets.
- **Rejected aliases**: `client-api`, `frontend-protocol`
- **Status**: active
- **Source**: `docs/architecture/seams.md:7-14`, `kobold-proto/src/northbound.rs:1-13`
- **Date**: 2026-09-08

### T-southbound-seam
- **ID**: T-southbound-seam
- **Term**: Southbound Seam
- **Definition**: The kernel-to-adapter execution boundary connecting `koboldd` to child agent adapters. Communicates via standard I/O pipes passing single-line JSON commands and events.
- **Rejected aliases**: `backend-pipe`, `adapter-protocol`
- **Status**: active
- **Source**: `docs/architecture/seams.md:71-78`, `kobold-proto/src/lib.rs:1-10`
- **Date**: 2026-09-08

### T-execution-kernel
- **ID**: T-execution-kernel
- **Term**: Kernel
- **Definition**: The core execution engine in `crates/kobold-kernel` orchestrating the agentic turn loop, tool dispatch, and event streaming across abstract `Backend` and `Tool` traits without direct I/O dependencies.
- **Rejected aliases**: `engine`, `orchestrator`
- **Status**: active
- **Source**: `crates/kobold-kernel`, D-10
- **Date**: 2026-09-08

### T-kobold-types
- **ID**: T-kobold-types
- **Term**: kobold-types
- **Definition**: The foundational crate defining common types, messages, event models, and abstract trait contracts (`Backend`, `Tool`, `EventSink`) without operational logic or heavy external dependencies.
- **Rejected aliases**: `common`, `contracts`, `interfaces`
- **Status**: active
- **Source**: Architecture planning
- **Date**: 2026-09-08

### T-kobold-context
- **ID**: T-kobold-context
- **Term**: kobold-context
- **Definition**: The crate managing conversation history, token budgeting, prompt construction, and context compaction strategies across model context windows.
- **Rejected aliases**: `session`, `history`
- **Status**: active
- **Source**: Architecture planning, D-12
- **Date**: 2026-09-08

### T-kobold-runtime
- **ID**: T-kobold-runtime
- **Term**: kobold-runtime
- **Definition**: The in-process SDK crate that assembles workspace configuration, system prompts, context, tools, and the kernel into a high-level `Session` API with zero socket or IPC overhead.
- **Rejected aliases**: `kobold-sdk`, `agent-runner`
- **Status**: active
- **Source**: Human decision via interview tool, D-32
- **Date**: 2026-09-09

### T-event-sink
- **ID**: T-event-sink
- **Term**: Event Sink
- **Definition**: An abstract trait contract in `kobold-types` (`EventSink`) through which `kobold-kernel` emits turn events to consumers without depending on concrete asynchronous runtimes.
- **Rejected aliases**: `event-channel`, `event-dispatcher`
- **Status**: active
- **Source**: Architecture planning, D-13
- **Date**: 2026-09-08

### T-approval-policy
- **ID**: T-approval-policy
- **Term**: Approval Policy
- **Definition**: A trait contract in `kobold-types` evaluating tool calls before dispatch to determine whether they run automatically, are rejected, or require interactive human authorization.
- **Rejected aliases**: `permission-gate`, `security-filter`
- **Status**: active
- **Source**: Architecture planning, D-14
- **Date**: 2026-09-08

### T-zdr
- **ID**: T-zdr
- **Term**: Zero Data Retention
- **Definition**: A user-toggleable privacy mode (`/zdr on|off`) setting `store: false` on OpenAI provider requests to prevent server-side retention of conversation and codebase data.
- **Rejected aliases**: `private-mode`, `no-store`
- **Status**: active
- **Source**: Architecture planning, D-25
- **Date**: 2026-09-08

### T-pruning-policy
- **ID**: T-pruning-policy
- **Term**: Pruning Policy
- **Definition**: A specification attached to tool outputs dictating how historical tool execution results are compacted (`Never`, `KeepLast`, `HeadTail`, `CollapseAfterTurns`, `Summary`) by `kobold-context` when token budget limits are approached.
- **Rejected aliases**: `compaction-hint`, `tool-truncation`
- **Status**: active
- **Source**: Human decision via interview tool, D-30
- **Date**: 2026-09-09

### T-lane
- **ID**: T-lane
- **Term**: Lane
- **Definition**: An isolated conversation and execution track with its own state machine, prompt queue, and history cursor. The default lane is `main`.
- **Rejected aliases**: `thread`, `channel`, `pane`
- **Status**: active
- **Source**: `kobold-core/src/lane.rs:1-10`, `docs/architecture/kernel.md:7-10`
- **Date**: 2026-09-08

### T-transcript-dag
- **ID**: T-transcript-dag
- **Term**: Transcript DAG
- **Definition**: An append-only Directed Acyclic Graph of immutable conversation turns logged in `.kobold/transcript.jsonl`. Supports branching via forks and checkpoints without history loss.
- **Rejected aliases**: `chat-history`, `message-log`
- **Status**: active
- **Source**: `docs/architecture/transcripts.md:1-5`, `kobold-core/src/transcript.rs`
- **Date**: 2026-09-08

### T-prompt-queue
- **ID**: T-prompt-queue
- **Term**: Prompt Queue
- **Definition**: A first-in, first-out buffer per lane that stores user inputs received while an agent is executing. Drains automatically when the active turn completes.
- **Rejected aliases**: `input-buffer`, `message-queue`
- **Status**: active
- **Source**: `docs/architecture/kernel.md:32-41`, `kobold-core/src/kernel.rs:164-166, 441-455`
- **Date**: 2026-09-08

### T-steering
- **ID**: T-steering
- **Term**: Mid-Turn Steering
- **Definition**: An interruption mechanism triggered by double-tapping `Esc` in the TUI or clicking Stop in the Web Companion. Cancels the active turn and returns the lane to `Ready` for corrective input.
- **Rejected aliases**: `cancellation`, `preemption`
- **Status**: active
- **Source**: `docs/architecture/kernel.md:44-61`, `docs/usage/tui.md:33-38`
- **Date**: 2026-09-08

### T-ui-lease
- **ID**: T-ui-lease
- **Term**: UI Lease
- **Definition**: A single-writer concurrency token held by exactly one attached frontend. Secondary frontends become read-only followers that mirror events but cannot submit inputs.
- **Rejected aliases**: `writer-lock`, `client-lock`
- **Status**: active
- **Source**: `docs/configuration/worktrees.md:7-16`, `kobold-core/src/daemon.rs:188-210`
- **Date**: 2026-09-08

### T-worktree-collision
- **ID**: T-worktree-collision
- **Term**: Worktree Collision Isolation
- **Definition**: An automated mechanism that creates a dedicated git worktree (`{number}-{adjective}-{vehicle}-{hex}`) when Kobold launches in a repository with an existing daemon session.
- **Rejected aliases**: `auto-branching`, `workspace-isolation`
- **Status**: active
- **Source**: `docs/configuration/worktrees.md:19-32`, `kobold-core/src/worktree.rs`
- **Date**: 2026-09-08

### T-acp-adapter
- **ID**: T-acp-adapter
- **Term**: ACP Adapter
- **Definition**: The `kobold-adapter-acp` binary bridging Kobold's Southbound seam to external coding agents speaking the Agent Client Protocol JSON-RPC specification.
- **Rejected aliases**: `agent-adapter`, `acp-bridge`
- **Status**: active
- **Source**: `docs/adapters/acp.md:1-5`, `kobold-adapter-acp/src/main.rs:1-5`
- **Date**: 2026-09-08

### T-tmux-adapter
- **ID**: T-tmux-adapter
- **Term**: Tmux Adapter
- **Definition**: The `kobold-adapter-tmux` binary managing persistent pseudo-terminal (PTY) and tmux sessions for shell tasks and build loops.
- **Rejected aliases**: `pty-adapter`, `terminal-harness`
- **Status**: active
- **Source**: `docs/adapters/tmux-pty.md:1-5`, `kobold-adapter-tmux/src/main.rs`
- **Date**: 2026-09-08

### T-openai-adapter
- **ID**: T-openai-adapter
- **Term**: OpenAI Adapter
- **Definition**: The `kobold-openai` binary providing direct bidirectional WebSocket streaming for OpenAI Realtime and Responses APIs.
- **Rejected aliases**: `realtime-adapter`, `responses-client`
- **Status**: active
- **Source**: `docs/adapters/openai.md:1-5`, `kobold-openai/src/main.rs`
- **Date**: 2026-09-08

### T-egress-broker
- **ID**: T-egress-broker
- **Term**: Egress Broker
- **Definition**: A local Unix Domain Socket proxy that mediates and allowlists all external network connections made by sandboxed child processes.
- **Rejected aliases**: `network-filter`, `proxy`
- **Status**: active
- **Source**: `docs/configuration/mcp.md:28-34`, `kobold-core/src/broker.rs`
- **Date**: 2026-09-08

### T-mcp
- **ID**: T-mcp
- **Term**: Model Context Protocol
- **Definition**: An open specification for external tool integration. Kobold hosts MCP servers in sanitized environments with sensitive API keys stripped.
- **Rejected aliases**: `tools-spec`, `plugins`
- **Status**: active
- **Source**: `docs/configuration/mcp.md:1-14`, `src/mcp/mod.rs`
- **Date**: 2026-09-08

### T-web-companion
- **ID**: T-web-companion
- **Term**: Web Companion
- **Definition**: A browser interface served by `koboldd` on `http://127.0.0.1:3000` with SHA-256 token authentication, diff inspection, and live thought logs.
- **Rejected aliases**: `web-ui`, `browser-client`
- **Status**: active
- **Source**: `docs/usage/web-companion.md:1-5`, `kobold-core/src/ws.rs`
- **Date**: 2026-09-08

### T-tui
- **ID**: T-tui
- **Term**: Terminal User Interface
- **Definition**: The terminal frontend built on Ratatui with differential scroll-hint rendering, Markdown parsing, and constant-time frame layout.
- **Rejected aliases**: `terminal-ui`, `cli-interface`
- **Status**: active
- **Source**: `docs/usage/tui.md:1-5`, `docs/performance/benchmarks.md:23-38`
- **Date**: 2026-09-08

### T-tts-engine
- **ID**: T-tts-engine
- **Term**: Pocket-TTS
- **Definition**: The local neural text-to-speech synthesis binary `kobold-tts` that generates spoken agent replies without external cloud API dependencies.
- **Rejected aliases**: `voice-engine`, `tts`
- **Status**: active
- **Source**: `kobold-tts/src/main.rs`, `README.md:47`
- **Date**: 2026-09-08

### T-agui-protocol
- **ID**: T-agui-protocol
- **Term**: AG-UI Protocol
- **Definition**: An event stream protocol specification for AI agent user interfaces. Kobold consumes 15 core event variants spanning runs, text deltas, reasoning boundaries, and tool calls.
- **Rejected aliases**: `agui`, `agent-ui-spec`
- **Status**: active
- **Source**: `docs/agui.md:1-5`, `kobold-proto/src/agui.rs`
- **Date**: 2026-09-08

### T-wizard
- **ID**: T-wizard
- **Term**: Setup Wizard
- **Definition**: An interactive first-run onboarding screen that discovers installed agents, configures providers, installs adapters, and writes settings.
- **Rejected aliases**: `onboarding`, `config-wizard`
- **Status**: active
- **Source**: `src/wizard.rs:1-9`, `src/main.rs`
- **Date**: 2026-09-08

### T-doctor
- **ID**: T-doctor
- **Term**: Kobold Doctor
- **Definition**: The diagnostic health-check command (`kobold doctor`) that probes installed agent binaries in parallel and verifies execution paths.
- **Rejected aliases**: `health-check`, `agent-checker`
- **Status**: active
- **Source**: `src/doctor.rs:1-6`, `src/main.rs:364-368`
- **Date**: 2026-09-08

### T-encrypted-reasoning
- **ID**: T-encrypted-reasoning
- **Term**: Encrypted Reasoning Item
- **Definition**: An opaque provider token (`REASONING_ENCRYPTED_VALUE`) that carries reasoning context across turns under zero cloud retention.
- **Rejected aliases**: `reasoning-state`, `thought-token`
- **Status**: active
- **Source**: `docs/agui.md:207-226`
- **Date**: 2026-09-08

### T-habrid
- **ID**: T-habrid
- **Term**: Habrid
- **Definition**: A multi-agent handoff CLI enabling cross-harness discussion threads and peer review between coding agents via Beads.
- **Rejected aliases**: `agent-handoff`, `beads-bridge`
- **Status**: active
- **Source**: `AGENTS.md:204`, `.agents/skills/habrid/SKILL.md:1-10`
- **Date**: 2026-09-08

### T-meta-harness
- **ID**: T-meta-harness
- **Term**: Meta-Harness
- **Definition**: An execution supervisor that drives external purpose-built coding agents (Claude Code, Codex, Grok Build) as backends rather than competing with them.
- **Rejected aliases**: `agent-wrapper`, `orchestrator-of-agents`
- **Status**: active
- **Source**: `/Users/luis/w/kobold.pdf:1`
- **Date**: 2026-09-08

### T-batch-inference
- **ID**: T-batch-inference
- **Term**: Batch Inference Routing
- **Definition**: Automatic offloading of non-interactive tasks (sweeps, migrations, test generation) to provider batch processing lanes at reduced pricing.
- **Rejected aliases**: `background-queue`, `delayed-inference`
- **Status**: active
- **Source**: `/Users/luis/w/kobold.pdf:2`
- **Date**: 2026-09-08

### T-aleph
- **ID**: T-aleph
- **Term**: Aleph
- **Definition**: The ParadigmaOS project knowledge and memory system that records verified claims, decisions, evidence, and dates to eliminate cold-start sessions.
- **Rejected aliases**: `project-memory`, `knowledge-store`
- **Status**: active
- **Source**: `/Users/luis/w/kobold.pdf:3`
- **Date**: 2026-09-08

### T-mithlond
- **ID**: T-mithlond
- **Term**: Mithlond
- **Definition**: The ParadigmaOS unified AI gateway providing single-endpoint billing and credential isolation across models, agents, and MCP servers.
- **Rejected aliases**: `ai-proxy`, `gateway`
- **Status**: active
- **Source**: `/Users/luis/w/kobold.pdf:3`
- **Date**: 2026-09-08

### T-olympus
- **ID**: T-olympus
- **Term**: Olympus
- **Definition**: The ParadigmaOS engineering workflow platform in which Kobold operates autonomously as a supervisor or worker in scheduled pipelines.
- **Rejected aliases**: `ci-runner`, `workflow-engine`
- **Status**: active
- **Source**: `/Users/luis/w/kobold.pdf:3`
- **Date**: 2026-09-08

## History
