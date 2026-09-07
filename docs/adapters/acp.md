# Agent Client Protocol (ACP) Adapter

The `kobold-adapter-acp` crate allows Kobold to act as a universal client and meta-harness for any coding agent conforming to the **Agent Client Protocol (ACP)** JSON-RPC specification.

---

## Supported Agent Engines

* **Anthropic Claude Code** (`claude-code`)
* **Google DeepMind Antigravity** (`antigravity` / `agy`)
* **xAI Grok Build** (`grok-build`)
* **OpenAI Codex CLI / Operator** (`codex`)
* **Meta Muse** (`muse`)
* **OpenCode / Smolagents** (`opencode`)

---

## Protocol Mapping

| Kobold Protocol | ACP JSON-RPC Equivalent | Purpose |
| :--- | :--- | :--- |
| `Startup` | `initialize` (Request) | Negotiates protocol version, capabilities, and agent metadata. |
| `Command::Send` | `session/prompt` (Request) | Submits a prompt turn to the agent. |
| `Command::ToolResult` | Response to `session/request_permission` | Delivers user permission (`allow` / `deny`) or tool output. |
| `Command::Cancel` | `session/cancel` (Notification) | Immediately halts in-flight generation and tool execution. |
| `Command::Quit` | `session/cancel` + process termination | Tears down the agent session. |
| `Outgoing::TextMessageContent` | `session/update` (`type: "text"`) | Streams agent markdown/text deltas. |
| `Outgoing::ReasoningMessageContent` | `session/update` (`type: "thought"`) | Streams chain-of-thought / reasoning deltas. |
| `Outgoing::ToolCallStart / End` | `session/update` (`type: "tool_call"`) | Signals tool execution lifecycle. |

---

## Usage

Set `ACP_AGENT_CMD` and `ACP_AGENT_ARGS` in the environment when starting `kobold` or `koboldd`:

```sh
# Launch with Claude Code
export ACP_AGENT_CMD="claude-code"
export ACP_AGENT_ARGS="--model claude-3-7-sonnet"
./target/release/kobold --adapter ./target/release/kobold-adapter-acp

# Launch with Antigravity
export ACP_AGENT_CMD="agy"
export ACP_AGENT_ARGS="--headless"
./target/release/kobold --adapter ./target/release/kobold-adapter-acp
```

---

## Steering & Permission Interception

When an agent requests confirmation to execute a sensitive command (e.g. `bash: cargo test`), `kobold-adapter-acp`:

1. Translates `session/request_permission` into an AG-UI question interrupt.
2. The user inspects and selects `allow` or `deny` in the TUI or Web Companion.
3. The answer is packaged into `Command::ToolResult` and returned to the agent, guiding its trajectory without aborting the session.
