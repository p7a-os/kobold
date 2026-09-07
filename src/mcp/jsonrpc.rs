//! Bare JSON-RPC 2.0 envelope, shared by every MCP transport.
//!
//! MCP layers its own semantics (methods, `_meta`, error codes) on top of
//! plain JSON-RPC. This file only knows about the envelope -- id matching,
//! request/response/notification shape -- so the same types serve stdio and
//! Streamable HTTP without duplicating the framing rules twice.

use serde::{Deserialize, Serialize};
use sonic_rs::JsonValueTrait;

/// A request id is a string, a number, or absent (making the message a
/// notification). The spec forbids `null` as an id on a request that expects
/// a reply, but a response may echo whatever the request sent, so this stays
/// permissive on read and only produces `Number`/`String` from our own side.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Id {
    Number(i64),
    String(String),
}

impl From<i64> for Id {
    fn from(n: i64) -> Self {
        Id::Number(n)
    }
}

impl std::fmt::Display for Id {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Id::Number(n) => write!(f, "{n}"),
            Id::String(s) => write!(f, "{s}"),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Request<P> {
    pub jsonrpc: JsonRpcVersion,
    pub id: Id,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<P>,
}

impl<P> Request<P> {
    pub fn new(id: Id, method: impl Into<String>, params: Option<P>) -> Self {
        Self {
            jsonrpc: JsonRpcVersion,
            id,
            method: method.into(),
            params,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Notification<P> {
    pub jsonrpc: JsonRpcVersion,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<P>,
}

impl<P> Notification<P> {
    pub fn new(method: impl Into<String>, params: Option<P>) -> Self {
        Self {
            jsonrpc: JsonRpcVersion,
            method: method.into(),
            params,
        }
    }
}

/// Always the literal `"2.0"`. A unit struct rather than a `String` field so a
/// message from an older or malformed peer that sends anything else fails to
/// parse at the type boundary instead of being silently accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JsonRpcVersion;

impl Serialize for JsonRpcVersion {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str("2.0")
    }
}

impl<'de> Deserialize<'de> for JsonRpcVersion {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        if s == "2.0" {
            Ok(JsonRpcVersion)
        } else {
            Err(serde::de::Error::custom(format!(
                "unsupported jsonrpc version {s:?}"
            )))
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default)]
    pub data: Option<sonic_rs::Value>,
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}

impl std::error::Error for RpcError {}

/// An inbound message, before we know which of the three shapes it is.
///
/// Discriminated by hand rather than `#[serde(untagged)]`: verified here that
/// sonic-rs's untagged support does not fall through to a later variant when
/// an earlier variant's optional fields are simply absent, so a bare request
/// (`id`+`method`, no `result`/`error`) failed to parse as anything at all.
/// Deserializing into a `Value` first and branching on which keys are present
/// sidesteps that entirely.
#[derive(Debug, Clone)]
pub enum Incoming {
    Response {
        id: Id,
        result: Option<sonic_rs::Value>,
        error: Option<RpcError>,
    },
    Request {
        id: Id,
        method: String,
        params: Option<sonic_rs::Value>,
    },
    Notification {
        method: String,
        params: Option<sonic_rs::Value>,
    },
}

impl<'de> Deserialize<'de> for Incoming {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;

        let v = sonic_rs::Value::deserialize(d)?;
        let has = |k: &str| v.get(k).is_some();

        if has("method") {
            let method = v
                .get("method")
                .and_then(|m| m.as_str())
                .ok_or_else(|| D::Error::custom("method field is not a string"))?
                .to_owned();
            let params = v.get("params").cloned();
            return Ok(if has("id") {
                let id = sonic_rs::from_value(&v.get("id").unwrap().clone())
                    .map_err(|e| D::Error::custom(format!("id: {e}")))?;
                Incoming::Request { id, method, params }
            } else {
                Incoming::Notification { method, params }
            });
        }

        if has("id") {
            let id = sonic_rs::from_value(&v.get("id").unwrap().clone())
                .map_err(|e| D::Error::custom(format!("id: {e}")))?;
            let result = v.get("result").cloned();
            // Built by hand, not `sonic_rs::from_value`: `data` is itself a
            // `sonic_rs::Value`, and a struct with a nested `Value` field hits
            // the same zero-copy landmine noted on `Incoming` above --
            // reproduced here with a real `-32022` payload carrying a
            // `data.supported` array, where `from_value` failed with `invalid
            // type: newtype struct, expected a valid json`.
            let error = match v.get("error") {
                Some(e) => {
                    let code = e.get("code").and_then(|c| c.as_i64()).ok_or_else(|| {
                        D::Error::custom("error.code is missing or not an integer")
                    })?;
                    let message = e
                        .get("message")
                        .and_then(|m| m.as_str())
                        .ok_or_else(|| {
                            D::Error::custom("error.message is missing or not a string")
                        })?
                        .to_owned();
                    let data = e.get("data").cloned();
                    Some(RpcError {
                        code,
                        message,
                        data,
                    })
                }
                None => None,
            };
            return Ok(Incoming::Response { id, result, error });
        }

        Err(D::Error::custom("message has neither `method` nor `id`"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_serializes_without_a_params_field_when_there_are_no_params() {
        let req = Request::<()>::new(Id::Number(1), "ping", None);
        let s = crate::json::to_string(&req).unwrap();
        assert!(!s.contains("params"), "{s}");
    }

    #[test]
    fn a_numeric_id_round_trips_through_json_as_a_number_not_a_string() {
        let req = Request::<()>::new(1.into(), "ping", None);
        let s = crate::json::to_string(&req).unwrap();
        assert!(s.contains("\"id\":1"), "{s}");
    }

    #[test]
    fn a_response_with_an_id_and_a_result_parses_as_the_response_variant() {
        let raw = br#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#;
        let msg: Incoming = crate::json::from_slice(raw).unwrap();
        assert!(matches!(
            msg,
            Incoming::Response {
                id: Id::Number(1),
                result: Some(_),
                error: None
            }
        ));
    }

    #[test]
    fn an_error_response_carries_the_code_and_message_through() {
        let raw = br#"{"jsonrpc":"2.0","id":"x","error":{"code":-32601,"message":"nope"}}"#;
        let msg: Incoming = crate::json::from_slice(raw).unwrap();
        match msg {
            Incoming::Response { error: Some(e), .. } => {
                assert_eq!(e.code, -32601);
                assert_eq!(e.message, "nope");
            }
            other => panic!("expected an error response, got {other:?}"),
        }
    }

    #[test]
    fn a_server_initiated_request_parses_as_the_request_variant_not_a_notification() {
        let raw = br#"{"jsonrpc":"2.0","id":7,"method":"roots/list"}"#;
        let msg: Incoming = crate::json::from_slice(raw).unwrap();
        match msg {
            Incoming::Request {
                id: Id::Number(7),
                method,
                ..
            } => assert_eq!(method, "roots/list"),
            other => panic!("expected a numbered request for roots/list, got {other:?}"),
        }
    }

    #[test]
    fn a_message_with_no_id_parses_as_a_notification() {
        let raw = br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        let msg: Incoming = crate::json::from_slice(raw).unwrap();
        assert!(matches!(msg, Incoming::Notification { .. }));
    }

    #[test]
    fn a_non_2_0_jsonrpc_version_is_rejected_rather_than_silently_accepted() {
        let raw = br#""1.0""#;
        let res: Result<JsonRpcVersion, _> = crate::json::from_slice(raw);
        assert!(res.is_err());
    }

    #[test]
    fn a_numeric_id_displays_as_the_bare_number() {
        assert_eq!(Id::Number(42).to_string(), "42");
    }

    #[test]
    fn a_string_id_displays_as_the_bare_string_not_quoted() {
        assert_eq!(Id::String("abc".to_string()).to_string(), "abc");
    }

    #[test]
    fn an_rpc_error_displays_the_message_and_the_code_together() {
        let e = RpcError {
            code: -32601,
            message: "method not found".to_string(),
            data: None,
        };
        assert_eq!(e.to_string(), "method not found (-32601)");
    }
}
