//! Southbound ACP (Agent Client Protocol) adapter library for Kobold.
//!
//! Supervises external autonomous agent CLIs speaking ACP over JSON-RPC 2.0 stdio,
//! translating their progress and tool invocations into standard AG-UI frames
//! and routing permission prompts through Kobold's `Ask` panel subsystem.

pub mod agent;
pub mod bridge;
pub mod proto;

pub use agent::AgentProcess;
pub use bridge::AcpBridge;
pub use proto::{JsonRpcMessage, SessionUpdate};
