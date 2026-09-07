//! Integration tests for AcpBridge lifecycle, event streaming, and permission interrupts.

use kobold_adapter_acp::proto::JsonRpcMessage;
use kobold_adapter_acp::AcpBridge;
use kobold_proto::agui;
use kobold_proto::{Command, OutgoingFrame, Transport};
use serde_json::Value;

#[test]
fn test_acp_bridge_full_lifecycle_and_turn() {
    let mut bridge = AcpBridge::new("session-test-1");

    // 1. Handshake
    let init_req = bridge.create_initialize_request();
    assert_eq!(init_req.method.as_deref(), Some("initialize"));

    let init_resp = JsonRpcMessage::response(
        Value::from(1),
        serde_json::json!({
            "protocolVersion": 1,
            "agentInfo": { "name": "claude-code", "version": "0.70.0" },
            "capabilities": {}
        }),
    );
    let (_, frames) = bridge.handle_agent_message(init_resp);
    assert_eq!(frames, vec![OutgoingFrame::Transport(Transport::Connected)]);

    // 2. User prompt turn
    let (rpc_opt, frames) = bridge.handle_kobold_command(Command::Send {
        lane: "main".into(),
        text: "Fix the bug in main.rs".into(),
        previous_response_id: None,
        replay: Vec::new(),
    });

    let prompt_rpc = rpc_opt.expect("must produce session/prompt rpc");
    assert_eq!(prompt_rpc.method.as_deref(), Some("session/prompt"));
    assert_eq!(frames.len(), 1);
    assert!(matches!(
        frames[0],
        OutgoingFrame::Event {
            event: agui::Outgoing::RunStarted { .. },
            ..
        }
    ));

    // 3. Agent sends thought delta
    let thought_msg = JsonRpcMessage::notification(
        "session/update",
        serde_json::json!({
            "sessionId": "session-test-1",
            "update": { "type": "thought", "delta": "Inspecting lines 40-50..." }
        }),
    );
    let (_, thought_frames) = bridge.handle_agent_message(thought_msg);
    assert_eq!(thought_frames.len(), 1);
    assert!(matches!(
        thought_frames[0],
        OutgoingFrame::Event {
            event: agui::Outgoing::ReasoningMessageContent { ref delta, .. },
            ..
        } if delta == "Inspecting lines 40-50..."
    ));

    // 4. Agent sends text delta
    let text_msg = JsonRpcMessage::notification(
        "session/update",
        serde_json::json!({
            "sessionId": "session-test-1",
            "update": { "type": "text", "delta": "Fixed the issue by adding a guard." }
        }),
    );
    let (_, text_frames) = bridge.handle_agent_message(text_msg);
    assert_eq!(text_frames.len(), 1);
    assert!(matches!(
        text_frames[0],
        OutgoingFrame::Event {
            event: agui::Outgoing::TextMessageContent { ref delta, .. },
            ..
        } if delta == "Fixed the issue by adding a guard."
    ));

    // 5. Agent finishes session/prompt turn
    let prompt_resp = JsonRpcMessage::response(
        prompt_rpc.id.unwrap(),
        serde_json::json!({ "status": "completed" }),
    );
    let (_, done_frames) = bridge.handle_agent_message(prompt_resp);
    assert_eq!(done_frames.len(), 1);
    assert!(matches!(
        done_frames[0],
        OutgoingFrame::Event {
            event: agui::Outgoing::RunFinished { .. },
            ..
        }
    ));
}

#[test]
fn test_acp_bridge_permission_flow() {
    let mut bridge = AcpBridge::new("session-perm-1");

    // Agent requests permission to execute bash command
    let perm_req = JsonRpcMessage::request(
        42,
        "session/request_permission",
        serde_json::json!({
            "sessionId": "session-perm-1",
            "permission": {
                "type": "bash",
                "toolName": "bash",
                "command": "cargo test",
                "options": ["allow", "deny"]
            }
        }),
    );

    let (_, frames) = bridge.handle_agent_message(perm_req);
    assert_eq!(frames.len(), 3); // Start, Args, End

    let mut call_id = String::new();
    if let OutgoingFrame::Event {
        event:
            agui::Outgoing::ToolCallStart {
                ref tool_call_id,
                ref tool_call_name,
                ..
            },
        ..
    } = frames[0]
    {
        assert_eq!(tool_call_name, "ask");
        call_id = tool_call_id.clone();
    }
    assert_eq!(call_id, "perm-42");

    // Kobold answers with "allow"
    let (reply_opt, _) = bridge.handle_kobold_command(Command::ToolResult {
        lane: "main".into(),
        call_id: "perm-42".into(),
        output: "allow".into(),
        error: false,
    });

    let reply = reply_opt.expect("must reply to agent permission request");
    assert_eq!(reply.id, Some(Value::from(42)));
    assert_eq!(reply.result.unwrap()["decision"], "allow");

    // Now test refusal / deny
    let perm_req_2 = JsonRpcMessage::request(
        43,
        "session/request_permission",
        serde_json::json!({
            "sessionId": "session-perm-1",
            "permission": {
                "toolName": "bash",
                "command": "rm -rf /"
            }
        }),
    );
    let _ = bridge.handle_agent_message(perm_req_2);

    let (deny_reply_opt, _) = bridge.handle_kobold_command(Command::ToolResult {
        lane: "main".into(),
        call_id: "perm-43".into(),
        output: "deny".into(),
        error: false,
    });
    let deny_reply = deny_reply_opt.expect("must reply to deny");
    assert_eq!(deny_reply.result.unwrap()["decision"], "deny");
}

#[test]
fn test_acp_bridge_error_handling() {
    let mut bridge = AcpBridge::new("session-err-1");

    // Initialize first
    let init_resp =
        JsonRpcMessage::response(Value::from(0), serde_json::json!({"protocolVersion": 1}));
    let _ = bridge.handle_agent_message(init_resp);

    let (rpc_opt, _) = bridge.handle_kobold_command(Command::Send {
        lane: "main".into(),
        text: "fail please".into(),
        previous_response_id: None,
        replay: Vec::new(),
    });
    let prompt_id = rpc_opt.unwrap().id.unwrap();

    let err_resp =
        JsonRpcMessage::error_response(prompt_id, -32603, "Internal agent execution error");
    let (_, frames) = bridge.handle_agent_message(err_resp);
    assert_eq!(frames.len(), 1);
    assert!(matches!(
        frames[0],
        OutgoingFrame::Event {
            event: agui::Outgoing::RunError { ref message, ref code, .. },
            ..
        } if message == "Internal agent execution error" && code.as_deref() == Some("-32603")
    ));
}
