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

## History
