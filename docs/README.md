# Kobold: The Autonomous Agent Harness & Supervisor

**Kobold** is a high-performance, lightweight execution kernel, meta-harness, and multi-lane supervisor designed for modern autonomous agent workflows.

Engineered in Rust with strict zero-cost abstractions, Kobold decouples frontends (terminal UIs, web companions, IDE plugins) from execution backends (AI coding agents like Claude Code, Antigravity, Grok Build, OpenAI Codex, headless tmux PTY sessions, or direct model providers).

---

## Key Highlights

* **Ultra-Low Latency & Minimal Footprint**:
  * **~6.1 MB idle RAM** footprint for the headless supervisor daemon.
  * **~20 µs** streaming token layout and frame render latency.
  * **~18 µs** keystroke echo latency.
  * **> 2,000 full turns/sec** throughput over Unix Domain Sockets.
  * **Flat $O(1)$ frame cost** across arbitrarily long conversation histories.
* **Dual-Seam Architecture**:
  * **Northbound Seam**: Conforms to the AG-UI specification over Unix Domain Sockets and WebSockets.
  * **Southbound Seam**: Conforms to the Agent Client Protocol (ACP) and JSON-RPC 2.0 stream pipes.
* **Multi-Lane State Machine**:
  * Concurrent, isolated execution lanes (`main`, parallel forks, sub-tasks).
  * Natural FIFO prompt queueing during active turns.
  * Instant double-strike mid-turn steering and cancellation (`Esc` `Esc`).
* **Multi-Frontend Support**:
  * **Streaming TUI**: Differential terminal renderer with OSC 9;4 hardware progress reporting and 97.5% cell elision via scroll hints.
  * **Web Companion**: Real-time browser companion on port 3000 with SHA-256 token authentication, interactive approval modals, and markdown rendering.
* **Persistent Session Management**:
  * Headless `koboldd` daemon allows seamless terminal detaches and reattaches (`kobold attach <session>`).
  * Strict single-writer UI lease guarantees no conflicting edits, with automated fallback to read-only follower mode.
  * Automatic git worktree branching (`eleven-pink-trains-a1b2`) to avoid concurrent workspace collision.

---

## Quickstart

### 1. Build from Source

```sh
# Clone repository
git clone https://github.com/p7a-os/kobold.git
cd kobold

# Build optimized release binaries
cargo build --release
```

Binaries generated in `target/release/`:
* `kobold`: The interactive Terminal UI (TUI) client and CLI.
* `koboldd`: The headless background session daemon and kernel supervisor.
* `kobold-adapter-acp`: The Agent Client Protocol adapter for driving external coding agents.
* `kobold-adapter-tmux`: The persistent PTY and tmux session bridge.
* `kobold-openai`: The OpenAI Responses WebSocket engine.
* `kobold-tts`: The local Pocket-TTS speech engine.

### 2. Launch an Interactive Session

```sh
# Start Kobold in the current repository
./target/release/kobold
```

### 3. Launch with an External Agent (ACP)

Control Claude Code, Google Antigravity, or Grok Build through Kobold:

```sh
# Run with Claude Code
ACP_AGENT_CMD=claude-code ./target/release/kobold --adapter ./target/release/kobold-adapter-acp

# Run with Antigravity
ACP_AGENT_CMD=agy ./target/release/kobold --adapter ./target/release/kobold-adapter-acp
```

### 4. Open the Web Companion

When `koboldd` is running with the web companion enabled:
```sh
# Access the web UI at the printed URL with the auto-generated auth token:
http://127.0.0.1:3000/?token=<AUTH_TOKEN>
```
