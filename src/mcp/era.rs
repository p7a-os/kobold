//! Deciding what era a server speaks -- pulled out from `client.rs` as a
//! pure function because the branch it makes (fall back to `initialize`, or
//! trust that `server/discover` answered) is the one piece of era detection
//! that has nothing to do with a transport and everything to do with getting
//! the classification right.
//!
//! Per spec: era is a property of the SERVER, not of a single request, and a
//! client detects it once and caches the result for the connection's
//! lifetime rather than re-probing every call.

use crate::mcp::jsonrpc::RpcError;
use crate::mcp::version::{Revision, UNSUPPORTED_PROTOCOL_VERSION};

/// What a `server/discover` attempt implies about the server's era, given
/// how it responded.
///
/// A successful response is unambiguous and does not go through this
/// function at all -- only the error path needs classifying, because an
/// error can mean two different things: "I am a modern server and rejected
/// your version" (still modern; `UnsupportedProtocolVersionError`) or "I
/// have never heard of `server/discover`" (a legacy server; anything else,
/// most commonly Method Not Found).
pub fn classify_discover_error(err: &RpcError) -> Revision {
    if err.code == UNSUPPORTED_PROTOCOL_VERSION {
        Revision::Modern
    } else {
        Revision::Legacy
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unsupported_version_error_is_still_a_modern_server() {
        let err = RpcError {
            code: UNSUPPORTED_PROTOCOL_VERSION,
            message: "x".into(),
            data: None,
        };
        assert_eq!(classify_discover_error(&err), Revision::Modern);
    }

    #[test]
    fn a_method_not_found_error_means_the_server_predates_server_discover() {
        let err = RpcError {
            code: -32601,
            message: "method not found".into(),
            data: None,
        };
        assert_eq!(classify_discover_error(&err), Revision::Legacy);
    }

    #[test]
    fn every_other_error_code_also_falls_back_to_legacy_not_just_dash_32601() {
        // The classifier is "is this the one recognized modern error", not
        // "is this specifically method-not-found" -- any other code from a
        // server that does not understand server/discover is just as
        // possible (a generic internal error, a parse error from a server
        // that mishandled the request shape), and all of them should fall
        // back the same way.
        let err = RpcError {
            code: -32603,
            message: "internal error".into(),
            data: None,
        };
        assert_eq!(classify_discover_error(&err), Revision::Legacy);
    }
}
