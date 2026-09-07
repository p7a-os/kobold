//! Headless execution kernel and meta-harness for Kobold.
//!
//! Exposes pure turn orchestration, multi-lane management, sandbox confinement,
//! egress network brokering, local & MCP tool execution, and append-only transcript trees.

pub mod adapters;
pub mod broker;
pub mod childenv;
pub mod daemon;
pub mod json;
pub mod kernel;
pub mod lane;
pub mod sandbox;
pub mod session;
pub mod tools;
pub mod transcript;
pub mod worktree;
pub mod ws;

pub use adapters::{AdapterError, AdapterKind, SouthboundAdapter};
pub use daemon::{default_socket_path, Daemon, DaemonClient, DaemonConfig};
pub use kernel::{Kernel, Silence, SilenceWatch, FIRST_UPDATE_NOTICE, STREAM_STALL};
pub use kobold_proto::northbound::LaneStatus;
pub use lane::{Lane, LaneEffect, Link, PendingCall, TranscriptEntry, Who};
pub use session::{session_runtime_dir, SessionMetadata, SessionRegistry};
pub use worktree::{
    create_git_worktree, find_git_root, generate_worktree_name, is_git_repo, parse_worktree_name,
};
pub use ws::{
    auth_file_path, generate_token, save_auth_token, validate_token, DEFAULT_WS_PORT,
    WEB_COMPANION_HTML,
};
