//! Contract tests for Agent Client Protocol (ACP) JSON-RPC 2.0 schemas and wire formats.

use kobold_adapter_acp::proto::{
    InitializeParams, JsonRpcMessage, PermissionDecision, RequestPermissionParams,
    SessionPromptParams, SessionUpdate, SessionUpdateParams,
};

#[test]
fn test_acp_initialize_schema_contract() {
    let raw = r#"{
        "protocolVersion": 1,
        "clientInfo": {
            "name": "kobold",
            "version": "0.1.0"
        },
        "capabilities": {
            "prompts": {},
            "tools": {}
        }
    }"#;

    let init: InitializeParams = sonic_rs::from_str(raw).expect("valid initialize params");
    assert_eq!(init.protocol_version, 1);
    assert_eq!(init.client_info.name, "kobold");
    assert_eq!(init.client_info.version, "0.1.0");

    let req = JsonRpcMessage::request(1, "initialize", serde_json::to_value(init).unwrap());
    let serialized = sonic_rs::to_string(&req).unwrap();
    assert!(serialized.contains(r#""jsonrpc":"2.0""#));
    assert!(serialized.contains(r#""method":"initialize""#));
}

#[test]
fn test_acp_session_prompt_schema_contract() {
    let raw = r#"{
        "sessionId": "01918a22-3837-7756-9a2c-f6889b70b551",
        "prompt": [
            { "type": "text", "text": "Run tests and summarize results" }
        ]
    }"#;

    let prompt: SessionPromptParams = sonic_rs::from_str(raw).expect("valid session prompt");
    assert_eq!(prompt.session_id, "01918a22-3837-7756-9a2c-f6889b70b551");
    assert_eq!(prompt.prompt.len(), 1);
}

#[test]
fn test_acp_session_update_schema_contract() {
    // 1. Text update
    let text_raw = r#"{"sessionId":"s1","update":{"type":"text","delta":"Building project..."}}"#;
    let u1: SessionUpdateParams = sonic_rs::from_str(text_raw).unwrap();
    assert_eq!(
        u1.update,
        SessionUpdate::Text {
            delta: "Building project...".into()
        }
    );

    // 2. Thought update
    let thought_raw =
        r#"{"sessionId":"s1","update":{"type":"thought","delta":"Analyzing dependencies"}}"#;
    let u2: SessionUpdateParams = sonic_rs::from_str(thought_raw).unwrap();
    assert_eq!(
        u2.update,
        SessionUpdate::Thought {
            delta: "Analyzing dependencies".into()
        }
    );

    // 3. Tool call start
    let tc_raw = r#"{"sessionId":"s1","update":{"type":"tool_call","toolCallId":"call-123","name":"cargo","status":"in_progress"}}"#;
    let u3: SessionUpdateParams = sonic_rs::from_str(tc_raw).unwrap();
    assert_eq!(
        u3.update,
        SessionUpdate::ToolCall {
            tool_call_id: "call-123".into(),
            name: Some("cargo".into()),
            status: Some("in_progress".into()),
        }
    );

    // 4. Tool call chunk
    let chunk_raw = r#"{"sessionId":"s1","update":{"type":"tool_call_chunk","toolCallId":"call-123","delta":"test --lib"}}"#;
    let u4: SessionUpdateParams = sonic_rs::from_str(chunk_raw).unwrap();
    assert_eq!(
        u4.update,
        SessionUpdate::ToolCallChunk {
            tool_call_id: "call-123".into(),
            delta: "test --lib".into(),
        }
    );
}

#[test]
fn test_acp_request_permission_contract() {
    let raw = r#"{
        "sessionId": "s1",
        "permission": {
            "type": "bash",
            "toolName": "bash",
            "command": "git push origin main",
            "options": ["allow", "deny"]
        }
    }"#;

    let req: RequestPermissionParams = sonic_rs::from_str(raw).unwrap();
    assert_eq!(req.permission.tool_name.as_deref(), Some("bash"));
    assert_eq!(
        req.permission.command.as_deref(),
        Some("git push origin main")
    );
    assert_eq!(req.permission.options, vec!["allow", "deny"]);

    // Decision response contract
    let decision = PermissionDecision {
        decision: "allow".into(),
    };
    let serialized = sonic_rs::to_string(&decision).unwrap();
    assert_eq!(serialized, r#"{"decision":"allow"}"#);
}
