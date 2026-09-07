//! Agent Client Protocol (ACP) JSON-RPC 2.0 wire types and codecs.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Standard JSON-RPC 2.0 error object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// A generic JSON-RPC 2.0 message crossing the stdio pipe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcMessage {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl JsonRpcMessage {
    /// Constructs a JSON-RPC 2.0 request.
    pub fn request(id: impl Into<Value>, method: impl Into<String>, params: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id: Some(id.into()),
            method: Some(method.into()),
            params: Some(params),
            result: None,
            error: None,
        }
    }

    /// Constructs a JSON-RPC 2.0 notification.
    pub fn notification(method: impl Into<String>, params: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id: None,
            method: Some(method.into()),
            params: Some(params),
            result: None,
            error: None,
        }
    }

    /// Constructs a JSON-RPC 2.0 success response.
    pub fn response(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id: Some(id),
            method: None,
            params: None,
            result: Some(result),
            error: None,
        }
    }

    /// Constructs a JSON-RPC 2.0 error response.
    pub fn error_response(id: Value, code: i64, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id: Some(id),
            method: None,
            params: None,
            result: None,
            error: Some(RpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }

    /// Whether this message is a request (has both id and method).
    pub fn is_request(&self) -> bool {
        self.id.is_some() && self.method.is_some()
    }

    /// Whether this message is a notification (has method but no id).
    pub fn is_notification(&self) -> bool {
        self.id.is_none() && self.method.is_some()
    }

    /// Whether this message is a response (has id and either result or error).
    pub fn is_response(&self) -> bool {
        self.id.is_some() && (self.result.is_some() || self.error.is_some())
    }
}

/// Client info sent during ACP `initialize`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientInfo {
    pub name: String,
    pub version: String,
}

/// Params for `initialize` request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub protocol_version: u32,
    pub client_info: ClientInfo,
    #[serde(default)]
    pub capabilities: Value,
}

/// Prompt item within `session/prompt`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PromptItem {
    Text { text: String },
}

/// Params for `session/prompt` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionPromptParams {
    pub session_id: String,
    pub prompt: Vec<PromptItem>,
}

/// Updates received via `session/update` notifications.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionUpdate {
    #[serde(rename = "text")]
    Text { delta: String },
    #[serde(rename = "thought")]
    Thought { delta: String },
    #[serde(rename = "tool_call")]
    ToolCall {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        status: Option<String>,
    },
    #[serde(rename = "tool_call_chunk")]
    ToolCallChunk {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        delta: String,
    },
    #[serde(other)]
    Unknown,
}

/// Params for `session/update` notification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionUpdateParams {
    pub session_id: String,
    pub update: SessionUpdate,
}

/// Description of permission request received via `session/request_permission`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionDetails {
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub options: Vec<String>,
}

/// Params for `session/request_permission` request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestPermissionParams {
    pub session_id: String,
    pub permission: PermissionDetails,
}

/// Result returned in response to `session/request_permission`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionDecision {
    pub decision: String, // "allow" | "deny"
}

/// Params for `session/execute_tool` request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecuteToolParams {
    pub session_id: String,
    pub tool_name: String,
    #[serde(default)]
    pub arguments: Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonrpc_request_and_response_roundtrip() {
        let req =
            JsonRpcMessage::request(1, "initialize", serde_json::json!({"protocolVersion": 1}));
        let encoded = sonic_rs::to_string(&req).unwrap();
        assert!(encoded.contains(r#""method":"initialize""#));

        let decoded: JsonRpcMessage = sonic_rs::from_str(&encoded).unwrap();
        assert!(decoded.is_request());
        assert_eq!(decoded.id, Some(Value::from(1)));

        let resp = JsonRpcMessage::response(Value::from(1), serde_json::json!({"status": "ok"}));
        assert!(resp.is_response());
    }

    #[test]
    fn session_update_parsing() {
        let text_json = r#"{"sessionId":"s1","update":{"type":"text","delta":"hello world"}}"#;
        let p: SessionUpdateParams = sonic_rs::from_str(text_json).unwrap();
        assert_eq!(p.session_id, "s1");
        assert_eq!(
            p.update,
            SessionUpdate::Text {
                delta: "hello world".into()
            }
        );

        let thought_json = r#"{"sessionId":"s1","update":{"type":"thought","delta":"hmm"}}"#;
        let p2: SessionUpdateParams = sonic_rs::from_str(thought_json).unwrap();
        assert_eq!(
            p2.update,
            SessionUpdate::Thought {
                delta: "hmm".into()
            }
        );
    }

    #[test]
    fn request_permission_parsing() {
        let perm_json = r#"{
            "sessionId": "s1",
            "permission": {
                "type": "tool",
                "toolName": "bash",
                "command": "cargo build",
                "options": ["allow", "deny"]
            }
        }"#;
        let p: RequestPermissionParams = sonic_rs::from_str(perm_json).unwrap();
        assert_eq!(p.permission.tool_name.as_deref(), Some("bash"));
        assert_eq!(p.permission.command.as_deref(), Some("cargo build"));
        assert_eq!(p.permission.options, vec!["allow", "deny"]);
    }
}
