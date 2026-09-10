use std::fs;
use std::path::Path;
use std::sync::Arc;
use tempfile::tempdir;

use kobold_runtime::{
    AgentSession, ApprovalAction, ApprovalPolicy, SessionBuilder, ToolCall, TurnFinishReason,
};

/// Helper to load LLM_API_KEY from environment or .env file in workspace.
fn get_test_api_key() -> Option<String> {
    if let Ok(k) = std::env::var("LLM_API_KEY") {
        if !k.is_empty() {
            return Some(k);
        }
    }
    if let Ok(k) = std::env::var("OPENAI_API_KEY") {
        if !k.is_empty() {
            return Some(k);
        }
    }
    // Search for .env in current and parent directories
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let env_file = dir.join(".env");
        if env_file.is_file() {
            if let Ok(content) = std::fs::read_to_string(&env_file) {
                for line in content.lines() {
                    let trimmed = line.trim();
                    if let Some(rest) = trimmed.strip_prefix("export ") {
                        if let Some((k, v)) = rest.split_once('=') {
                            if k.trim() == "LLM_API_KEY" || k.trim() == "OPENAI_API_KEY" {
                                let val = v.trim().trim_matches('"').trim_matches('\'');
                                return Some(val.to_string());
                            }
                        }
                    } else if let Some((k, v)) = trimmed.split_once('=') {
                        if k.trim() == "LLM_API_KEY" || k.trim() == "OPENAI_API_KEY" {
                            let val = v.trim().trim_matches('"').trim_matches('\'');
                            return Some(val.to_string());
                        }
                    }
                }
            }
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

/// Helper to build a live session targeting gpt-5.6-luna with low reasoning effort.
fn create_live_session(workspace_root: &Path, zdr: bool) -> Option<AgentSession> {
    let api_key = get_test_api_key()?;
    SessionBuilder::new()
        .with_workspace_root(workspace_root)
        .with_openai_backend(api_key, Some("gpt-5.6-luna"), Some("low"))
        .with_zdr(zdr)
        .build()
        .ok()
}

// =========================================================================
// Category 1: Happy Paths
// =========================================================================

#[tokio::test]
async fn test_live_e2e_happy_path_file_tool_loop() {
    let ws = tempdir().unwrap();
    let ws_path = ws.path().to_path_buf();

    fs::write(
        ws_path.join("AGENTS.md"),
        "Rule 1: Use write_file to create files. Use read_file to inspect files.\n\
         Rule 2: Always verify created files before reporting completion.",
    )
    .unwrap();

    let mut session = match create_live_session(&ws_path, false) {
        Some(s) => s,
        None => {
            eprintln!("Skipping test_live_e2e_happy_path_file_tool_loop: no API key found");
            return;
        }
    };

    let prompt = "Create a Python file named calculator.py with a function `def multiply(x, y): return x * y`. \
                  Then read calculator.py to verify its contents, and state the function name defined.";

    let turn_result = session.prompt(prompt).await.expect("Turn should succeed");

    assert_eq!(turn_result.finish_reason, TurnFinishReason::Stop);
    assert!(turn_result.tool_calls_executed >= 1);

    // Verify calculator.py was actually written in the workspace
    let calc_file = ws_path.join("calculator.py");
    assert!(calc_file.is_file(), "calculator.py must exist on disk");
    let content = fs::read_to_string(&calc_file).unwrap();
    assert!(content.contains("def multiply"), "calculator.py content: {content}");

    // Verify model response
    let response = turn_result.final_response.to_lowercase();
    assert!(response.contains("multiply"), "Response should mention multiply: {response}");

    // Verify transcript DAG
    let records = session.transcript().read_branch_records("main").unwrap();
    assert!(records.len() >= 3, "Transcript should contain user, tool calls/results, and assistant");
    assert_eq!(records[0].role, kobold_runtime::Role::User);
    assert!(records.iter().any(|r| r.role == kobold_runtime::Role::Tool));
    assert!(records.last().unwrap().role == kobold_runtime::Role::Assistant);
}

#[tokio::test]
async fn test_live_e2e_happy_path_multi_turn_bash_and_context() {
    let ws = tempdir().unwrap();
    let ws_path = ws.path().to_path_buf();

    let mut session = match create_live_session(&ws_path, false) {
        Some(s) => s,
        None => {
            eprintln!("Skipping test_live_e2e_happy_path_multi_turn_bash_and_context: no API key found");
            return;
        }
    };

    // Turn 1: Execute bash command
    let turn1 = session
        .prompt("Use the bash tool to create a directory named `src_modules` and inside it create an empty file named `mod.rs`. Confirm when done.")
        .await
        .expect("Turn 1 should succeed");

    assert_eq!(turn1.finish_reason, TurnFinishReason::Stop);
    assert!(ws_path.join("src_modules").join("mod.rs").is_file());

    // Turn 2: Follow-up question relying on previous turn memory
    let turn2 = session
        .prompt("What was the exact name of the directory you created in your previous turn? Reply with only the directory name.")
        .await
        .expect("Turn 2 should succeed");

    assert_eq!(turn2.turn_index, 2);
    let reply = turn2.final_response.to_lowercase();
    assert!(
        reply.contains("src_modules"),
        "Turn 2 reply should identify src_modules from conversation history: {reply}"
    );

    // Verify transcript sequence monotonicity
    let records = session.transcript().read_branch_records("main").unwrap();
    for (i, rec) in records.iter().enumerate() {
        assert_eq!(rec.seq, i);
    }
}

// =========================================================================
// Category 2: Error Paths & Self-Correction
// =========================================================================

#[tokio::test]
async fn test_live_e2e_error_path_tool_failure_and_self_correction() {
    let ws = tempdir().unwrap();
    let ws_path = ws.path().to_path_buf();

    // Create the fallback target file
    fs::write(
        ws_path.join("actual_secret.txt"),
        "KOBOLD_SECRET_KEY_99",
    )
    .unwrap();

    let mut session = match create_live_session(&ws_path, false) {
        Some(s) => s,
        None => {
            eprintln!("Skipping test_live_e2e_error_path_tool_failure_and_self_correction: no API key found");
            return;
        }
    };

    let prompt = "First attempt to read `missing_secret.txt` using read_file. When that fails with an error, \
                  read `actual_secret.txt` instead and tell me the secret key.";

    let turn_result = session.prompt(prompt).await.expect("Turn should succeed with self-correction");

    // Live model should have executed at least 2 tool calls (missing_secret -> actual_secret)
    assert!(turn_result.tool_calls_executed >= 2);
    assert!(turn_result.final_response.contains("KOBOLD_SECRET_KEY_99"));

    // Verify that transcript captured both the error and the recovery
    let records = session.transcript().read_branch_records("main").unwrap();
    let tool_results: Vec<_> = records.iter().filter(|r| r.role == kobold_runtime::Role::Tool).collect();
    assert!(tool_results.len() >= 2);
    assert!(
        tool_results.iter().any(|r| r.text.as_deref().unwrap_or("").contains("No such file") || r.text.as_deref().unwrap_or("").contains("error")),
        "Transcript should log tool error feedback"
    );
    assert!(
        tool_results.iter().any(|r| r.text.as_deref().unwrap_or("").contains("KOBOLD_SECRET_KEY_99")),
        "Transcript should log successful recovery"
    );
}

#[tokio::test]
async fn test_live_e2e_error_path_approval_policy_denial() {
    let ws = tempdir().unwrap();
    let ws_path = ws.path().to_path_buf();

    let api_key = match get_test_api_key() {
        Some(k) => k,
        None => return,
    };

    struct DenyDangerousBashPolicy;
    #[async_trait::async_trait]
    impl ApprovalPolicy for DenyDangerousBashPolicy {
        async fn check(&self, call: &ToolCall) -> ApprovalAction {
            if call.name == "bash" && call.arguments.contains("dangerous_wipe") {
                ApprovalAction::Deny {
                    reason: "Security policy: execution of dangerous_wipe is forbidden".into(),
                }
            } else {
                ApprovalAction::Approve
            }
        }
    }

    let mut session = SessionBuilder::new()
        .with_workspace_root(&ws_path)
        .with_openai_backend(api_key, Some("gpt-5.6-luna"), Some("low"))
        .with_policy(Arc::new(DenyDangerousBashPolicy))
        .build()
        .expect("Session should build");

    let prompt = "Run a bash command with `echo dangerous_wipe`. If blocked, explain that it was denied.";
    let turn_result = session.prompt(prompt).await.expect("Session should handle denial gracefully");

    let reply = turn_result.final_response.to_lowercase();
    assert!(
        reply.contains("denied") || reply.contains("forbidden") || reply.contains("policy") || reply.contains("security"),
        "Model should report policy denial to user: {reply}"
    );

    // Verify denial was recorded in transcript
    let records = session.transcript().read_branch_records("main").unwrap();
    let denial_record = records.iter().find(|r| {
        r.role == kobold_runtime::Role::Tool
            && r.text.as_deref().unwrap_or("").contains("Security policy: execution of dangerous_wipe is forbidden")
    });
    assert!(denial_record.is_some(), "Denial explanation must be recorded in transcript");
}

// =========================================================================
// Category 3: Alt / Edge Cases
// =========================================================================

#[tokio::test]
async fn test_live_e2e_edge_case_path_cone_escape_refusal() {
    let ws = tempdir().unwrap();
    let ws_path = ws.path().to_path_buf();

    let mut session = match create_live_session(&ws_path, false) {
        Some(s) => s,
        None => {
            eprintln!("Skipping test_live_e2e_edge_case_path_cone_escape_refusal: no API key found");
            return;
        }
    };

    let prompt = "Use read_file to read the file `../../../../../../etc/passwd`.";
    let turn_result = session.prompt(prompt).await.expect("Turn should complete safely");

    // The tool execution should fail with outside cone refusal, fed to model
    let reply = turn_result.final_response.to_lowercase();
    assert!(
        reply.contains("cone")
            || reply.contains("outside")
            || reply.contains("cannot")
            || reply.contains("refused")
            || reply.contains("error")
            || reply.contains("forbidden"),
        "Model should communicate boundary constraint: {reply}"
    );

    // Verify transcript records cone refusal error
    let records = session.transcript().read_branch_records("main").unwrap();
    assert!(records.iter().any(|r| {
        r.role == kobold_runtime::Role::Tool
            && r.text.as_deref().unwrap_or("").contains("cone")
    }));
}

#[tokio::test]
async fn test_live_e2e_edge_case_zdr_and_checkpoint_rollback() {
    let ws = tempdir().unwrap();
    let ws_path = ws.path().to_path_buf();

    let config_file = ws_path.join("config.ini");
    fs::write(&config_file, "status=green\n").unwrap();

    // Initialize with ZDR: ON
    let mut session = match create_live_session(&ws_path, true) {
        Some(s) => s,
        None => {
            eprintln!("Skipping test_live_e2e_edge_case_zdr_and_checkpoint_rollback: no API key found");
            return;
        }
    };

    // 1. Create baseline APFS CoW checkpoint
    session.create_checkpoint("chk_green").expect("Create checkpoint");

    // 2. Prompt live model to mutate config.ini
    let prompt = "Use write_file to overwrite config.ini with `status=red` precisely.";
    let turn_res = session.prompt(prompt).await.expect("Turn should mutate file");
    assert_eq!(turn_res.finish_reason, TurnFinishReason::Stop);

    // Verify file changed to red
    let current_content = fs::read_to_string(&config_file).unwrap();
    assert!(current_content.contains("status=red"));

    // 3. Rollback workspace to chk_green
    session.rollback_checkpoint("chk_green").expect("Rollback should succeed");

    // Verify file reverted to green
    let reverted_content = fs::read_to_string(&config_file).unwrap();
    assert_eq!(reverted_content, "status=green\n");

    // Verify transcript recorded the mutation turn
    let records = session.transcript().read_branch_records("main").unwrap();
    assert!(records.len() >= 3);
}

#[tokio::test]
async fn test_live_e2e_edge_case_transcript_dag_forking() {
    let ws = tempdir().unwrap();
    let ws_path = ws.path().to_path_buf();

    let mut session = match create_live_session(&ws_path, false) {
        Some(s) => s,
        None => {
            eprintln!("Skipping test_live_e2e_edge_case_transcript_dag_forking: no API key found");
            return;
        }
    };

    // Turn 1 on main branch
    let turn1 = session.prompt("Say the word 'nebula' precisely.").await.unwrap();
    assert!(turn1.final_response.to_lowercase().contains("nebula"));

    // Fork session from main at seq 1 into 'cosmic-branch'
    let mut forked_session = session.fork("cosmic-branch", 1).expect("Fork session");
    assert_eq!(forked_session.branch(), "cosmic-branch");

    // Prompt forked session
    let turn_fork = forked_session
        .prompt("Say the word 'pulsar' precisely.")
        .await
        .unwrap();
    assert!(turn_fork.final_response.to_lowercase().contains("pulsar"));

    // Verify both branch histories in transcript log
    let main_records = session.transcript().read_branch_records("main").unwrap();
    assert_eq!(main_records.len(), 2);
    assert!(main_records[1].text.as_deref().unwrap().to_lowercase().contains("nebula"));

    let fork_records = forked_session.transcript().read_branch_records("cosmic-branch").unwrap();
    assert_eq!(fork_records.len(), 4, "Forked branch should inherit parent 2 records plus its own 2");
    assert!(fork_records[1].text.as_deref().unwrap().to_lowercase().contains("nebula"));
    assert!(fork_records[3].text.as_deref().unwrap().to_lowercase().contains("pulsar"));
}
