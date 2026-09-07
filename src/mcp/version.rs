//! Protocol version negotiation, spanning the 2025-11-25 and 2026-07-28
//! revisions -- which negotiate in genuinely different ways, not just
//! different version strings.
//!
//! 2025-11-25 and earlier: a stateful handshake. The client's `initialize`
//! request carries the version it wants; the server either echoes it back or
//! offers a different one it supports; the client accepts or disconnects.
//! Negotiated once, for the life of the connection.
//!
//! 2026-07-28: no handshake-level negotiation at all. Every request declares
//! its version itself (in `_meta` and, on HTTP, the `MCP-Protocol-Version`
//! header), and the server accepts or rejects each request independently
//! with `UnsupportedProtocolVersionError`. `server/discover` exists so a
//! client can learn what a server supports up front instead of guessing.

use serde::{Deserialize, Serialize};

pub const V2025_11_25: &str = "2025-11-25";
pub const V2026_07_28: &str = "2026-07-28";

/// Versions this client speaks, newest first -- offered in that order because
/// both revisions ask a client to request the latest version it supports.
pub const SUPPORTED: &[&str] = &[V2026_07_28, V2025_11_25];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Revision {
    /// Handshake-negotiated: `initialize` / `initialized`, session id on HTTP.
    Legacy,
    /// Per-request version, `server/discover`, MRTR instead of server-initiated
    /// requests.
    Modern,
}

pub fn revision_of(version: &str) -> Option<Revision> {
    match version {
        V2025_11_25 => Some(Revision::Legacy),
        V2026_07_28 => Some(Revision::Modern),
        _ => None,
    }
}

/// JSON-RPC error code for `UnsupportedProtocolVersionError` (2026-07-28+).
pub const UNSUPPORTED_PROTOCOL_VERSION: i64 = -32022;

#[derive(Debug, Clone, Deserialize)]
pub struct UnsupportedVersionData {
    pub supported: Vec<String>,
    pub requested: String,
}

/// What a client sends to identify itself, in either revision's shape.
#[derive(Debug, Clone, Serialize)]
pub struct ClientInfo {
    pub name: String,
    pub version: String,
}

/// Pick the version to request first: our newest, unless a prior
/// `server/discover` (or a previous negotiation) narrowed it down to the
/// newest one both sides actually share.
pub fn preferred(server_supports: Option<&[String]>) -> &'static str {
    match server_supports {
        Some(theirs) => SUPPORTED
            .iter()
            .find(|ours| theirs.iter().any(|v| v == *ours))
            .copied()
            .unwrap_or(SUPPORTED[0]),
        None => SUPPORTED[0],
    }
}

/// Given the error a 2026-07-28+ server sent back for a request, what version
/// the retry should use -- if this error is the retryable kind at all.
///
/// Per spec, on `UnsupportedProtocolVersionError` the client SHOULD pick a
/// mutually supported version from the error's `supported` list and retry
/// the same request with it. Any other error code means the request was
/// rejected for its own reasons, not a version mismatch, so retrying with a
/// different version would not help -- this returns `None` rather than
/// guessing.
pub fn retry_version(err: &crate::mcp::jsonrpc::RpcError) -> Option<&'static str> {
    if err.code != UNSUPPORTED_PROTOCOL_VERSION {
        return None;
    }
    let data = err.data.as_ref()?;
    let json = crate::json::to_string(data).ok()?;
    let parsed: UnsupportedVersionData = sonic_rs::from_str(&json).ok()?;
    Some(preferred(Some(&parsed.supported)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_newest_supported_version_is_offered_first() {
        assert_eq!(SUPPORTED[0], V2026_07_28);
    }

    #[test]
    fn each_supported_version_string_maps_to_a_revision() {
        for v in SUPPORTED {
            assert!(revision_of(v).is_some(), "{v} has no revision mapping");
        }
    }

    #[test]
    fn an_unknown_version_string_maps_to_no_revision() {
        assert_eq!(revision_of("1999-01-01"), None);
    }

    #[test]
    fn with_no_server_hint_the_client_prefers_its_own_newest_version() {
        assert_eq!(preferred(None), V2026_07_28);
    }

    #[test]
    fn when_the_server_only_supports_the_legacy_revision_that_is_preferred_instead() {
        let theirs = vec![V2025_11_25.to_string()];
        assert_eq!(preferred(Some(&theirs)), V2025_11_25);
    }

    #[test]
    fn when_the_server_supports_neither_known_version_we_still_offer_our_newest() {
        let theirs = vec!["2024-01-01".to_string()];
        assert_eq!(preferred(Some(&theirs)), V2026_07_28);
    }

    #[test]
    fn the_unsupported_protocol_version_code_matches_the_spec_exactly() {
        assert_eq!(UNSUPPORTED_PROTOCOL_VERSION, -32022);
    }

    #[test]
    fn an_unsupported_version_error_yields_a_mutually_supported_retry_version() {
        let data: sonic_rs::Value = sonic_rs::from_str(
            r#"{"supported":["2025-11-25","2024-01-01"],"requested":"1900-01-01"}"#,
        )
        .unwrap();
        let err = crate::mcp::jsonrpc::RpcError {
            code: UNSUPPORTED_PROTOCOL_VERSION,
            message: "unsupported".to_owned(),
            data: Some(data),
        };
        assert_eq!(retry_version(&err), Some(V2025_11_25));
    }

    #[test]
    fn an_unrelated_error_code_yields_no_retry_version() {
        let err = crate::mcp::jsonrpc::RpcError {
            code: -32601,
            message: "method not found".to_owned(),
            data: None,
        };
        assert_eq!(retry_version(&err), None);
    }

    #[test]
    fn an_unsupported_version_error_with_no_data_yields_no_retry_version() {
        let err = crate::mcp::jsonrpc::RpcError {
            code: UNSUPPORTED_PROTOCOL_VERSION,
            message: "unsupported".to_owned(),
            data: None,
        };
        assert_eq!(retry_version(&err), None);
    }
}
