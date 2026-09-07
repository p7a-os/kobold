# The Dual-Seam Model

Kobold is built on the **Dual-Seam Architecture**, an interface pattern that decouples the user-facing presentation layer (Northbound) from the agent/provider execution layer (Southbound).

---

## 1. The Northbound Seam (Client $\leftrightarrow$ Kernel)

The Northbound seam standardizes communication between user interfaces (the terminal UI, web companions, IDE integrations) and the headless supervisor kernel.

### Transport
* **Local IPC**: Unix Domain Socket at `/tmp/kobold-{uid}/{session_id}.sock` using line-delimited JSON.
* **Remote / Web**: WebSockets at `ws://127.0.0.1:3000/ws` with Bearer token authentication.

### Message Types

#### Client $\to$ Kernel: `ClientFrame`
```rust
pub enum ClientFrame {
    /// Submit a prompt to a specific execution lane.
    Prompt { lane: String, text: String },
    
    /// Submit answers to an interactive tool permission or interrupt prompt.
    SubmitInterrupt { lane: String, call_id: String, answers: Vec<String> },
    
    /// Decline or cancel an interactive interrupt.
    CancelInterrupt { lane: String, call_id: String },
    
    /// Fork a new execution lane from a historical message index on an existing branch.
    Fork { new_lane: String, parent_branch: String, parent_at: usize },
    
    /// Cancel an in-flight turn on the specified lane.
    CancelTurn { lane: String },
    
    /// Request a full state hydration snapshot.
    SyncRequest { lane: String },
    
    /// Notify the daemon of frontend disconnection without stopping execution.
    Detach,
}
```

#### Kernel $\to$ Client: `ServerFrame`
```rust
pub enum ServerFrame {
    /// Multiplexed AG-UI event for a lane (text delta, tool start, run finished).
    Event { lane: String, event: agui::Outgoing },
    
    /// Status update for a lane (Ready, Waiting, Gone).
    StatusChange { lane: String, status: LaneStatus },
    
    /// Full hydration snapshot sent upon connection or fork.
    Snapshot {
        lane: String,
        branch: String,
        messages: Vec<TranscriptRecord>,
        active_interrupt: Option<AskRecord>,
        status: LaneStatus,
    },
    
    /// System notices and informational warnings.
    Notice { text: String },
    
    /// Terminal error notification.
    Error { message: String },
}
```

---

## 2. The Southbound Seam (Kernel $\leftrightarrow$ Adapter)

The Southbound seam standardizes how Kobold controls external agents, PTYs, and model providers. Adapters run as isolated subprocesses confined by local sandboxing.

### Transport
* **Standard I/O**: Anonymous stdin/stdout pipes passing one-line JSON frames.
* **Startup Handshake**: A single, mandatory `Startup` frame arrives on stdin before any command.

### Message Types

#### Kernel $\to$ Adapter: `Command`
```rust
pub enum Command {
    /// Instructs the adapter to run a prompt on a lane.
    Send {
        lane: String,
        text: String,
        previous_response_id: Option<String>,
        replay: Vec<(String, String)>,
    },
    
    /// Produces a tool result on its way back to the model or agent.
    ToolResult {
        lane: String,
        call_id: String,
        output: String,
        error: bool,
    },
    
    /// Cancels the currently executing prompt/turn on the lane.
    Cancel {
        lane: String,
    },
    
    /// Graceful termination request.
    Quit,
}
```

#### Adapter $\to$ Kernel: `IncomingFrame`
```rust
pub enum IncomingFrame {
    /// Streaming event for a lane conforming to AG-UI wire specification.
    Event { lane: String, event: agui::Incoming },
    
    /// Transport lifecycle changes (Connected, Disconnected).
    Transport(Transport),
}
```
