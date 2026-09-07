//! Bridges between Kobold Southbound protocol (`Command` / `OutgoingFrame`) and ACP (`JsonRpcMessage`).

use kobold_proto::agui::{self, Base};
use kobold_proto::{Command, OutgoingFrame, Transport};
use serde_json::Value;
use std::collections::HashMap;

use crate::proto::{
    ClientInfo, ExecuteToolParams, InitializeParams, JsonRpcMessage, PromptItem,
    RequestPermissionParams, SessionPromptParams, SessionUpdate, SessionUpdateParams,
};

/// The bridge state machine.
pub struct AcpBridge {
    next_rpc_id: u64,
    current_lane: String,
    session_id: String,
    initialized: bool,
    pending_prompt_rpc_id: Option<u64>,
    pending_permissions: HashMap<String, Value>,
    pending_tool_calls: HashMap<String, Value>,
}

impl AcpBridge {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            next_rpc_id: 1,
            current_lane: "main".into(),
            session_id: session_id.into(),
            initialized: false,
            pending_prompt_rpc_id: None,
            pending_permissions: HashMap::new(),
            pending_tool_calls: HashMap::new(),
        }
    }

    /// Generates the initial ACP `initialize` request to be sent to the agent.
    pub fn create_initialize_request(&mut self) -> JsonRpcMessage {
        let id = self.next_id();
        let params = InitializeParams {
            protocol_version: 1,
            client_info: ClientInfo {
                name: "kobold".into(),
                version: "0.1.0".into(),
            },
            capabilities: serde_json::json!({
                "prompts": {},
                "tools": {}
            }),
        };
        JsonRpcMessage::request(
            id,
            "initialize",
            serde_json::to_value(params).unwrap_or(Value::Null),
        )
    }

    /// Handles a Command from Kobold (`Command::Send`, `Command::ToolResult`, `Command::Quit`).
    /// Returns:
    /// - `Option<JsonRpcMessage>`: outbound message to send to the child agent
    /// - `Vec<OutgoingFrame>`: immediate outbound frames to emit to Kobold
    pub fn handle_kobold_command(
        &mut self,
        command: Command,
    ) -> (Option<JsonRpcMessage>, Vec<OutgoingFrame>) {
        match command {
            Command::Send { lane, text, .. } => {
                self.current_lane = lane.clone();
                let id = self.next_id();
                self.pending_prompt_rpc_id = Some(id);

                let params = SessionPromptParams {
                    session_id: self.session_id.clone(),
                    prompt: vec![PromptItem::Text { text }],
                };
                let rpc_msg = JsonRpcMessage::request(
                    id,
                    "session/prompt",
                    serde_json::to_value(params).unwrap_or(Value::Null),
                );

                let start_frame = OutgoingFrame::Event {
                    lane: lane.clone(),
                    event: agui::Outgoing::RunStarted {
                        base: Base::default(),
                        thread_id: lane,
                        run_id: format!("run-{}", id),
                        parent_run_id: None,
                    },
                };
                (Some(rpc_msg), vec![start_frame])
            }
            Command::ToolResult {
                lane: _,
                call_id,
                output,
                error,
            } => {
                if let Some(rpc_id) = self.pending_permissions.remove(&call_id) {
                    let decision = if error {
                        "deny"
                    } else {
                        let lower = output.to_lowercase();
                        if lower.contains("deny")
                            || lower.contains("no")
                            || lower.contains("cancel")
                        {
                            "deny"
                        } else {
                            "allow"
                        }
                    };
                    let resp = JsonRpcMessage::response(
                        rpc_id,
                        serde_json::json!({ "decision": decision }),
                    );
                    (Some(resp), Vec::new())
                } else if let Some(rpc_id) = self.pending_tool_calls.remove(&call_id) {
                    let resp = if error {
                        JsonRpcMessage::error_response(rpc_id, -32000, output)
                    } else {
                        JsonRpcMessage::response(rpc_id, serde_json::json!({ "output": output }))
                    };
                    (Some(resp), Vec::new())
                } else {
                    (None, Vec::new())
                }
            }
            Command::Cancel { lane: _ } => {
                self.pending_prompt_rpc_id = None;
                self.pending_permissions.clear();
                self.pending_tool_calls.clear();
                let cancel = JsonRpcMessage::notification(
                    "session/cancel",
                    serde_json::json!({ "sessionId": self.session_id }),
                );
                (Some(cancel), Vec::new())
            }
            Command::Quit => {
                let cancel = JsonRpcMessage::notification(
                    "session/cancel",
                    serde_json::json!({ "sessionId": self.session_id }),
                );
                (Some(cancel), Vec::new())
            }
        }
    }

    /// Handles an incoming JSON-RPC message from the child agent.
    /// Returns:
    /// - `Option<JsonRpcMessage>`: any immediate response to the agent
    /// - `Vec<OutgoingFrame>`: outbound frames to emit to Kobold
    pub fn handle_agent_message(
        &mut self,
        msg: JsonRpcMessage,
    ) -> (Option<JsonRpcMessage>, Vec<OutgoingFrame>) {
        // 1. Response to `initialize`
        if !self.initialized && msg.is_response() {
            self.initialized = true;
            return (None, vec![OutgoingFrame::Transport(Transport::Connected)]);
        }

        // 2. Response to `session/prompt`
        if let Some(pending_id) = self.pending_prompt_rpc_id {
            if let Some(msg_id) = &msg.id {
                let matches_id = msg_id.as_u64() == Some(pending_id)
                    || msg_id.as_str() == Some(&pending_id.to_string());
                if matches_id {
                    self.pending_prompt_rpc_id = None;
                    if let Some(err) = msg.error {
                        return (
                            None,
                            vec![OutgoingFrame::Event {
                                lane: self.current_lane.clone(),
                                event: agui::Outgoing::RunError {
                                    base: Base::default(),
                                    code: Some(err.code.to_string()),
                                    message: err.message,
                                    usage: None,
                                },
                            }],
                        );
                    } else {
                        return (
                            None,
                            vec![OutgoingFrame::Event {
                                lane: self.current_lane.clone(),
                                event: agui::Outgoing::RunFinished {
                                    base: Base::default(),
                                    thread_id: self.current_lane.clone(),
                                    run_id: format!("run-{}", pending_id),
                                    usage: None,
                                },
                            }],
                        );
                    }
                }
            }
        }

        // 3. Notification: `session/update`
        if msg.is_notification() && msg.method.as_deref() == Some("session/update") {
            if let Some(ref params_val) = msg.params {
                if let Ok(params) =
                    serde_json::from_value::<SessionUpdateParams>(params_val.clone())
                {
                    let frame = match params.update {
                        SessionUpdate::Text { delta } => OutgoingFrame::Event {
                            lane: self.current_lane.clone(),
                            event: agui::Outgoing::TextMessageContent {
                                base: Base::default(),
                                message_id: format!("msg-{}", self.next_rpc_id),
                                delta,
                            },
                        },
                        SessionUpdate::Thought { delta } => OutgoingFrame::Event {
                            lane: self.current_lane.clone(),
                            event: agui::Outgoing::ReasoningMessageContent {
                                base: Base::default(),
                                message_id: format!("thought-{}", self.next_rpc_id),
                                delta,
                            },
                        },
                        SessionUpdate::ToolCall {
                            tool_call_id,
                            name,
                            status,
                        } => {
                            if let Some(s) = &status {
                                if s == "completed" || s == "done" || s == "finished" {
                                    return (
                                        None,
                                        vec![OutgoingFrame::Event {
                                            lane: self.current_lane.clone(),
                                            event: agui::Outgoing::ToolCallEnd {
                                                base: Base::default(),
                                                tool_call_id,
                                            },
                                        }],
                                    );
                                }
                            }
                            OutgoingFrame::Event {
                                lane: self.current_lane.clone(),
                                event: agui::Outgoing::ToolCallStart {
                                    base: Base::default(),
                                    tool_call_id,
                                    tool_call_name: name.unwrap_or_else(|| "tool".into()),
                                    parent_message_id: None,
                                },
                            }
                        }
                        SessionUpdate::ToolCallChunk {
                            tool_call_id,
                            delta,
                        } => OutgoingFrame::Event {
                            lane: self.current_lane.clone(),
                            event: agui::Outgoing::ToolCallArgs {
                                base: Base::default(),
                                tool_call_id,
                                delta,
                            },
                        },
                        SessionUpdate::Unknown => return (None, Vec::new()),
                    };
                    return (None, vec![frame]);
                }
            }
        }

        // 4. Request: `session/request_permission`
        if msg.is_request() && msg.method.as_deref() == Some("session/request_permission") {
            if let (Some(rpc_id), Some(params_val)) = (&msg.id, &msg.params) {
                if let Ok(params) =
                    serde_json::from_value::<RequestPermissionParams>(params_val.clone())
                {
                    let id_str = match rpc_id {
                        Value::String(s) => s.clone(),
                        Value::Number(n) => n.to_string(),
                        other => other.to_string(),
                    };
                    let call_id = format!("perm-{id_str}");
                    self.pending_permissions
                        .insert(call_id.clone(), rpc_id.clone());

                    let tool_name = params.permission.tool_name.unwrap_or_else(|| "tool".into());
                    let desc = params
                        .permission
                        .command
                        .or(params.permission.description)
                        .unwrap_or_else(|| "permission required".into());

                    let question = format!("Allow {tool_name}: {desc}?");
                    let args = serde_json::json!({
                        "question": question,
                        "options": ["allow", "deny"]
                    })
                    .to_string();

                    let frames = vec![
                        OutgoingFrame::Event {
                            lane: self.current_lane.clone(),
                            event: agui::Outgoing::ToolCallStart {
                                base: Base::default(),
                                tool_call_id: call_id.clone(),
                                tool_call_name: "ask".into(),
                                parent_message_id: None,
                            },
                        },
                        OutgoingFrame::Event {
                            lane: self.current_lane.clone(),
                            event: agui::Outgoing::ToolCallArgs {
                                base: Base::default(),
                                tool_call_id: call_id.clone(),
                                delta: args,
                            },
                        },
                        OutgoingFrame::Event {
                            lane: self.current_lane.clone(),
                            event: agui::Outgoing::ToolCallEnd {
                                base: Base::default(),
                                tool_call_id: call_id,
                            },
                        },
                    ];
                    return (None, frames);
                }
            }
        }

        // 5. Request: `session/execute_tool`
        if msg.is_request() && msg.method.as_deref() == Some("session/execute_tool") {
            if let (Some(rpc_id), Some(params_val)) = (&msg.id, &msg.params) {
                if let Ok(params) = serde_json::from_value::<ExecuteToolParams>(params_val.clone())
                {
                    let id_str = match rpc_id {
                        Value::String(s) => s.clone(),
                        Value::Number(n) => n.to_string(),
                        other => other.to_string(),
                    };
                    let call_id = format!("exec-{id_str}");
                    self.pending_tool_calls
                        .insert(call_id.clone(), rpc_id.clone());
                    let args = params.arguments.to_string();

                    let frames = vec![
                        OutgoingFrame::Event {
                            lane: self.current_lane.clone(),
                            event: agui::Outgoing::ToolCallStart {
                                base: Base::default(),
                                tool_call_id: call_id.clone(),
                                tool_call_name: params.tool_name,
                                parent_message_id: None,
                            },
                        },
                        OutgoingFrame::Event {
                            lane: self.current_lane.clone(),
                            event: agui::Outgoing::ToolCallArgs {
                                base: Base::default(),
                                tool_call_id: call_id.clone(),
                                delta: args,
                            },
                        },
                        OutgoingFrame::Event {
                            lane: self.current_lane.clone(),
                            event: agui::Outgoing::ToolCallEnd {
                                base: Base::default(),
                                tool_call_id: call_id,
                            },
                        },
                    ];
                    return (None, frames);
                }
            }
        }

        (None, Vec::new())
    }

    fn next_id(&mut self) -> u64 {
        let id = self.next_rpc_id;
        self.next_rpc_id = self.next_rpc_id.wrapping_add(1);
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_flow_emits_connected_transport() {
        let mut bridge = AcpBridge::new("sess-1");
        let init_req = bridge.create_initialize_request();
        assert_eq!(init_req.method.as_deref(), Some("initialize"));

        let init_resp =
            JsonRpcMessage::response(Value::from(1), serde_json::json!({"protocolVersion": 1}));
        let (_, frames) = bridge.handle_agent_message(init_resp);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0], OutgoingFrame::Transport(Transport::Connected));
    }

    #[test]
    fn send_command_generates_prompt_rpc_and_run_started() {
        let mut bridge = AcpBridge::new("sess-1");
        let (rpc_opt, frames) = bridge.handle_kobold_command(Command::Send {
            lane: "main".into(),
            text: "list files".into(),
            previous_response_id: None,
            replay: Vec::new(),
        });

        let rpc = rpc_opt.expect("must generate prompt rpc");
        assert_eq!(rpc.method.as_deref(), Some("session/prompt"));
        assert_eq!(frames.len(), 1);
        assert!(matches!(
            frames[0],
            OutgoingFrame::Event {
                event: agui::Outgoing::RunStarted { .. },
                ..
            }
        ));
    }

    #[test]
    fn session_update_translates_to_agui_events() {
        let mut bridge = AcpBridge::new("sess-1");

        let text_msg = JsonRpcMessage::notification(
            "session/update",
            serde_json::json!({
                "sessionId": "sess-1",
                "update": { "type": "text", "delta": "hello" }
            }),
        );
        let (_, frames) = bridge.handle_agent_message(text_msg);
        assert_eq!(frames.len(), 1);
        assert!(matches!(
            frames[0],
            OutgoingFrame::Event {
                ref lane,
                event: agui::Outgoing::TextMessageContent { ref delta, .. }
            } if lane == "main" && delta == "hello"
        ));
    }

    #[test]
    fn permission_request_and_response_flow() {
        let mut bridge = AcpBridge::new("sess-1");

        let perm_req = JsonRpcMessage::request(
            10,
            "session/request_permission",
            serde_json::json!({
                "sessionId": "sess-1",
                "permission": {
                    "toolName": "bash",
                    "command": "echo test"
                }
            }),
        );
        let (_, frames) = bridge.handle_agent_message(perm_req);
        assert_eq!(frames.len(), 3); // ToolCallStart, ToolCallArgs, ToolCallEnd
        assert!(
            matches!(frames[0], OutgoingFrame::Event { event: agui::Outgoing::ToolCallStart { ref tool_call_id, .. }, .. } if tool_call_id == "perm-10")
        );

        // Now Kobold answers via ToolResult
        let (resp_opt, _) = bridge.handle_kobold_command(Command::ToolResult {
            lane: "main".into(),
            call_id: "perm-10".into(),
            output: "allow".into(),
            error: false,
        });

        let resp = resp_opt.expect("must generate response");
        assert_eq!(resp.id, Some(Value::from(10)));
        assert_eq!(resp.result.unwrap()["decision"], "allow");
    }

    #[test]
    fn execute_tool_request_and_response_flow() {
        let mut bridge = AcpBridge::new("sess-1");

        let exec_req = JsonRpcMessage::request(
            25,
            "session/execute_tool",
            serde_json::json!({
                "sessionId": "sess-1",
                "toolName": "read_file",
                "arguments": { "path": "src/main.rs" }
            }),
        );
        let (_, frames) = bridge.handle_agent_message(exec_req);
        assert_eq!(frames.len(), 3);
        assert!(matches!(
            frames[0],
            OutgoingFrame::Event {
                event: agui::Outgoing::ToolCallStart { ref tool_call_id, ref tool_call_name, .. },
                ..
            } if tool_call_id == "exec-25" && tool_call_name == "read_file"
        ));

        // Kobold responds with ToolResult
        let (resp_opt, _) = bridge.handle_kobold_command(Command::ToolResult {
            lane: "main".into(),
            call_id: "exec-25".into(),
            output: "fn main() {}".into(),
            error: false,
        });
        let resp = resp_opt.expect("must generate response");
        assert_eq!(resp.id, Some(Value::from(25)));
        assert_eq!(resp.result.unwrap()["output"], "fn main() {}");
    }

    #[test]
    fn tool_call_completion_emits_tool_call_end() {
        let mut bridge = AcpBridge::new("sess-1");

        let tc_completed = JsonRpcMessage::notification(
            "session/update",
            serde_json::json!({
                "sessionId": "sess-1",
                "update": {
                    "type": "tool_call",
                    "toolCallId": "call-99",
                    "status": "completed"
                }
            }),
        );
        let (_, frames) = bridge.handle_agent_message(tc_completed);
        assert_eq!(frames.len(), 1);
        assert!(matches!(
            frames[0],
            OutgoingFrame::Event {
                event: agui::Outgoing::ToolCallEnd { ref tool_call_id, .. },
                ..
            } if tool_call_id == "call-99"
        ));
    }
}
