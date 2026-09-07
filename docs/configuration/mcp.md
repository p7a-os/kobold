# Model Context Protocol (MCP) Sandbox

Kobold natively supports external tool execution through the **Model Context Protocol (MCP)** specification.

---

## Security & Credential Isolation

Unlike traditional tool runners, Kobold enforces strict credential sandboxing for all MCP subprocesses:

1. **Clean Environment**: MCP servers do **not** inherit Kobold's environment variables.
2. **Key Protection**: Secrets like `LLM_API_KEY` or `OPENAI_API_KEY` are stripped before spawning MCP children.
3. **Explicit Grants**: Any credential an MCP server requires must be explicitly specified in its own `env` block.

```json
{
  "name": "github",
  "command": "bunx",
  "args": ["-y", "@modelcontextprotocol/server-github"],
  "env": {
    "GITHUB_TOKEN": "ghp_..."
  }
}
```

---

## Sandboxed Egress Broker

When sandboxing is active:
* Child processes cannot dial arbitrary remote IP addresses.
* All external network traffic is routed through Kobold's local egress broker.
* The broker enforces destination allowlists and logs egress connections.
