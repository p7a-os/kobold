pub mod error;
pub mod orchestrator;
pub mod turn;

pub use error::KernelError;
pub use orchestrator::{Kernel, KernelBuilder};
pub use turn::TurnResult;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use kobold_types::{
        ApprovalAction, ApprovalPolicy, Backend, BackendError, BackendEvent, BackendStream,
        EventSink, EventSinkError, KernelEvent, Message, Role, TokenUsage, Tool, ToolCall,
        ToolDefinition, ToolError, ToolOutput, TurnFinishReason,
    };

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
                return Err(BackendError::Internal("no more mock responses".into()));
            }
            let events = guard.remove(0);
            let stream = futures_util::stream::iter(events.into_iter().map(Ok));
            Ok(Box::pin(stream))
        }
    }

    struct MockTool {
        name: String,
        definition: ToolDefinition,
        calls: std::sync::Mutex<Vec<ToolCall>>,
        handler: Arc<dyn Fn(&ToolCall) -> Result<ToolOutput, ToolError> + Send + Sync>,
    }

    impl MockTool {
        fn new(
            name: &str,
            handler: impl Fn(&ToolCall) -> Result<ToolOutput, ToolError> + Send + Sync + 'static,
        ) -> Self {
            Self {
                name: name.to_string(),
                definition: ToolDefinition::new(name, "Mock tool", serde_json::json!({})),
                calls: std::sync::Mutex::new(Vec::new()),
                handler: Arc::new(handler),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.lock().unwrap().len()
        }
    }

    #[async_trait::async_trait]
    impl Tool for MockTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn definition(&self) -> ToolDefinition {
            self.definition.clone()
        }

        async fn execute(&self, call: &ToolCall) -> Result<ToolOutput, ToolError> {
            self.calls.lock().unwrap().push(call.clone());
            (self.handler)(call)
        }
    }

    struct RecordingEventSink {
        events: std::sync::Mutex<Vec<KernelEvent>>,
    }

    impl RecordingEventSink {
        fn new() -> Self {
            Self {
                events: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn recorded(&self) -> Vec<KernelEvent> {
            self.events.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl EventSink for RecordingEventSink {
        async fn emit(&self, event: KernelEvent) -> Result<(), EventSinkError> {
            self.events.lock().unwrap().push(event);
            Ok(())
        }
    }

    struct DynamicApprovalPolicy {
        decider: Box<dyn Fn(&ToolCall) -> ApprovalAction + Send + Sync>,
    }

    impl DynamicApprovalPolicy {
        fn new(f: impl Fn(&ToolCall) -> ApprovalAction + Send + Sync + 'static) -> Self {
            Self {
                decider: Box::new(f),
            }
        }
    }

    #[async_trait::async_trait]
    impl ApprovalPolicy for DynamicApprovalPolicy {
        async fn check(&self, call: &ToolCall) -> ApprovalAction {
            (self.decider)(call)
        }
    }

    #[tokio::test]
    async fn test_simple_text_turn() {
        let backend_events = vec![vec![
            BackendEvent::ThoughtDelta {
                delta: "thinking deeply...".into(),
            },
            BackendEvent::TextDelta {
                delta: "Hello, ".into(),
            },
            BackendEvent::TextDelta {
                delta: "world!".into(),
            },
            BackendEvent::Finished {
                finish_reason: TurnFinishReason::Stop,
                usage: Some(TokenUsage::new(12, 8)),
            },
        ]];

        let backend = Arc::new(MockBackend::new(backend_events));
        let sink = Arc::new(RecordingEventSink::new());

        let mut kernel = Kernel::builder()
            .with_backend(backend)
            .with_event_sink(sink.clone())
            .build()
            .expect("kernel should build");

        let result = kernel.step("Hi").await.expect("turn should succeed");

        assert_eq!(result.turn_index, 0);
        assert_eq!(result.finish_reason, TurnFinishReason::Stop);
        assert_eq!(result.tool_calls_executed, 0);
        assert_eq!(result.final_response, "Hello, world!");
        assert_eq!(result.thought.as_deref(), Some("thinking deeply..."));
        assert_eq!(result.usage, Some(TokenUsage::new(12, 8)));

        // Verify emitted kernel events
        let events = sink.recorded();
        assert!(matches!(events[0], KernelEvent::TurnStarted { turn_index: 0 }));
        assert!(matches!(&events[1], KernelEvent::ThoughtDelta { delta } if delta == "thinking deeply..."));
        assert!(matches!(&events[2], KernelEvent::TextDelta { delta } if delta == "Hello, "));
        assert!(matches!(&events[3], KernelEvent::TextDelta { delta } if delta == "world!"));
        assert!(matches!(
            &events[4],
            KernelEvent::TurnCompleted {
                finish_reason: TurnFinishReason::Stop,
                usage: Some(_)
            }
        ));

        // Verify conversation history
        assert_eq!(kernel.history().turn_count(), 1);
        assert_eq!(kernel.history().len(), 2);
    }

    #[tokio::test]
    async fn test_tool_execution_loop() {
        let calc_tool = Arc::new(MockTool::new("calc", |call| {
            let input = &call.arguments;
            if input.contains("2+2") {
                Ok(ToolOutput::success(&call.id, "4"))
            } else {
                Ok(ToolOutput::error(&call.id, "unsupported expression"))
            }
        }));

        let backend_events = vec![
            // Step 1: Model requests tool call
            vec![
                BackendEvent::ToolCallComplete {
                    call: ToolCall::new("call_1", "calc", r#"{"expr":"2+2"}"#),
                },
                BackendEvent::Finished {
                    finish_reason: TurnFinishReason::ToolCalls,
                    usage: None,
                },
            ],
            // Step 2: Model receives output and emits answer
            vec![
                BackendEvent::TextDelta {
                    delta: "The answer is 4.".into(),
                },
                BackendEvent::Finished {
                    finish_reason: TurnFinishReason::Stop,
                    usage: Some(TokenUsage::new(30, 10)),
                },
            ],
        ];

        let backend = Arc::new(MockBackend::new(backend_events));
        let sink = Arc::new(RecordingEventSink::new());

        let mut kernel = Kernel::builder()
            .with_backend(backend)
            .with_tool(calc_tool.clone())
            .with_event_sink(sink.clone())
            .build()
            .expect("kernel should build");

        let result = kernel
            .step("Calculate 2+2")
            .await
            .expect("turn should succeed");

        assert_eq!(result.turn_index, 0);
        assert_eq!(result.finish_reason, TurnFinishReason::Stop);
        assert_eq!(result.tool_calls_executed, 1);
        assert_eq!(result.final_response, "The answer is 4.");
        assert_eq!(calc_tool.call_count(), 1);

        // Verify tool events in sink
        let events = sink.recorded();
        let has_tool_start = events
            .iter()
            .any(|e| matches!(e, KernelEvent::ToolExecutionStarted { call } if call.name == "calc"));
        let has_tool_complete = events.iter().any(|e| {
            matches!(e, KernelEvent::ToolExecutionCompleted { output } if output.content == "4")
        });
        assert!(has_tool_start);
        assert!(has_tool_complete);
    }

    #[tokio::test]
    async fn test_self_correction_on_tool_error() {
        let tool = Arc::new(MockTool::new("read_file", |call| {
            if call.arguments.contains("bad.txt") {
                Err(ToolError::Execution("No such file or directory".into()))
            } else {
                Ok(ToolOutput::success(&call.id, "file content recovered"))
            }
        }));

        let backend_events = vec![
            // Step 1: Model calls with non-existent file
            vec![
                BackendEvent::ToolCallComplete {
                    call: ToolCall::new("c1", "read_file", r#"{"path":"bad.txt"}"#),
                },
                BackendEvent::Finished {
                    finish_reason: TurnFinishReason::ToolCalls,
                    usage: None,
                },
            ],
            // Step 2: Model sees error output, self-corrects and calls good.txt
            vec![
                BackendEvent::ToolCallComplete {
                    call: ToolCall::new("c2", "read_file", r#"{"path":"good.txt"}"#),
                },
                BackendEvent::Finished {
                    finish_reason: TurnFinishReason::ToolCalls,
                    usage: None,
                },
            ],
            // Step 3: Model receives good content and concludes
            vec![
                BackendEvent::TextDelta {
                    delta: "Found content successfully.".into(),
                },
                BackendEvent::Finished {
                    finish_reason: TurnFinishReason::Stop,
                    usage: None,
                },
            ],
        ];

        let backend = Arc::new(MockBackend::new(backend_events));

        let mut kernel = Kernel::builder()
            .with_backend(backend)
            .with_tool(tool.clone())
            .build()
            .expect("kernel should build");

        let result = kernel.step("Read config").await.expect("turn should succeed");

        assert_eq!(result.tool_calls_executed, 2);
        assert_eq!(result.final_response, "Found content successfully.");
        assert_eq!(tool.call_count(), 2);

        // Inspect conversation messages:
        // Turn: User -> Assistant(c1) -> ToolOutput(error) -> Assistant(c2) -> ToolOutput(success) -> Assistant("Found content...")
        let msgs = kernel.history().messages();
        assert_eq!(msgs.len(), 6);
        assert_eq!(msgs[2].role, Role::Tool);
        assert!(msgs[2].text().contains("Tool execution error"));
        assert_eq!(msgs[4].role, Role::Tool);
        assert_eq!(msgs[4].text(), "file content recovered");
    }

    #[tokio::test]
    async fn test_approval_policy_denial() {
        let dangerous_tool = Arc::new(MockTool::new("delete_root", |_| {
            panic!("Should never be called when denied by policy");
        }));

        let policy = Arc::new(DynamicApprovalPolicy::new(|call| {
            if call.name == "delete_root" {
                ApprovalAction::Deny {
                    reason: "Deleting root is strictly forbidden".into(),
                }
            } else {
                ApprovalAction::Approve
            }
        }));

        let backend_events = vec![
            // Step 1: Model requests delete_root
            vec![
                BackendEvent::ToolCallComplete {
                    call: ToolCall::new("d1", "delete_root", r#"{"path":"/"}"#),
                },
                BackendEvent::Finished {
                    finish_reason: TurnFinishReason::ToolCalls,
                    usage: None,
                },
            ],
            // Step 2: Model receives policy denial and responds to user
            vec![
                BackendEvent::TextDelta {
                    delta: "I cannot delete root because the action was denied.".into(),
                },
                BackendEvent::Finished {
                    finish_reason: TurnFinishReason::Stop,
                    usage: None,
                },
            ],
        ];

        let backend = Arc::new(MockBackend::new(backend_events));
        let sink = Arc::new(RecordingEventSink::new());

        let mut kernel = Kernel::builder()
            .with_backend(backend)
            .with_tool(dangerous_tool.clone())
            .with_policy(policy)
            .with_event_sink(sink.clone())
            .build()
            .expect("kernel should build");

        let result = kernel.step("Wipe disk").await.expect("turn should handle denial");

        assert_eq!(result.final_response, "I cannot delete root because the action was denied.");
        assert_eq!(dangerous_tool.call_count(), 0);

        let events = sink.recorded();
        let denial_event = events.iter().find(|e| {
            matches!(e, KernelEvent::ApprovalResolved { approved, .. } if !*approved)
        });
        assert!(denial_event.is_some());

        // Verify denial message was fed back to model as tool error output
        let msgs = kernel.history().messages();
        assert_eq!(msgs[2].role, Role::Tool);
        assert!(msgs[2]
            .text()
            .contains("Tool execution denied by policy: Deleting root is strictly forbidden"));
    }

    #[tokio::test]
    async fn test_max_tool_steps_guard() {
        let tool = Arc::new(MockTool::new("infinite_loop", |call| {
            Ok(ToolOutput::success(&call.id, "continue"))
        }));

        // Model returns tool calls on every step
        let backend_events = vec![
            vec![
                BackendEvent::ToolCallComplete {
                    call: ToolCall::new("c1", "infinite_loop", "{}"),
                },
                BackendEvent::Finished {
                    finish_reason: TurnFinishReason::ToolCalls,
                    usage: None,
                },
            ],
            vec![
                BackendEvent::ToolCallComplete {
                    call: ToolCall::new("c2", "infinite_loop", "{}"),
                },
                BackendEvent::Finished {
                    finish_reason: TurnFinishReason::ToolCalls,
                    usage: None,
                },
            ],
            vec![
                BackendEvent::ToolCallComplete {
                    call: ToolCall::new("c3", "infinite_loop", "{}"),
                },
                BackendEvent::Finished {
                    finish_reason: TurnFinishReason::ToolCalls,
                    usage: None,
                },
            ],
            vec![
                BackendEvent::ToolCallComplete {
                    call: ToolCall::new("c4", "infinite_loop", "{}"),
                },
                BackendEvent::Finished {
                    finish_reason: TurnFinishReason::ToolCalls,
                    usage: None,
                },
            ],
        ];

        let backend = Arc::new(MockBackend::new(backend_events));
        let sink = Arc::new(RecordingEventSink::new());

        let mut kernel = Kernel::builder()
            .with_backend(backend)
            .with_tool(tool)
            .with_event_sink(sink.clone())
            .with_max_tool_steps(3)
            .build()
            .expect("kernel should build");

        let err = kernel.step("Run loop").await.expect_err("should exceed max iterations");

        match err {
            KernelError::MaxIterationsExceeded(max) => assert_eq!(max, 3),
            other => panic!("Unexpected error: {:?}", other),
        }

        let events = sink.recorded();
        assert!(events.iter().any(|e| matches!(e, KernelEvent::Error { .. })));
        assert!(events.iter().any(|e| matches!(
            e,
            KernelEvent::TurnCompleted {
                finish_reason: TurnFinishReason::Length,
                ..
            }
        )));
    }
}
