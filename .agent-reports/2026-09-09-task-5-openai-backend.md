# TASK-5: kobold-backend-openai Streaming Backend Crate

## 1. Request as understood

Build `crates/kobold-backend-openai` implementing the `Backend` trait contract from `kobold-types` using the OpenAI Responses WebSocket protocol (`wss://api.openai.com/v1/responses`). The backend must stream incremental text and reasoning deltas, tool call chunks, and response completion events as an async stream, enforce Zero Data Retention (`store: false`) when toggled, manage connection lifecycles with lazy connection and 5-minute idle timeouts, and serialize messages according to strict Responses API schema constraints (assistant messages as `output_text`, user/system as `input_text`, tool outputs as `function_call_output`).

## 2. Facts the decision depended on

- `kobold-backend-openai` implements the `Backend` trait from `kobold-types` (`D-10`, `D-26`).
- OpenAI's Responses WebSocket API operates at `wss://api.openai.com/v1/responses`, accepting `response.create` requests (`F-5`).
- Sending `input_text` for an `assistant` message causes the server to close the WebSocket; replayed assistant content must use `output_text`.
- When Zero Data Retention (ZDR) is enabled, requests specify `store: false` (`D-25`, `D-31`).
- Token usage reports from `response.completed` reconcile with the context estimator (`D-13`, `D-28`).
- TLS certificate configuration caches WebPKI roots to eliminate re-handshake latency.

## 3. Sources checked

1. `.tasks.md:76-89` - Verified TASK-5 scope and acceptance criteria.
2. `.knowledge/decisions.md:D-24, D-25, D-26, D-31` - Checked architectural decisions for OpenAI transport, ZDR, and stream contracts.
3. `kobold-openai/src/ws.rs:1-240` and `kobold-openai/src/events.rs:1-350` - Inspected existing wire models and connection drivers.
4. OpenAI Responses WebSocket documentation.

## 4. Observations

- Sequential turn orchestration in `kobold-kernel` matches OpenAI's single in-flight response constraint per WebSocket connection.
- Because `kobold-context` prunes historical tool outputs and slides intermediate turns, sending the full compacted message context on each turn avoids context desynchronization.
- Using `fastwebsockets::FragmentCollector` with `tokio-rustls` and cached roots provides zero-copy frame parsing with minimal memory allocations.
- A decoupled `WebSocketTransport` trait allows injecting mock transports for deterministic offline testing without requiring active network connectivity or valid credentials.

## 5. Decision

Implemented `crates/kobold-backend-openai`:
- `config.rs`: `OpenAiConfig` defining API keys, models, base URL, reasoning effort, ZDR toggle, idle timeouts, and egress socket routing.
- `wire.rs`: Outgoing serialization (`ResponseCreate`, `InputItem`, `ToolDefinitionWire`) strictly enforcing `output_text` for assistant and `input_text` for user/system messages, and incoming event parsing (`WireEvent`, `WireUsage`, `WireError`).
- `transport.rs`: `WebSocketTransport` contract, `LiveOpenAiTransport` with cached TLS configurations, and `MockWebSocketTransport` for unit tests.
- `backend.rs`: `OpenAiBackend` implementing `kobold_types::Backend`, returning a pinned `ReceiverStream` yielding `BackendEvent` items.
- `lib.rs`: Public re-exports and unit test suite.

## 6. Verification

Run the verification instrument:

1. Open a terminal in the repository root directory.
2. Execute the verification script:
   ```bash
   .agent-reports/2026-09-09-task-5-openai-backend/verify.sh
   ```
3. Confirm that all 5 tests pass for `kobold-backend-openai`.
4. Confirm that all 41 tests pass across the entire workspace.
5. Confirm that `cargo clippy` emits zero warnings.

Observed output:
- `cargo check -p kobold-backend-openai`: exit code 0.
- `cargo test -p kobold-backend-openai`: 5 passed, 0 failed.
- Full workspace test suite: 41 passed, 0 failed.
- `cargo clippy -p kobold-backend-openai`: exit code 0.

## 7. Unverified and risks

- Live external network calls to `wss://api.openai.com/v1/responses` require a funded OpenAI API key in the execution environment. Unit tests verified serialization, event streaming, reasoning deltas, tool call parsing, and error paths using the mock transport. Live end-to-end integration will be demonstrated during TASK-6 and TASK-7.

## 8. Out-of-scope findings

- Programmatic SDK session assembly (`kobold-runtime`) belongs to TASK-6.
- Multi-session daemon supervisor and Ratatui TUI client belong to TASK-7.
