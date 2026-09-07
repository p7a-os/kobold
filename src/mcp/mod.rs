//! MCP client: connects kobold to Model Context Protocol servers so their
//! tools, and eventually resources and prompts, surface to the model
//! alongside kobold's own local tools.
//!
//! Split by concern rather than by spec revision, because most of the client
//! (framing, transports) is shared -- only [`version`] and the request/result
//! shapes for initialization and elicitation actually differ between
//! 2025-11-25 and 2026-07-28.
//!
//! - [`jsonrpc`]: the bare JSON-RPC 2.0 envelope both transports carry.
//! - [`stdio`]: child-process transport, newline-delimited JSON.
//! - [`version`]: protocol version negotiation across both spec revisions.
//!
//! HTTP transport and the elicitation flow land in following commits; this
//! module is being built up incrementally rather than landed whole.

pub mod client;
pub mod elicit;
pub mod era;
pub mod http;
pub mod jsonrpc;
pub mod stdio;
pub mod version;
