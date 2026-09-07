//! Southbound Adapter architecture for Kobold.
//!
//! Provides the abstraction layer between the Kobold Kernel and diverse LLM backends:
//! - Direct Model APIs (OpenAI Responses WebSocket, Anthropic Messages, Gemini Live)
//! - Agent CLIs via ACP (Agent Client Protocol over JSON-RPC)
//! - Agent CLIs via Headless JSON (programmatic CLI stdout parsing)
//! - Agent CLIs via PTY / tmux (terminal automation for interactive coding agents)

use async_trait::async_trait;
use kobold_proto::{Command, IncomingFrame};

/// Category of Southbound backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterKind {
    /// Direct model provider (OpenAI, Anthropic, Gemini, Grok direct).
    DirectModel,
    /// Autonomous agent CLI supporting Agent Client Protocol (ACP) over stdio.
    AgentAcp,
    /// Autonomous agent CLI executing in headless programmatic JSON mode.
    AgentCliJson,
    /// Autonomous agent CLI driven inside a pseudo-terminal / tmux session.
    AgentPtyTmux,
}

/// Errors produced during Southbound adapter communication.
#[derive(Debug)]
pub enum AdapterError {
    Spawn(String),
    Io(std::io::Error),
    Protocol(String),
    Terminated,
}

impl std::fmt::Display for AdapterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(s) => write!(f, "adapter spawn error: {s}"),
            Self::Io(e) => write!(f, "adapter io error: {e}"),
            Self::Protocol(s) => write!(f, "adapter protocol violation: {s}"),
            Self::Terminated => write!(f, "adapter process terminated"),
        }
    }
}

impl std::error::Error for AdapterError {}

impl From<std::io::Error> for AdapterError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

/// Unified asynchronous interface for Southbound LLM and Agent adapters.
#[async_trait]
pub trait SouthboundAdapter: Send + Sync {
    /// Returns the architectural kind of this adapter.
    fn kind(&self) -> AdapterKind;

    /// Dispatches a command from the kernel to the adapter.
    async fn send(&self, command: Command) -> Result<(), AdapterError>;

    /// Receives the next frame from the adapter. Returns `None` when stream closes.
    async fn recv(&mut self) -> Result<Option<IncomingFrame>, AdapterError>;

    /// Signals the adapter to terminate and releases resources.
    async fn shutdown(&mut self) -> Result<(), AdapterError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    struct MockAdapter {
        kind: AdapterKind,
        tx: mpsc::UnboundedSender<Command>,
        rx: mpsc::Receiver<IncomingFrame>,
    }

    #[async_trait]
    impl SouthboundAdapter for MockAdapter {
        fn kind(&self) -> AdapterKind {
            self.kind
        }

        async fn send(&self, command: Command) -> Result<(), AdapterError> {
            self.tx.send(command).map_err(|_| AdapterError::Terminated)
        }

        async fn recv(&mut self) -> Result<Option<IncomingFrame>, AdapterError> {
            Ok(self.rx.recv().await)
        }

        async fn shutdown(&mut self) -> Result<(), AdapterError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_southbound_adapter_trait() {
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
        let (frame_tx, frame_rx) = mpsc::channel(16);

        let mut adapter: Box<dyn SouthboundAdapter> = Box::new(MockAdapter {
            kind: AdapterKind::AgentAcp,
            tx: cmd_tx,
            rx: frame_rx,
        });

        assert_eq!(adapter.kind(), AdapterKind::AgentAcp);

        adapter
            .send(Command::Send {
                lane: "main".into(),
                text: "test".into(),
                previous_response_id: None,
                replay: Vec::new(),
            })
            .await
            .unwrap();

        let received_cmd = cmd_rx.recv().await.unwrap();
        assert!(matches!(received_cmd, Command::Send { text, .. } if text == "test"));

        frame_tx
            .send(IncomingFrame::Transport(kobold_proto::Transport::Connected))
            .await
            .unwrap();

        let received_frame = adapter.recv().await.unwrap().unwrap();
        assert!(matches!(received_frame, IncomingFrame::Transport(..)));

        adapter.shutdown().await.unwrap();
    }
}
