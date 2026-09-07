# OpenAI Responses WebSocket Adapter

The `kobold-openai` crate is a native, streaming WebSocket client for OpenAI's real-time Responses API.

---

## Features

* **WebSocket Streaming**: Full-duplex bidirectional streaming with sub-20ms first-token latency.
* **Reasoning Summaries**: Native support for reasoning effort tiers (`none`, `low`, `medium`, `high`, `xhigh`, `max`) and reasoning delta streaming.
* **Server-Side & Client-Side Tools**: Enables server-side tools (e.g. web search) alongside local functions and MCP extensions.
* **Connection Resilience & Replay**: Automatic exponential backoff reconnection. If a connection drops mid-turn, Kobold replays prior context seamlessly.

---

## Usage

Set your API key in the environment or in `.kobold/settings.json`:

```sh
export LLM_API_KEY="sk-..."
./target/release/kobold --adapter ./target/release/kobold-openai
```
