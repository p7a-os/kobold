//! `kobold-adapter-tmux` library module.
//!
//! Exposes ANSI sequence processing, prompt detection, PTY execution,
//! and the Southbound meta-harness bridge.

pub mod ansi;
pub mod bridge;
pub mod prompts;
pub mod pty;

pub use bridge::TmuxBridge;
pub use pty::PtySession;
