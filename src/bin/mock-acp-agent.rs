//! High-fidelity simulated ACP autonomous agent runner.
//!
//! Models the ACP JSON-RPC 2.0 interface and behavioral characteristics of:
//! - Claude Code (`claude-code`)
//! - Google Antigravity (`antigravity`)
//! - xAI Grok Build (`grok-build`)
//! - OpenAI Codex (`codex`)
//! - Meta Muse (`muse`)
//! - OpenCode (`opencode`)

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{BufRead, Write};

#[derive(Debug, Serialize, Deserialize)]
struct RpcMessage {
    jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<Value>,
}

impl RpcMessage {
    fn response(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: Some(id),
            method: None,
            params: None,
            result: Some(result),
            error: None,
        }
    }

    fn notification(method: &str, params: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: None,
            method: Some(method.into()),
            params: Some(params),
            result: None,
            error: None,
        }
    }

    fn request(id: Value, method: &str, params: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: Some(id),
            method: Some(method.into()),
            params: Some(params),
            result: None,
            error: None,
        }
    }
}

fn emit(msg: &RpcMessage) {
    let json = serde_json::to_string(msg).expect("serialize rpc");
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{json}");
    let _ = stdout.flush();
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut agent = std::env::var("ACP_AGENT_NAME").unwrap_or_else(|_| "claude-code".to_string());

    let mut i = 0;
    while i < args.len() {
        if args[i] == "--agent" && i + 1 < args.len() {
            agent = args[i + 1].clone();
            i += 2;
        } else if !args[i].starts_with('-') {
            agent = args[i].clone();
            i += 1;
        } else {
            i += 1;
        }
    }

    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();

    while let Some(Ok(line)) = lines.next() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<RpcMessage>(trimmed) else {
            continue;
        };

        match msg.method.as_deref() {
            Some("initialize") => {
                let id = msg.id.unwrap_or(Value::from(1));
                let (name, ver, caps) = match agent.as_str() {
                    "claude-code" | "claude" => (
                        "claude-code",
                        "1.0.0",
                        serde_json::json!({ "prompts": {}, "tools": {}, "reasoning": true }),
                    ),
                    "antigravity" => (
                        "antigravity",
                        "2.0.0",
                        serde_json::json!({ "prompts": {}, "tools": {}, "subagents": true, "artifacts": true }),
                    ),
                    "grok-build" | "grok" => (
                        "grok-build",
                        "3.0.0",
                        serde_json::json!({ "prompts": {}, "tools": {}, "fast_tokens": true }),
                    ),
                    "codex" => (
                        "codex",
                        "0.1.0",
                        serde_json::json!({ "prompts": {}, "tools": {}, "patch": true }),
                    ),
                    "muse" => (
                        "muse",
                        "1.5.0",
                        serde_json::json!({ "prompts": {}, "tools": {}, "multi_repo": true }),
                    ),
                    "opencode" => (
                        "opencode",
                        "0.9.0",
                        serde_json::json!({ "prompts": {}, "tools": {}, "terminal": true }),
                    ),
                    other => (
                        other,
                        "0.1.0",
                        serde_json::json!({ "prompts": {}, "tools": {} }),
                    ),
                };

                let resp = RpcMessage::response(
                    id,
                    serde_json::json!({
                        "protocolVersion": 1,
                        "agentInfo": {
                            "name": name,
                            "version": ver
                        },
                        "capabilities": caps
                    }),
                );
                emit(&resp);
            }
            Some("session/prompt") => {
                let prompt_id = msg.id.unwrap_or(Value::from(1));
                let session_id = msg
                    .params
                    .as_ref()
                    .and_then(|p| p.get("sessionId"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("default-sess")
                    .to_string();
                let prompt_text = msg
                    .params
                    .as_ref()
                    .and_then(|p| p.get("prompt"))
                    .and_then(|arr| arr.as_array())
                    .and_then(|items| items.first())
                    .and_then(|it| it.get("text"))
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string();

                match agent.as_str() {
                    "claude-code" | "claude" => {
                        // 1. Reasoning thought delta
                        emit(&RpcMessage::notification(
                            "session/update",
                            serde_json::json!({
                                "sessionId": session_id,
                                "update": { "type": "thought", "delta": "Claude Code analyzing architecture and auth pipeline..." }
                            }),
                        ));
                        // 2. Text delta
                        emit(&RpcMessage::notification(
                            "session/update",
                            serde_json::json!({
                                "sessionId": session_id,
                                "update": { "type": "text", "delta": "Running cargo check for verification." }
                            }),
                        ));
                        // 3. Permission request
                        let req_id = Value::from(101);
                        emit(&RpcMessage::request(
                            req_id.clone(),
                            "session/request_permission",
                            serde_json::json!({
                                "sessionId": session_id,
                                "permission": {
                                    "type": "tool",
                                    "toolName": "bash",
                                    "command": "cargo check",
                                    "options": ["allow", "deny"]
                                }
                            }),
                        ));
                        // 4. Await permission response
                        if let Some(Ok(resp_line)) = lines.next() {
                            let resp: Result<RpcMessage, _> = serde_json::from_str(&resp_line);
                            let decision = resp
                                .ok()
                                .and_then(|r| r.result)
                                .and_then(|res| res.get("decision").cloned())
                                .and_then(|d| d.as_str().map(String::from))
                                .unwrap_or_else(|| "deny".into());

                            if decision == "allow" {
                                emit(&RpcMessage::notification(
                                    "session/update",
                                    serde_json::json!({
                                        "sessionId": session_id,
                                        "update": { "type": "text", "delta": "Check passed. Claude Code finished task." }
                                    }),
                                ));
                            } else {
                                emit(&RpcMessage::notification(
                                    "session/update",
                                    serde_json::json!({
                                        "sessionId": session_id,
                                        "update": { "type": "text", "delta": "Permission denied by user. Halting." }
                                    }),
                                ));
                            }
                        }
                        // 5. Complete session/prompt turn
                        emit(&RpcMessage::response(
                            prompt_id,
                            serde_json::json!({ "status": "completed" }),
                        ));
                    }
                    "antigravity" => {
                        // 1. Mission planner thought
                        emit(&RpcMessage::notification(
                            "session/update",
                            serde_json::json!({
                                "sessionId": session_id,
                                "update": { "type": "thought", "delta": "Antigravity mission planner: decomposing requirements." }
                            }),
                        ));
                        // 2. Tool call start & chunk & completion
                        emit(&RpcMessage::notification(
                            "session/update",
                            serde_json::json!({
                                "sessionId": session_id,
                                "update": {
                                    "type": "tool_call",
                                    "toolCallId": "ag-call-1",
                                    "name": "view_file",
                                    "status": "in_progress"
                                }
                            }),
                        ));
                        emit(&RpcMessage::notification(
                            "session/update",
                            serde_json::json!({
                                "sessionId": session_id,
                                "update": {
                                    "type": "tool_call_chunk",
                                    "toolCallId": "ag-call-1",
                                    "delta": "{\"path\": \"Cargo.toml\"}"
                                }
                            }),
                        ));
                        emit(&RpcMessage::notification(
                            "session/update",
                            serde_json::json!({
                                "sessionId": session_id,
                                "update": {
                                    "type": "tool_call",
                                    "toolCallId": "ag-call-1",
                                    "status": "completed"
                                }
                            }),
                        ));
                        // 3. Client tool execution request
                        let exec_id = Value::from(201);
                        emit(&RpcMessage::request(
                            exec_id.clone(),
                            "session/execute_tool",
                            serde_json::json!({
                                "sessionId": session_id,
                                "toolName": "file_read",
                                "arguments": { "path": "Cargo.toml" }
                            }),
                        ));
                        // Read client execution response
                        let _ = lines.next();

                        // 4. Workspace permission request
                        let perm_id = Value::from(202);
                        emit(&RpcMessage::request(
                            perm_id.clone(),
                            "session/request_permission",
                            serde_json::json!({
                                "sessionId": session_id,
                                "permission": {
                                    "type": "file_edit",
                                    "toolName": "write_to_file",
                                    "command": "Update src/greeting.rs",
                                    "options": ["allow", "deny"]
                                }
                            }),
                        ));
                        if let Some(Ok(resp_line)) = lines.next() {
                            let resp: Result<RpcMessage, _> = serde_json::from_str(&resp_line);
                            let decision = resp
                                .ok()
                                .and_then(|r| r.result)
                                .and_then(|res| res.get("decision").cloned())
                                .and_then(|d| d.as_str().map(String::from))
                                .unwrap_or_else(|| "deny".into());

                            if decision == "allow" {
                                emit(&RpcMessage::notification(
                                    "session/update",
                                    serde_json::json!({
                                        "sessionId": session_id,
                                        "update": { "type": "text", "delta": "Antigravity task completed: greeting.rs written." }
                                    }),
                                ));
                            } else {
                                emit(&RpcMessage::notification(
                                    "session/update",
                                    serde_json::json!({
                                        "sessionId": session_id,
                                        "update": { "type": "text", "delta": "Antigravity file update declined." }
                                    }),
                                ));
                            }
                        }
                        emit(&RpcMessage::response(
                            prompt_id,
                            serde_json::json!({ "status": "completed" }),
                        ));
                    }
                    "grok-build" | "grok" => {
                        // High-speed reasoning chunks
                        emit(&RpcMessage::notification(
                            "session/update",
                            serde_json::json!({
                                "sessionId": session_id,
                                "update": { "type": "thought", "delta": "xAI Grok Build compiler optimizing target graph..." }
                            }),
                        ));
                        emit(&RpcMessage::notification(
                            "session/update",
                            serde_json::json!({
                                "sessionId": session_id,
                                "update": { "type": "thought", "delta": "Computing parallel incremental compilation DAG..." }
                            }),
                        ));
                        // Permission request
                        let perm_id = Value::from(301);
                        emit(&RpcMessage::request(
                            perm_id.clone(),
                            "session/request_permission",
                            serde_json::json!({
                                "sessionId": session_id,
                                "permission": {
                                    "type": "bash",
                                    "toolName": "bash",
                                    "command": "cargo build --release --workspace",
                                    "options": ["allow", "deny"]
                                }
                            }),
                        ));
                        if let Some(Ok(resp_line)) = lines.next() {
                            let resp: Result<RpcMessage, _> = serde_json::from_str(&resp_line);
                            let decision = resp
                                .ok()
                                .and_then(|r| r.result)
                                .and_then(|res| res.get("decision").cloned())
                                .and_then(|d| d.as_str().map(String::from))
                                .unwrap_or_else(|| "deny".into());

                            if decision == "allow" {
                                emit(&RpcMessage::notification(
                                    "session/update",
                                    serde_json::json!({
                                        "sessionId": session_id,
                                        "update": { "type": "text", "delta": "Grok Build finished in 1.4s. 6 crates compiled." }
                                    }),
                                ));
                            } else {
                                emit(&RpcMessage::notification(
                                    "session/update",
                                    serde_json::json!({
                                        "sessionId": session_id,
                                        "update": { "type": "text", "delta": "Build command cancelled." }
                                    }),
                                ));
                            }
                        }
                        emit(&RpcMessage::response(
                            prompt_id,
                            serde_json::json!({ "status": "completed" }),
                        ));
                    }
                    "codex" => {
                        emit(&RpcMessage::notification(
                            "session/update",
                            serde_json::json!({
                                "sessionId": session_id,
                                "update": { "type": "thought", "delta": "OpenAI Codex generating implementation code..." }
                            }),
                        ));
                        emit(&RpcMessage::notification(
                            "session/update",
                            serde_json::json!({
                                "sessionId": session_id,
                                "update": { "type": "text", "delta": "```rust\npub fn acp_ready() -> bool { true }\n```" }
                            }),
                        ));
                        let perm_id = Value::from(401);
                        emit(&RpcMessage::request(
                            perm_id.clone(),
                            "session/request_permission",
                            serde_json::json!({
                                "sessionId": session_id,
                                "permission": {
                                    "type": "patch",
                                    "toolName": "git_apply",
                                    "command": "git apply codex.patch",
                                    "options": ["allow", "deny"]
                                }
                            }),
                        ));
                        if let Some(Ok(resp_line)) = lines.next() {
                            let resp: Result<RpcMessage, _> = serde_json::from_str(&resp_line);
                            let decision = resp
                                .ok()
                                .and_then(|r| r.result)
                                .and_then(|res| res.get("decision").cloned())
                                .and_then(|d| d.as_str().map(String::from))
                                .unwrap_or_else(|| "deny".into());

                            if decision == "allow" {
                                emit(&RpcMessage::notification(
                                    "session/update",
                                    serde_json::json!({
                                        "sessionId": session_id,
                                        "update": { "type": "text", "delta": "Patch codex.patch applied cleanly." }
                                    }),
                                ));
                            } else {
                                emit(&RpcMessage::notification(
                                    "session/update",
                                    serde_json::json!({
                                        "sessionId": session_id,
                                        "update": { "type": "text", "delta": "Patch application rejected." }
                                    }),
                                ));
                            }
                        }
                        emit(&RpcMessage::response(
                            prompt_id,
                            serde_json::json!({ "status": "completed" }),
                        ));
                    }
                    "muse" => {
                        emit(&RpcMessage::notification(
                            "session/update",
                            serde_json::json!({
                                "sessionId": session_id,
                                "update": { "type": "thought", "delta": "Meta Muse analyzing cross-crate dependencies..." }
                            }),
                        ));
                        emit(&RpcMessage::notification(
                            "session/update",
                            serde_json::json!({
                                "sessionId": session_id,
                                "update": { "type": "text", "delta": "Proposed refactoring touches 3 crates and 12 interfaces." }
                            }),
                        ));
                        let perm_id = Value::from(501);
                        emit(&RpcMessage::request(
                            perm_id.clone(),
                            "session/request_permission",
                            serde_json::json!({
                                "sessionId": session_id,
                                "permission": {
                                    "type": "changeset",
                                    "toolName": "workspace_patch",
                                    "description": "Apply multi-crate refactoring changeset",
                                    "options": ["allow", "deny"]
                                }
                            }),
                        ));
                        if let Some(Ok(resp_line)) = lines.next() {
                            let resp: Result<RpcMessage, _> = serde_json::from_str(&resp_line);
                            let decision = resp
                                .ok()
                                .and_then(|r| r.result)
                                .and_then(|res| res.get("decision").cloned())
                                .and_then(|d| d.as_str().map(String::from))
                                .unwrap_or_else(|| "deny".into());

                            if decision == "allow" {
                                emit(&RpcMessage::notification(
                                    "session/update",
                                    serde_json::json!({
                                        "sessionId": session_id,
                                        "update": { "type": "text", "delta": "Changeset committed across all workspaces." }
                                    }),
                                ));
                            } else {
                                emit(&RpcMessage::notification(
                                    "session/update",
                                    serde_json::json!({
                                        "sessionId": session_id,
                                        "update": { "type": "text", "delta": "Changeset rejected. Preserving original workspace state." }
                                    }),
                                ));
                            }
                        }
                        emit(&RpcMessage::response(
                            prompt_id,
                            serde_json::json!({ "status": "completed" }),
                        ));
                    }
                    "opencode" => {
                        emit(&RpcMessage::notification(
                            "session/update",
                            serde_json::json!({
                                "sessionId": session_id,
                                "update": { "type": "thought", "delta": "OpenCode terminal agent parsing command requirements..." }
                            }),
                        ));
                        emit(&RpcMessage::notification(
                            "session/update",
                            serde_json::json!({
                                "sessionId": session_id,
                                "update": {
                                    "type": "tool_call",
                                    "toolCallId": "oc-tc-1",
                                    "name": "sh",
                                    "status": "in_progress"
                                }
                            }),
                        ));
                        emit(&RpcMessage::notification(
                            "session/update",
                            serde_json::json!({
                                "sessionId": session_id,
                                "update": {
                                    "type": "tool_call_chunk",
                                    "toolCallId": "oc-tc-1",
                                    "delta": "npm test"
                                }
                            }),
                        ));
                        emit(&RpcMessage::notification(
                            "session/update",
                            serde_json::json!({
                                "sessionId": session_id,
                                "update": {
                                    "type": "tool_call",
                                    "toolCallId": "oc-tc-1",
                                    "status": "completed"
                                }
                            }),
                        ));
                        let perm_id = Value::from(601);
                        emit(&RpcMessage::request(
                            perm_id.clone(),
                            "session/request_permission",
                            serde_json::json!({
                                "sessionId": session_id,
                                "permission": {
                                    "type": "bash",
                                    "toolName": "sh",
                                    "command": "npm test",
                                    "options": ["allow", "deny"]
                                }
                            }),
                        ));
                        if let Some(Ok(resp_line)) = lines.next() {
                            let resp: Result<RpcMessage, _> = serde_json::from_str(&resp_line);
                            let decision = resp
                                .ok()
                                .and_then(|r| r.result)
                                .and_then(|res| res.get("decision").cloned())
                                .and_then(|d| d.as_str().map(String::from))
                                .unwrap_or_else(|| "deny".into());

                            if decision == "allow" {
                                emit(&RpcMessage::notification(
                                    "session/update",
                                    serde_json::json!({
                                        "sessionId": session_id,
                                        "update": { "type": "text", "delta": "All 14 test suites passed successfully." }
                                    }),
                                ));
                            } else {
                                emit(&RpcMessage::notification(
                                    "session/update",
                                    serde_json::json!({
                                        "sessionId": session_id,
                                        "update": { "type": "text", "delta": "Test execution skipped by user." }
                                    }),
                                ));
                            }
                        }
                        emit(&RpcMessage::response(
                            prompt_id,
                            serde_json::json!({ "status": "completed" }),
                        ));
                    }
                    "steering" | "cancellable" => {
                        if prompt_text.contains("Pivot") || prompt_text.contains("steer") {
                            emit(&RpcMessage::notification(
                                "session/update",
                                serde_json::json!({
                                    "sessionId": session_id,
                                    "update": { "type": "thought", "delta": "Pivoting to user-requested steered task." }
                                }),
                            ));
                            emit(&RpcMessage::notification(
                                "session/update",
                                serde_json::json!({
                                    "sessionId": session_id,
                                    "update": { "type": "text", "delta": "Steered task completed successfully." }
                                }),
                            ));
                            emit(&RpcMessage::response(
                                prompt_id,
                                serde_json::json!({ "status": "completed" }),
                            ));
                            continue;
                        }

                        emit(&RpcMessage::notification(
                            "session/update",
                            serde_json::json!({
                                "sessionId": session_id,
                                "update": { "type": "thought", "delta": "Steering agent started a long-running multi-step task..." }
                            }),
                        ));
                        emit(&RpcMessage::notification(
                            "session/update",
                            serde_json::json!({
                                "sessionId": session_id,
                                "update": { "type": "text", "delta": "Processing chunk 1/100..." }
                            }),
                        ));

                        if let Some(Ok(next_line)) = lines.next() {
                            let next_msg: Result<RpcMessage, _> = serde_json::from_str(&next_line);
                            if let Ok(msg) = next_msg {
                                if msg.method.as_deref() == Some("session/cancel") {
                                    emit(&RpcMessage::notification(
                                        "session/update",
                                        serde_json::json!({
                                            "sessionId": session_id,
                                            "update": { "type": "text", "delta": " [Interrupted by user]" }
                                        }),
                                    ));
                                    emit(&RpcMessage::response(
                                        prompt_id,
                                        serde_json::json!({ "status": "cancelled" }),
                                    ));
                                    continue;
                                }
                            }
                        }
                        emit(&RpcMessage::response(
                            prompt_id,
                            serde_json::json!({ "status": "completed" }),
                        ));
                    }
                    _ => {
                        emit(&RpcMessage::notification(
                            "session/update",
                            serde_json::json!({
                                "sessionId": session_id,
                                "update": { "type": "text", "delta": format!("Generic response from {agent}") }
                            }),
                        ));
                        emit(&RpcMessage::response(
                            prompt_id,
                            serde_json::json!({ "status": "completed" }),
                        ));
                    }
                }
            }
            Some("session/cancel") => {
                // Handled gracefully
            }
            _ => {}
        }
    }
}
