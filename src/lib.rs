//! Shared library behind the `kobold` and `probe` binaries.
//!
//! These were previously pulled into each binary with `#[path]`, which meant
//! every module was compiled twice and each binary reported the other's code
//! as dead. One library, two thin binaries.

pub mod adapter;
pub mod app;
pub mod auth;
pub mod complete;
pub mod doctor;
pub mod edit;
pub mod fixture;
pub mod installer;
pub mod layout;
pub mod mcp;
pub mod md;
pub mod net;
pub mod osc;
pub mod progress;
pub mod settings;
pub mod syntax;
pub mod term;
pub mod tmux;
pub mod tts;
pub mod wizard;

pub use kobold_core::{
    broker, childenv, daemon, json, kernel, lane, sandbox, session, tools, transcript, worktree, ws,
};
