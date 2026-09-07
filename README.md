# Kobold

[![CI](https://github.com/p7a-os/kobold/actions/workflows/ci.yml/badge.svg)](https://github.com/p7a-os/kobold/actions)
[![Docs](https://github.com/p7a-os/kobold/actions/workflows/docs.yml/badge.svg)](https://p7a-os.github.io/kobold)
[![License: AGPL v3](https://img.shields.io/badge/License-AGPL_v3-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-1.93%2B-orange.svg)](rust-toolchain.toml)
[![Status: Alpha](https://img.shields.io/badge/Status-Alpha-orange.svg)](#)

> [!CAUTION]
> **Alpha Development Phase — Try at Your Own Risk**
>
> Kobold is currently in an active **alpha development phase**. APIs, configuration schemas, adapter protocols, and CLI flags are subject to change. Autonomous coding agents supervised by Kobold execute system actions, terminal commands, and file edits. Supervise operations carefully and run in non-critical environments or isolated git worktrees.

**Blazing-fast, decoupled multi-lane harness and streaming client for autonomous coding agents.**

```
   ┌───────────────────────────────────────────────────────────┐
   │                    NORTHBOUND SEAM                        │
   │   Ratatui TUI    │   Web Companion (WS)   │   Headless CI │
   └─────────────────────────────┬─────────────────────────────┘
                                 │ UDS / AG-UI Frames
   ┌─────────────────────────────▼─────────────────────────────┐
   │                    KOBOLD KERNEL (koboldd)                │
   │  Multi-Lane Engine │ Concurrency Leases │ DAG Transcripts │
   │  Mid-Turn Steering │ Worktree Isolation │ Sandboxing      │
   └─────────────────────────────┬─────────────────────────────┘
                                 │ Stdio / IPC Lines
   ┌─────────────────────────────▼─────────────────────────────┐
   │                    SOUTHBOUND SEAM                        │
   │  kobold-adapter-acp   │ kobold-adapter-tmux │ kobold-openai │
   │  (Claude, Antigravity,│ (Persistent PTY     │ (Realtime WS) │
   │   Codex, Grok, etc.)  │  & Terminal agents) │               │
   └───────────────────────────────────────────────────────────┘
```

---

## Highlights

- ⚡ **Ultra-Low Latency**: Sub-millisecond IPC turn roundtrips over Unix Domain Sockets (~0.48 ms), ~18 µs keystroke/stream frame render times.
- 🪶 **Minimal RAM Footprint**: Starts cold in ~200 ms, idles at **6.1 MB RSS**, and uses <8 MB under active turn bursts.
- 🔄 **Universal Agent Client Protocol (ACP)**: Drive [Claude Code](docs/adapters/acp.md), [Google Antigravity](docs/adapters/acp.md), [xAI Grok Build](docs/adapters/acp.md), [OpenAI Codex](docs/adapters/acp.md), and [OpenCode](docs/adapters/acp.md) with permission negotiation and tool inspection.
- 🛑 **Live Steering & Mid-Flight Cancellation**: Double-tap `Esc` in the TUI or click **Stop** in the Web Companion to preempt and steer running agents mid-flight without breaking session state.
- 🌐 **Web Companion UI**: Built-in zero-dependency dark-mode web companion (`--ws-port <PORT>`) with reactive status badges, diff cards, and live thought logs.
- 💻 **Virtualized $O(1)$ TUI**: Full-screen Ratatui terminal UI with Markdown rendering, syntax highlighting, readline keybindings, and flat $O(1)$ render scaling across deep conversation histories.
- 🔒 **Hermetic Sandboxing & Broker**: Subprocesses execute sandboxed via `bwrap` (Linux) or `sandbox-exec` (macOS) with egress broker allowlisting.
- 🎙️ **Native Neural TTS**: Optional built-in Pocket-TTS speech synthesis engine (`kobold-tts`) for spoken agent replies with zero cloud API keys.

---

## Quickstart

### Instant Install (macOS & Linux)

Install the prebuilt binaries or build from source automatically:

```sh
curl -fsSL https://raw.githubusercontent.com/p7a-os/kobold/main/install.sh | bash
```

### Build from Source

Prerequisites: A standard C compiler (for `ring` crypto). Optional: `cmake` (only required if compiling the Pocket-TTS tokenizer).

```sh
git clone https://github.com/p7a-os/kobold.git
cd kobold
cargo build --release
```

Binaries are emitted to `target/release/`:
- `kobold`: The main CLI and interactive TUI client.
- `koboldd`: The multi-lane kernel daemon.
- `kobold-adapter-acp`: Southbound Agent Client Protocol (ACP) adapter.
- `kobold-adapter-tmux`: Southbound persistent PTY / tmux harness adapter.
- `kobold-openai`: Southbound OpenAI Realtime WebSocket streaming adapter.
- `kobold-tts`: Pocket-TTS neural voice synthesis engine.

---

### 2. Running with Your Preferred Agent

#### Option A: Drive External Agents via ACP (Claude Code, Antigravity, Codex, etc.)

Point `ACP_AGENT_CMD` to an ACP-compatible agent server:

```sh
# Instant demo (no external tools or API keys required - uses built-in mock loop)
./target/release/kobold -a kobold-adapter-acp -p "Hello Kobold"

# Claude Code (using the official ACP bridge)
ACP_AGENT_CMD="bunx -y @agentclientprotocol/claude-agent-acp" ./target/release/kobold -a kobold-adapter-acp

# Google Antigravity / xAI Grok / OpenAI Codex
ACP_AGENT_CMD="antigravity" ./target/release/kobold -a kobold-adapter-acp
ACP_AGENT_CMD="codex" ./target/release/kobold -a kobold-adapter-acp
```

#### Option B: Launch with Web Companion UI

Launch Kobold with the WebSocket companion enabled:

```sh
# Interactive TUI + Web Companion on port 3000
./target/release/kobold --ws-port 3000

# Or with ACP adapter on port 3000
./target/release/kobold -a kobold-adapter-acp --ws-port 3000
```
Open `http://localhost:3000` in any browser to see the live conversation, status badges, diffs, and execution controls alongside the terminal.

#### Option C: Native OpenAI Streaming Models

```sh
export LLM_API_KEY="sk-proj-..."
./target/release/kobold
```

#### Option D: Persistent PTY / Tmux Sessions

```sh
./target/release/kobold -a kobold-adapter-tmux
```

---

### 3. Headless & Scripting Usage

Execute single turns directly from scripts or CI pipelines without opening the TUI:

```sh
./target/release/kobold -p "Explain ownership in Rust"
git diff | ./target/release/kobold -p "Generate a concise conventional commit message"
```

---

### 4. Daemon & Session Management

Kobold runs as a persistent background daemon (`koboldd`). Sessions detach safely without terminating in-flight model or tool runs:

```sh
# Start a detached background session
./target/release/kobold --detach --session feature-login

# List active sessions across the machine
./target/release/kobold list

# Re-attach to a running session
./target/release/kobold attach feature-login

# Terminate a background session
./target/release/kobold kill feature-login
```

> **Tip**: Run `cargo install --path .` to place `kobold` directly on your PATH, allowing you to invoke bare `kobold` from any directory.

---

## TUI Keybindings

| Key | Action |
|---|---|
| `Enter` | Submit prompt (or queue behind an active turn) |
| `Shift+Enter` / `Alt+Enter` / `Ctrl+J` | Insert newline in multi-line prompt |
| `Esc` | Clear queued prompts |
| `Esc Esc` (Double tap) | Cancel / steer active in-flight turn (`Command::Cancel`) |
| `Up` / `Down` | Browse prompt history (or navigate `/` menu) |
| `Tab` | Autocomplete slash command |
| `Ctrl+C` | Graceful exit |
| `Ctrl+D` (Twice) | Detach from session without terminating daemon |
| `/help` | List available slash commands |
| `/voice` | Toggle local voice readout |
| `/fork` | Fork conversation branch in transcript DAG |

---

## Configuration

Kobold stores configuration in `.kobold/settings.json` (auto-generated on first launch). Environment variables override file settings:

| Setting / Variable | Purpose | Default |
|---|---|---|
| `LLM_API_KEY` / `OPENAI_API_KEY` | Cloud provider credential | — |
| `KOBOLD_MODEL` | Target LLM model name | `gpt-5.6-luna` |
| `KOBOLD_EFFORT` | Reasoning effort (`none`, `low`, `medium`, `high`, `max`) | `medium` |
| `ACP_AGENT_CMD` | Subprocess command for ACP adapter | — |
| `ACP_AGENT_ARGS` | Arguments passed to ACP child agent | `[]` |
| `KOBOLD_TTS_VOICE` | Voice persona (`alba`, `marius`, `javert`, `jean`, `cosette`) | `alba` |

See [Configuration Guide](docs/configuration/settings.md) for full settings reference.

---

## Architecture & Documentation

Comprehensive documentation is available in the [`docs/`](docs/README.md) directory and online at [https://p7a-os.github.io/kobold](https://p7a-os.github.io/kobold):

- [Architecture Overview](docs/architecture/README.md)
- [Dual-Seam Model](docs/architecture/seams.md)
- [AG-UI Protocol Specification](docs/agui.md)
- [Kernel & Lane Engine](docs/architecture/kernel.md)
- [Transcript DAG & Branching](docs/architecture/transcripts.md)
- [ACP Adapter Guide](docs/adapters/acp.md)
- [Tmux / PTY Adapter](docs/adapters/tmux-pty.md)
- [OpenAI Realtime Adapter](docs/adapters/openai.md)
- [Worktree Concurrency & UI Leases](docs/configuration/worktrees.md)
- [MCP Sandboxing](docs/configuration/mcp.md)
- [Performance & Footprint Audit](docs/performance/benchmarks.md)

To preview the documentation locally with [docmd](https://docmd.io):
```sh
bunx @docmd/core dev
```

---

## Testing

Kobold enforces a strict 5-tier test taxonomy with 100% offline determinism:

```sh
cargo test --workspace --all-targets
```

Includes:
- **Unit Tests**: Low-level protocol codecs and data structures.
- **Integration Tests**: In-process channels, mock agents, and broker proxies.
- **Contract Tests**: JSON schema validation against AG-UI and ACP specifications.
- **Property Tests**: `proptest` roundtrips across arbitrary inputs and token floods.
- **E2E Tests**: Subprocess daemons, detached socket life-cycles, multi-agent ACP orchestration, and performance benchmarks.

---

## License

GNU Affero General Public License v3.0 (`AGPL-3.0`). See [LICENSE](LICENSE) for details.
