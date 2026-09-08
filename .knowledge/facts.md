# Facts

## Current

### F-1
- **ID**: F-1
- **Statement**: Kobold compiles as a multi-package Cargo workspace comprising the root package `kobold`, `kobold-core`, `kobold-proto`, `kobold-adapter-acp`, `kobold-adapter-tmux`, `kobold-openai`, and `kobold-tts`.
- **Status**: verified
- **Valid**: permanent
- **Source**: `Cargo.toml:1-6`, executed `cargo check --workspace --all-targets`
- **Date**: 2026-09-08

### F-2
- **ID**: F-2
- **Statement**: The Northbound Seam supports local IPC over Unix Domain Sockets (`/tmp/kobold-{uid}/{session_id}.sock`) and remote connections over WebSockets (`ws://127.0.0.1:3000/ws`).
- **Status**: verified
- **Valid**: permanent
- **Source**: `docs/architecture/seams.md:12-14`, `kobold-core/src/daemon.rs:25-35`
- **Date**: 2026-09-08

### F-3
- **ID**: F-3
- **Statement**: The Southbound Seam communicates over anonymous standard I/O pipes using single-line JSON framing, initiated by a mandatory `Startup` frame on stdin before any `Command`.
- **Status**: verified
- **Valid**: permanent
- **Source**: `docs/architecture/seams.md:75-78`, `kobold-proto/src/lib.rs:18-30, 130-152`
- **Date**: 2026-09-08

### F-4
- **ID**: F-4
- **Statement**: `koboldd` enforces a single-writer UI Lease (`rw_client_id`) granting write authority to one attached frontend at a time, while all other attached frontends operate in read-only follower mode.
- **Status**: verified
- **Valid**: permanent
- **Source**: `docs/configuration/worktrees.md:9-15`, `kobold-core/src/daemon.rs:188-210`
- **Date**: 2026-09-08

### F-5
- **ID**: F-5
- **Statement**: Worktree Collision Isolation automatically detects if Kobold launches in a directory with an active session and prompts or creates an isolated git worktree named `{number}-{adjective}-{vehicle}-{hex}`.
- **Status**: verified
- **Valid**: permanent
- **Source**: `docs/configuration/worktrees.md:21-32`, `src/main.rs:462-493`, `kobold-core/src/worktree.rs`
- **Date**: 2026-09-08

### F-6
- **ID**: F-6
- **Statement**: Conversation histories are stored as an immutable Transcript DAG in an append-only JSONL log (`.kobold/transcript.jsonl`), enabling non-destructive forks and rewinds.
- **Status**: verified
- **Valid**: permanent
- **Source**: `docs/architecture/transcripts.md:1-30`, `kobold-core/src/transcript.rs`
- **Date**: 2026-09-08

### F-7
- **ID**: F-7
- **Statement**: Child processes for adapters and MCP servers are confined via Bubblewrap on Linux (`bwrap`) or Seatbelt on macOS (`sandbox-exec`), routing outbound network traffic through a local Egress Broker proxy.
- **Status**: verified
- **Valid**: permanent
- **Source**: `kobold-core/src/sandbox.rs`, `kobold-core/src/broker.rs`
- **Date**: 2026-09-08

### F-8
- **ID**: F-8
- **Statement**: Kobold implements a subset of the AG-UI Protocol consuming 15 of 33 event types (11 emitted and consumed, 4 consumed only) and ignores server-side history snapshots, activity deltas, and step events.
- **Status**: verified
- **Valid**: permanent
- **Source**: `docs/agui.md:52-71, 671-678`, `kobold-proto/src/agui.rs`
- **Date**: 2026-09-08

### F-9
- **ID**: F-9
- **Statement**: Mid-Turn Steering cancels an in-flight turn upon double-tapping `Esc` in the Terminal User Interface or clicking Stop in the Web Companion, resetting the Lane to `Ready` without dropping session state.
- **Status**: verified
- **Valid**: permanent
- **Source**: `docs/architecture/kernel.md:44-61`, `kobold-core/src/kernel.rs:233-243`
- **Date**: 2026-09-08

### F-10
- **ID**: F-10
- **Statement**: Model Context Protocol subprocess environments strip sensitive variables including `LLM_API_KEY` and `OPENAI_API_KEY` unless explicitly passed in the server's own configuration block.
- **Status**: verified
- **Valid**: permanent
- **Source**: `docs/configuration/mcp.md:9-14`, `kobold-core/src/childenv.rs`
- **Date**: 2026-09-08

### F-11
- **ID**: F-11
- **Statement**: `park_ask` leaves the internal lane status unchanged upon tool question parking, allowing queued prompts in the Prompt Queue to dispatch while a question remains unanswered on screen.
- **Status**: verified
- **Valid**: until resolved in kernel state machine
- **Source**: `docs/agui.md:576-604`, `src/app.rs`
- **Date**: 2026-09-08

### F-12
- **ID**: F-12
- **Statement**: In the interactive Setup Wizard, direct provider connections for Anthropic and Google are marked disabled pending implementation, while OpenAI and OpenRouter are selectable.
- **Status**: verified
- **Valid**: permanent
- **Source**: `src/wizard.rs:362-363`
- **Date**: 2026-09-08

### F-13
- **ID**: F-13
- **Statement**: The macOS Seatbelt sandbox profile network-outbound exception for Unix Domain Sockets is based on the assumption that Seatbelt counts Unix domain sockets under `network*`.
- **Status**: assumed
- **Valid**: until verified by a live test on macOS
- **Source**: `kobold-core/src/sandbox.rs:408-417`
- **Date**: 2026-09-08

### F-14
- **ID**: F-14
- **Statement**: The project integrates Habrid via `.agents/skills/habrid/SKILL.md` and `.codex/skills/habrid/SKILL.md` for multi-agent inquiry and transcript handoff across agent harnesses.
- **Status**: verified
- **Valid**: permanent
- **Source**: `AGENTS.md:204`, `.agents/skills/habrid/SKILL.md:1-10`
- **Date**: 2026-09-08

### F-15
- **ID**: F-15
- **Statement**: The ParadigmaOS product vision document (`/Users/luis/w/kobold.pdf`) specifies Kobold as a dual Harness and Meta-Harness providing multi-model composition per task, automated batch inference routing, credential injection at the egress boundary, four surfaces (Terminal, Web, Mobile, Desktop), remote persistent sandboxes, and native hooks into Mithlond, Aleph, and Olympus.
- **Status**: verified
- **Valid**: permanent
- **Source**: `/Users/luis/w/kobold.pdf:1-4`
- **Date**: 2026-09-08

## History
