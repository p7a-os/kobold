# Settings & Environment Variables

Kobold resolves configuration following a strict precedence hierarchy:

$$\text{Command-Line Flags} > \text{Environment Variables} > \text{.kobold/settings.json} > \text{Defaults}$$

---

## Configuration File: `.kobold/settings.json`

Created automatically with defaults upon the first execution.

```json
{
  "model": "gpt-5.6-luna",
  "reasoning_effort": "medium",
  "context_window": 128000,
  "code_bg": 236,
  "voice": {
    "enabled": false,
    "command": "",
    "voice": "alba",
    "audio_addr": ""
  },
  "mcp_servers": [],
  "server_tools": []
}
```

### Options Reference

| Key | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `model` | string | `gpt-5.6-luna` | Default model identifier. |
| `reasoning_effort` | string | `medium` | Effort tier: `none`, `low`, `medium`, `high`, `xhigh`, `max`. |
| `context_window` | integer | `128000` | Token capacity driving the visual context gauge. |
| `code_bg` | integer | `236` | ANSI 256-color palette index behind formatted code blocks. |
| `voice.enabled` | boolean | `false` | Enable Pocket-TTS voice synthesis at startup. |
| `voice.voice` | string | `alba` | Speech voice (`alba`, `marius`, `javert`, `jean`, `fantine`, `cosette`, `eponine`, `azelma`). |
| `mcp_servers` | array | `[]` | MCP servers to spawn at startup (see [MCP Guide](mcp.md)). |

---

## Environment Variables

| Variable | Overrides | Description |
| :--- | :--- | :--- |
| `LLM_API_KEY` | - | Primary API authentication key (fallback to `OPENAI_API_KEY`). |
| `KOBOLD_MODEL` | `model` | Model name override. |
| `KOBOLD_EFFORT` | `reasoning_effort` | Reasoning effort tier. |
| `KOBOLD_CONTEXT_WINDOW` | `context_window` | Context window size in tokens. |
| `KOBOLD_CODE_BG` | `code_bg` | Code block background color index. |
| `ACP_AGENT_CMD` | - | Command binary to execute for ACP adapter (e.g. `claude-code`, `agy`). |
| `ACP_AGENT_ARGS` | - | Arguments passed to the ACP agent child process. |
| `KOBOLD_NAGLE` | - | If set, leaves Nagle's algorithm enabled (defaults to `TCP_NODELAY`). |
| `KOBOLD_STATS` | - | If set, prints render frame latency statistics on exit. |
