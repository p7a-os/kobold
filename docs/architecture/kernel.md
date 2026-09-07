# Execution Kernel & Multi-Lane Engine

At the core of Kobold is the headless **Kernel** (`kobold-core::Kernel`), responsible for turn orchestration, lane state machines, prompt queueing, and tool execution.

---

## Lane State Machine

A **Lane** represents an isolated thread of interaction. The primary lane is named `main`, and additional parallel lanes can be spawned via forking (`Fork`).

```mermaid
stateDiagram-v2
    [*] --> Connecting: Spawn adapter
    Connecting --> Ready: Adapter emits Transport::Connected
    Ready --> Waiting: Command::Send dispatched
    Waiting --> Waiting: Prompt queued (FIFO)
    Waiting --> Ready: RunFinished received & queue drained
    Waiting --> Ready: CancelTurn dispatched
    Ready --> Gone: Transport::Disconnected / Fatal error
    Waiting --> Gone: Transport::Disconnected / Fatal error
```

### Lane States

* **`Connecting`**: Adapter subprocess is initializing and negotiating protocol handshake.
* **`Ready`**: Lane is idle and ready to ingest user prompts.
* **`Waiting`**: An agent or model turn is actively generating tokens or running tools.
* **`Gone`**: The adapter has died, been terminated, or suffered an unrecoverable crash.

---

## FIFO Prompt Queueing

When the user submits a prompt while a lane is in `Waiting` state, Kobold avoids blocking the user:

1. **Enqueue**: The kernel appends the incoming prompt text to `lane.queue`.
2. **Drain**: When the active turn ends (marked by `RunFinished` or an unhandled error), the kernel invokes `drain_lane_queue(&lane, commands)`.
3. **Sequential Execution**: If `lane.queue` has pending items, the oldest prompt is popped and dispatched immediately via `Command::Send`.

This allows users to stack multiple tasks or train-of-thought messages while the agent is executing.

---

## Mid-Turn Steering & Cancellation

If an agent starts going down an unwanted path:

1. **Northbound Trigger**:
   * In TUI: User double-taps `Esc` (`Esc` `Esc`).
   * In Web Companion: User clicks **Stop**.
2. **Kernel Processing**:
   * Kernel sets `lane.interrupted = true` and settles the pending request count.
   * Dispatches `Command::Cancel { lane }` to the adapter subprocess.
   * Broadcasts `StatusChange { status: Ready }` to all connected frontends.
3. **Adapter Propagation**:
   * `kobold-adapter-acp` transmits `session/cancel` JSON-RPC notification to the agent.
   * `kobold-adapter-tmux` transmits `SIGINT` (`\x03`) to the PTY.
4. **Steering Ingestion**:
   * The lane is immediately ready for a new prompt.
   * When the user submits a corrective prompt, `sent_request()` clears `lane.interrupted = false` and dispatches the new instruction.
