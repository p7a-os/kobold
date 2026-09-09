//! In-process agent session runtime, transcript persistence, and workspace supervisor SDK for Kobold.

pub mod builder;
pub mod error;
pub mod session;
pub mod transcript;

pub use builder::SessionBuilder;
pub use error::RuntimeError;
pub use session::AgentSession;
pub use transcript::{TranscriptLog, TranscriptRecord};

// Re-export common types for SDK consumers
pub use kobold_context::budget::TokenBudget;
pub use kobold_context::history::ConversationHistory;
pub use kobold_kernel::TurnResult;
pub use kobold_tool_bash::sandbox::SandboxMode;
pub use kobold_types::{
    ApprovalAction, ApprovalPolicy, Backend, BackendError, BackendEvent, BackendStream, EventSink,
    EventSinkError, KernelEvent, Message, Role, TokenUsage, Tool, ToolCall, ToolDefinition,
    ToolError, ToolOutput, TurnFinishReason,
};

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Arc;
    use tempfile::tempdir;

    struct MockBackend {
        responses: std::sync::Mutex<Vec<Vec<BackendEvent>>>,
    }

    impl MockBackend {
        fn new(responses: Vec<Vec<BackendEvent>>) -> Self {
            Self {
                responses: std::sync::Mutex::new(responses),
            }
        }
    }

    #[async_trait::async_trait]
    impl Backend for MockBackend {
        async fn stream(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
        ) -> Result<BackendStream, BackendError> {
            let mut guard = self.responses.lock().unwrap();
            if guard.is_empty() {
                return Err(BackendError::Internal("No more mock responses".into()));
            }
            let events = guard.remove(0);
            let stream = futures_util::stream::iter(events.into_iter().map(Ok));
            Ok(Box::pin(stream))
        }
    }

    #[tokio::test]
    async fn test_session_builder_defaults_and_agents_md() {
        let ws = tempdir().unwrap();
        let ws_path = ws.path().to_path_buf();

        // Create AGENTS.md and CLAUDE.md
        fs::write(ws_path.join("AGENTS.md"), "Rule 1: Always verify.").unwrap();
        fs::write(ws_path.join("CLAUDE.md"), "Deprecated claude instructions.").unwrap();

        let backend = Arc::new(MockBackend::new(vec![]));

        let session = SessionBuilder::new()
            .with_workspace_root(&ws_path)
            .with_backend(backend)
            .with_session_id("sess_test_01")
            .with_branch("feature/auth")
            .build()
            .expect("Session should assemble successfully");

        assert_eq!(session.session_id(), "sess_test_01");
        assert_eq!(session.branch(), "feature/auth");
        assert_eq!(session.history().turn_count(), 1);

        // Verify AGENTS.md is loaded in Turn 0 and CLAUDE.md is strictly excluded
        let system_msg = &session.history().messages()[0];
        let text = system_msg.text();
        assert!(text.contains("Rule 1: Always verify."));
        assert!(!text.contains("Deprecated claude instructions."));
    }

    #[tokio::test]
    async fn test_session_prompt_and_transcript_persistence() {
        let ws = tempdir().unwrap();
        let ws_path = ws.path().to_path_buf();

        let backend_events = vec![vec![
            BackendEvent::ThoughtDelta {
                delta: "thinking about authentication...".into(),
            },
            BackendEvent::TextDelta {
                delta: "Authentication module initialized.".into(),
            },
            BackendEvent::Finished {
                finish_reason: TurnFinishReason::Stop,
                usage: Some(TokenUsage::new(45, 20)),
            },
        ]];

        let backend = Arc::new(MockBackend::new(backend_events));

        let mut session = SessionBuilder::new()
            .with_workspace_root(&ws_path)
            .with_backend(backend)
            .build()
            .expect("Session should build");

        let turn_result = session
            .prompt("Initialize the auth module.")
            .await
            .expect("Turn should succeed");

        assert_eq!(turn_result.turn_index, 1);
        assert_eq!(turn_result.final_response, "Authentication module initialized.");
        assert_eq!(turn_result.usage, Some(TokenUsage::new(45, 20)));

        // Verify transcript records on disk
        let transcript_file = ws_path.join(".kobold").join("transcript.jsonl");
        assert!(transcript_file.exists());

        let log = session.transcript();
        let records = log.read_branch_records("main").expect("Read branch records");

        // Should have:
        // Record 0: User ("Initialize the auth module.")
        // Record 1: Assistant ("Authentication module initialized.")
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].role, Role::User);
        assert_eq!(records[0].text.as_deref(), Some("Initialize the auth module."));
        assert_eq!(records[0].seq, 0);

        assert_eq!(records[1].role, Role::Assistant);
        assert_eq!(records[1].text.as_deref(), Some("Authentication module initialized."));
        assert_eq!(records[1].seq, 1);
        assert_eq!(records[1].finish_reason, Some(TurnFinishReason::Stop));
        assert_eq!(records[1].usage, Some(TokenUsage::new(45, 20)));

        // Verify user input history extraction
        let history = log.user_history().expect("User history");
        assert_eq!(history, vec!["Initialize the auth module.".to_string()]);
    }

    #[tokio::test]
    async fn test_session_fs_tool_execution_and_transcript() {
        let ws = tempdir().unwrap();
        let ws_path = ws.path().to_path_buf();

        let backend_events = vec![
            // Step 1: Model requests write_file
            vec![
                BackendEvent::ToolCallComplete {
                    call: ToolCall::new(
                        "c_write_1",
                        "write_file",
                        serde_json::json!({
                            "path": "hello.txt",
                            "content": "Hello, Kobold runtime!"
                        })
                        .to_string(),
                    ),
                },
                BackendEvent::Finished {
                    finish_reason: TurnFinishReason::ToolCalls,
                    usage: None,
                },
            ],
            // Step 2: Model receives tool confirmation and finishes
            vec![
                BackendEvent::TextDelta {
                    delta: "File written successfully.".into(),
                },
                BackendEvent::Finished {
                    finish_reason: TurnFinishReason::Stop,
                    usage: Some(TokenUsage::new(50, 15)),
                },
            ],
        ];

        let backend = Arc::new(MockBackend::new(backend_events));

        let mut session = SessionBuilder::new()
            .with_workspace_root(&ws_path)
            .with_backend(backend)
            .build()
            .expect("Session should build");

        let turn_result = session
            .prompt("Create hello.txt")
            .await
            .expect("Prompt should execute write_file");

        assert_eq!(turn_result.tool_calls_executed, 1);
        assert_eq!(turn_result.final_response, "File written successfully.");

        // Verify file was written to disk inside workspace cone
        let created_file = ws_path.join("hello.txt");
        assert!(created_file.is_file());
        assert_eq!(
            fs::read_to_string(&created_file).unwrap(),
            "Hello, Kobold runtime!"
        );

        // Verify transcript captured user, assistant tool-call, tool output, and final assistant message
        let log = session.transcript();
        let records = log.read_branch_records("main").expect("Read records");
        assert_eq!(records.len(), 4);

        assert_eq!(records[0].role, Role::User);
        assert_eq!(records[1].role, Role::Assistant);
        assert_eq!(records[1].tool_calls.len(), 1);
        assert_eq!(records[1].tool_calls[0].name, "write_file");

        assert_eq!(records[2].role, Role::Tool);
        assert_eq!(records[2].tool_call_id.as_deref(), Some("c_write_1"));
        assert!(records[2].text.as_deref().unwrap().contains("Successfully wrote 22 bytes"));

        assert_eq!(records[3].role, Role::Assistant);
        assert_eq!(records[3].text.as_deref(), Some("File written successfully."));
    }

    #[tokio::test]
    async fn test_transcript_dag_branching() {
        let ws = tempdir().unwrap();
        let ws_path = ws.path().to_path_buf();
        let log = TranscriptLog::open_default(&ws_path).unwrap();

        // 1. Write records to 'main'
        let rec0 = TranscriptRecord::from_message(
            "s1",
            "main",
            0,
            None,
            &Message::user("Task 1"),
            None,
            None,
        );
        let rec1 = TranscriptRecord::from_message(
            "s1",
            "main",
            1,
            None,
            &Message::assistant("Reply 1"),
            Some(TurnFinishReason::Stop),
            None,
        );
        log.append(&rec0).unwrap();
        log.append(&rec1).unwrap();

        // 2. Fork 'fork-A' from 'main' at seq 1
        let rec_fork0 = TranscriptRecord::from_message(
            "s1",
            "fork-A",
            0,
            Some(("main", 1)),
            &Message::user("Forked task"),
            None,
            None,
        );
        let rec_fork1 = TranscriptRecord::from_message(
            "s1",
            "fork-A",
            1,
            None,
            &Message::assistant("Forked reply"),
            Some(TurnFinishReason::Stop),
            None,
        );
        log.append(&rec_fork0).unwrap();
        log.append(&rec_fork1).unwrap();

        // 3. Verify main branch chain
        let main_recs = log.read_branch_records("main").unwrap();
        assert_eq!(main_recs.len(), 2);
        assert_eq!(main_recs[0].text.as_deref(), Some("Task 1"));
        assert_eq!(main_recs[1].text.as_deref(), Some("Reply 1"));

        // 4. Verify fork-A branch chain inherits main[0..=1] then fork-A[0..=1]
        let fork_recs = log.read_branch_records("fork-A").unwrap();
        assert_eq!(fork_recs.len(), 4);
        assert_eq!(fork_recs[0].text.as_deref(), Some("Task 1"));
        assert_eq!(fork_recs[1].text.as_deref(), Some("Reply 1"));
        assert_eq!(fork_recs[2].text.as_deref(), Some("Forked task"));
        assert_eq!(fork_recs[3].text.as_deref(), Some("Forked reply"));

        // 5. Verify replaying to ConversationHistory
        let replayed_hist = log.replay_to_history("fork-A").unwrap();
        assert_eq!(replayed_hist.len(), 4);
    }

    #[tokio::test]
    async fn test_session_multi_turn_continuation() {
        let ws = tempdir().unwrap();
        let ws_path = ws.path().to_path_buf();

        let backend_events = vec![
            // Turn 1 response
            vec![
                BackendEvent::TextDelta {
                    delta: "Answer to turn 1.".into(),
                },
                BackendEvent::Finished {
                    finish_reason: TurnFinishReason::Stop,
                    usage: Some(TokenUsage::new(20, 10)),
                },
            ],
            // Turn 2 response
            vec![
                BackendEvent::TextDelta {
                    delta: "Answer to turn 2.".into(),
                },
                BackendEvent::Finished {
                    finish_reason: TurnFinishReason::Stop,
                    usage: Some(TokenUsage::new(35, 12)),
                },
            ],
        ];

        let backend = Arc::new(MockBackend::new(backend_events));

        let mut session = SessionBuilder::new()
            .with_workspace_root(&ws_path)
            .with_backend(backend)
            .build()
            .expect("Session should build");

        let turn1 = session.prompt("Question 1").await.unwrap();
        assert_eq!(turn1.final_response, "Answer to turn 1.");
        assert_eq!(turn1.turn_index, 1);

        let turn2 = session.prompt("Question 2").await.unwrap();
        assert_eq!(turn2.final_response, "Answer to turn 2.");
        assert_eq!(turn2.turn_index, 2);

        // Verify transcript has 4 records with sequential indices: 0, 1, 2, 3
        let records = session.transcript().read_branch_records("main").unwrap();
        assert_eq!(records.len(), 4);
        assert_eq!(records[0].seq, 0);
        assert_eq!(records[0].role, Role::User);
        assert_eq!(records[0].text.as_deref(), Some("Question 1"));

        assert_eq!(records[1].seq, 1);
        assert_eq!(records[1].role, Role::Assistant);
        assert_eq!(records[1].text.as_deref(), Some("Answer to turn 1."));

        assert_eq!(records[2].seq, 2);
        assert_eq!(records[2].role, Role::User);
        assert_eq!(records[2].text.as_deref(), Some("Question 2"));

        assert_eq!(records[3].seq, 3);
        assert_eq!(records[3].role, Role::Assistant);
        assert_eq!(records[3].text.as_deref(), Some("Answer to turn 2."));
    }

    #[tokio::test]
    async fn test_session_checkpoint_and_rollback() {
        let ws = tempdir().unwrap();
        let ws_path = ws.path().to_path_buf();

        let test_file = ws_path.join("data.txt");
        fs::write(&test_file, "Initial version").unwrap();

        let backend = Arc::new(MockBackend::new(vec![]));

        let session = SessionBuilder::new()
            .with_workspace_root(&ws_path)
            .with_backend(backend)
            .build()
            .expect("Session should build");

        // Create checkpoint
        let cp_path = session.create_checkpoint("cp_baseline").expect("Create checkpoint");
        assert!(cp_path.exists());

        // Modify workspace file
        fs::write(&test_file, "Corrupted content").unwrap();
        assert_eq!(fs::read_to_string(&test_file).unwrap(), "Corrupted content");

        // Rollback to checkpoint
        session.rollback_checkpoint("cp_baseline").expect("Rollback checkpoint");
        assert_eq!(fs::read_to_string(&test_file).unwrap(), "Initial version");
    }

    #[tokio::test]
    async fn test_session_with_approval_policy_denial() {
        let ws = tempdir().unwrap();
        let ws_path = ws.path().to_path_buf();

        struct DenyAllPolicy;
        #[async_trait::async_trait]
        impl ApprovalPolicy for DenyAllPolicy {
            async fn check(&self, _call: &ToolCall) -> ApprovalAction {
                ApprovalAction::Deny {
                    reason: "Safety policy denied tool invocation".into(),
                }
            }
        }

        let backend_events = vec![
            // Step 1: Model requests write_file
            vec![
                BackendEvent::ToolCallComplete {
                    call: ToolCall::new("call_deny", "write_file", r#"{"path":"bad.txt","content":"x"}"#),
                },
                BackendEvent::Finished {
                    finish_reason: TurnFinishReason::ToolCalls,
                    usage: None,
                },
            ],
            // Step 2: Model receives denial feedback and acknowledges
            vec![
                BackendEvent::TextDelta {
                    delta: "I could not write the file because it was denied.".into(),
                },
                BackendEvent::Finished {
                    finish_reason: TurnFinishReason::Stop,
                    usage: None,
                },
            ],
        ];

        let backend = Arc::new(MockBackend::new(backend_events));

        let mut session = SessionBuilder::new()
            .with_workspace_root(&ws_path)
            .with_backend(backend)
            .with_policy(Arc::new(DenyAllPolicy))
            .build()
            .expect("Session should build");

        let res = session.prompt("Write bad.txt").await.unwrap();
        assert_eq!(res.final_response, "I could not write the file because it was denied.");
        assert_eq!(res.tool_calls_executed, 0);

        // Verify the file was NEVER created
        assert!(!ws_path.join("bad.txt").exists());

        // Verify transcript recorded the denial as tool result
        let records = session.transcript().read_branch_records("main").unwrap();
        assert_eq!(records.len(), 4);
        assert_eq!(records[2].role, Role::Tool);
        assert!(records[2].text.as_deref().unwrap().contains("Safety policy denied tool invocation"));
    }

    #[tokio::test]
    async fn test_live_session_with_openai_backend() {
        let api_key = match std::env::var("LLM_API_KEY").or_else(|_| std::env::var("OPENAI_API_KEY")) {
            Ok(k) if !k.is_empty() => k,
            _ => return,
        };

        let ws = tempdir().unwrap();
        let ws_path = ws.path().to_path_buf();

        fs::write(
            ws_path.join("AGENTS.md"),
            "Rule: Always be concise. Answer in a single word.",
        )
        .unwrap();

        let mut session = SessionBuilder::new()
            .with_workspace_root(&ws_path)
            .with_openai_backend(api_key, Some("gpt-5.6-luna"), Some("low"))
            .with_zdr(true)
            .build()
            .expect("Session should assemble with live backend");

        let turn_result = session
            .prompt("What is 6 multiplied by 7? Reply with only the number.")
            .await
            .expect("Live turn should succeed");

        assert!(turn_result.final_response.contains("42"));

        // Verify transcript was written to disk
        let records = session.transcript().read_branch_records("main").unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].role, Role::User);
        assert_eq!(records[1].role, Role::Assistant);
        assert!(records[1].text.as_deref().unwrap().contains("42"));
    }
}

