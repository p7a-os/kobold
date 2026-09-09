pub mod backend;
pub mod config;
pub mod transport;
pub mod wire;

pub use backend::OpenAiBackend;
pub use config::{OpenAiConfig, DEFAULT_MODEL, DEFAULT_OPENAI_WS_URL};
pub use transport::{LiveOpenAiTransport, MockWebSocketTransport, WebSocketTransport};

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;
    use kobold_types::{Backend, BackendEvent, Message, ToolCall, ToolDefinition, TurnFinishReason};

    #[test]
    fn test_zdr_flag_sets_store_false() {
        let config_zdr = OpenAiConfig::new("test-key").with_zdr(true);
        assert!(config_zdr.zdr_enabled);

        let config_normal = OpenAiConfig::new("test-key").with_zdr(false);
        assert!(!config_normal.zdr_enabled);
    }

    #[test]
    fn test_assistant_output_text_and_user_input_text() {
        let msgs = vec![
            Message::system("You are a helpful assistant."),
            Message::user("Hello!"),
            Message::assistant("Hi there!"),
            Message::assistant_tool_calls(vec![ToolCall::new("c1", "calc", r#"{"x":1}"#)]),
            Message::tool_result("c1", "result is 2"),
        ];

        let input_items = wire::convert_messages_to_input(&msgs);
        assert_eq!(input_items.len(), 5);

        let json_val = serde_json::to_value(&input_items).unwrap();
        let arr = json_val.as_array().unwrap();

        // System message -> input_text
        assert_eq!(arr[0]["type"], "message");
        assert_eq!(arr[0]["role"], "system");
        assert_eq!(arr[0]["content"][0]["type"], "input_text");

        // User message -> input_text
        assert_eq!(arr[1]["type"], "message");
        assert_eq!(arr[1]["role"], "user");
        assert_eq!(arr[1]["content"][0]["type"], "input_text");

        // Assistant message -> output_text
        assert_eq!(arr[2]["type"], "message");
        assert_eq!(arr[2]["role"], "assistant");
        assert_eq!(arr[2]["content"][0]["type"], "output_text");

        // Assistant tool call -> function_call
        assert_eq!(arr[3]["type"], "function_call");
        assert_eq!(arr[3]["call_id"], "c1");
        assert_eq!(arr[3]["name"], "calc");

        // Tool output -> function_call_output
        assert_eq!(arr[4]["type"], "function_call_output");
        assert_eq!(arr[4]["call_id"], "c1");
        assert_eq!(arr[4]["output"], "result is 2");
    }

    #[tokio::test]
    async fn test_mock_backend_stream_text_and_thought() {
        let mock_events = vec![
            serde_json::json!({
                "type": "response.reasoning_summary.delta",
                "delta": "Thinking through the steps..."
            }).to_string(),
            serde_json::json!({
                "type": "response.output_text.delta",
                "delta": "Hello "
            }).to_string(),
            serde_json::json!({
                "type": "response.output_text.delta",
                "delta": "world!"
            }).to_string(),
            serde_json::json!({
                "type": "response.completed",
                "response": {
                    "id": "resp_1",
                    "status": "completed",
                    "usage": {
                        "input_tokens": 15,
                        "output_tokens": 10,
                        "total_tokens": 25
                    }
                }
            }).to_string(),
        ];

        let mock_transport = Box::new(MockWebSocketTransport::new(mock_events));
        let config = OpenAiConfig::new("test-key").with_zdr(true);
        let backend = OpenAiBackend::with_transport(config, mock_transport);

        let messages = vec![Message::user("Hello")];
        let mut stream = backend.stream(&messages, &[]).await.expect("stream should open");

        let mut received = Vec::new();
        while let Some(event_res) = stream.next().await {
            received.push(event_res.expect("event should be Ok"));
        }

        assert_eq!(received.len(), 4);
        assert!(matches!(&received[0], BackendEvent::ThoughtDelta { delta } if delta == "Thinking through the steps..."));
        assert!(matches!(&received[1], BackendEvent::TextDelta { delta } if delta == "Hello "));
        assert!(matches!(&received[2], BackendEvent::TextDelta { delta } if delta == "world!"));
        assert!(matches!(&received[3], BackendEvent::Finished { finish_reason, usage } if *finish_reason == TurnFinishReason::Stop && usage.unwrap().total_tokens == 25));
    }

    #[tokio::test]
    async fn test_mock_backend_stream_tool_calls() {
        let mock_events = vec![
            serde_json::json!({
                "type": "response.function_call_arguments.delta",
                "call_id": "call_abc",
                "name": "bash",
                "delta": "{\"command\": \"ls\"}"
            }).to_string(),
            serde_json::json!({
                "type": "response.function_call_arguments.done",
                "call_id": "call_abc",
                "name": "bash",
                "arguments": "{\"command\": \"ls\"}"
            }).to_string(),
            serde_json::json!({
                "type": "response.completed",
                "response": {
                    "id": "resp_2",
                    "status": "completed",
                    "usage": {
                        "input_tokens": 20,
                        "output_tokens": 8,
                        "total_tokens": 28
                    }
                }
            }).to_string(),
        ];

        let mock_transport = Box::new(MockWebSocketTransport::new(mock_events));
        let config = OpenAiConfig::new("test-key");
        let backend = OpenAiBackend::with_transport(config, mock_transport);

        let messages = vec![Message::user("Run ls")];
        let tools = vec![ToolDefinition::new("bash", "Run command", serde_json::json!({}))];
        let mut stream = backend.stream(&messages, &tools).await.expect("stream should open");

        let mut received = Vec::new();
        while let Some(event_res) = stream.next().await {
            received.push(event_res.expect("event should be Ok"));
        }

        assert_eq!(received.len(), 3);
        assert!(matches!(&received[0], BackendEvent::ToolCallChunk { .. }));
        assert!(matches!(&received[1], BackendEvent::ToolCallComplete { call } if call.name == "bash" && call.id == "call_abc"));
        assert!(matches!(&received[2], BackendEvent::Finished { finish_reason, .. } if *finish_reason == TurnFinishReason::ToolCalls));
    }

    #[tokio::test]
    async fn test_mock_backend_error_handling() {
        let mock_events = vec![
            serde_json::json!({
                "type": "error",
                "error": {
                    "code": "rate_limit_exceeded",
                    "message": "Too many requests"
                }
            }).to_string(),
        ];

        let mock_transport = Box::new(MockWebSocketTransport::new(mock_events));
        let config = OpenAiConfig::new("test-key");
        let backend = OpenAiBackend::with_transport(config, mock_transport);

        let messages = vec![Message::user("Hi")];
        let mut stream = backend.stream(&messages, &[]).await.expect("stream should open");

        let first = stream.next().await.expect("should yield event");
        assert!(first.is_err());
        let err = first.unwrap_err();
        assert!(err.to_string().contains("Too many requests"));
    }

    #[tokio::test]
    #[ignore = "requires live network and LLM_API_KEY"]
    async fn test_live_openai_stream() {
        let api_key = match std::env::var("LLM_API_KEY") {
            Ok(k) if !k.is_empty() => k,
            _ => return,
        };

        let config = OpenAiConfig::new(api_key)
            .with_model("gpt-5.6-luna")
            .with_reasoning_effort("low")
            .with_zdr(true);
        let backend = OpenAiBackend::new(config);

        let messages = vec![Message::user("Say the word 'kobold' exactly.")];
        let mut stream = backend.stream(&messages, &[]).await.expect("live stream should open");

        let mut received_text = String::new();
        while let Some(event_res) = stream.next().await {
            let event = event_res.expect("live event should be Ok");
            if let BackendEvent::TextDelta { delta } = event {
                received_text.push_str(&delta);
            }
        }
        assert!(received_text.to_lowercase().contains("kobold"));
    }
}

