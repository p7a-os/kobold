use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use async_trait::async_trait;
use futures_core::Stream;
use kobold_types::{
    Backend, BackendError, BackendEvent, BackendStream, Message, ToolCall, ToolDefinition,
    TurnFinishReason,
};
use tokio::sync::{mpsc, Mutex};

/// Lightweight Stream adapter for tokio mpsc::Receiver.
pub struct ReceiverStream<T> {
    inner: mpsc::Receiver<T>,
}

impl<T> ReceiverStream<T> {
    pub fn new(inner: mpsc::Receiver<T>) -> Self {
        Self { inner }
    }
}

impl<T> Stream for ReceiverStream<T> {
    type Item = T;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.inner.poll_recv(cx)
    }
}


use crate::config::OpenAiConfig;
use crate::transport::{LiveOpenAiTransport, WebSocketTransport};
use crate::wire::{
    convert_messages_to_input, ReasoningConfig, ResponseCreate, ToolDefinitionWire, WireEvent,
};

/// OpenAI Responses WebSocket backend implementing the `kobold_types::Backend` trait.
pub struct OpenAiBackend {
    config: OpenAiConfig,
    mock_transport: Option<Arc<Mutex<Box<dyn WebSocketTransport>>>>,
}

impl OpenAiBackend {
    pub fn new(config: OpenAiConfig) -> Self {
        Self {
            config,
            mock_transport: None,
        }
    }

    /// Construct with a pre-configured or mock transport for isolated testing.
    pub fn with_transport(
        config: OpenAiConfig,
        transport: Box<dyn WebSocketTransport>,
    ) -> Self {
        Self {
            config,
            mock_transport: Some(Arc::new(Mutex::new(transport))),
        }
    }

    /// Read configuration.
    pub fn config(&self) -> &OpenAiConfig {
        &self.config
    }
}

#[async_trait]
impl Backend for OpenAiBackend {
    async fn stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> Result<BackendStream, BackendError> {
        let tools_wire: Option<Vec<ToolDefinitionWire>> = if tools.is_empty() {
            None
        } else {
            Some(tools.iter().map(ToolDefinitionWire::from_tool_def).collect())
        };

        let reasoning = self.config.reasoning_effort.as_deref().map(|effort| {
            ReasoningConfig {
                effort,
                summary: "auto",
            }
        });

        // In ZDR mode, store is set to false (D-25, D-31)
        let store = !self.config.zdr_enabled;

        let req = ResponseCreate {
            kind: "response.create",
            model: &self.config.model,
            store,
            input: convert_messages_to_input(messages),
            reasoning,
            tools: tools_wire,
        };

        let req_json = serde_json::to_string(&req)
            .map_err(|e| BackendError::Protocol(format!("Failed to serialize request: {e}")))?;

        // Acquire active transport: either mock or fresh live connection
        let mut transport: Box<dyn WebSocketTransport> = match &self.mock_transport {
            Some(arc) => {
                // For mock testing, take ownership or reuse
                let guard = arc.clone();
                let mut guard = guard.lock().await;
                guard.send(&req_json).await?;
                return consume_transport_stream(arc.clone()).await;
            }
            None => {
                let live = LiveOpenAiTransport::connect(
                    &self.config.api_key,
                    self.config.egress_socket.as_deref(),
                )
                .await?;
                Box::new(live)
            }
        };

        transport.send(&req_json).await?;
        let transport_arc = Arc::new(Mutex::new(transport));
        consume_transport_stream(transport_arc).await
    }
}

async fn consume_transport_stream(
    transport_arc: Arc<Mutex<Box<dyn WebSocketTransport>>>,
) -> Result<BackendStream, BackendError> {
    let (tx, rx) = mpsc::channel(64);

    tokio::spawn(async move {
        let mut has_tool_calls = false;

        loop {
            let next_msg = {
                let mut guard = transport_arc.lock().await;
                guard.next_message().await
            };

            let text = match next_msg {
                Ok(Some(msg)) => msg,
                Ok(None) => break,
                Err(err) => {
                    let _ = tx.send(Err(err)).await;
                    break;
                }
            };

            if text.trim().is_empty() {
                continue;
            }

            let event: WireEvent = match serde_json::from_str(&text) {
                Ok(e) => e,
                Err(err) => {
                    let _ = tx.send(Err(BackendError::Protocol(format!(
                        "Failed to parse wire event JSON: {err} in text: {text}"
                    )))).await;
                    break;
                }
            };

            match event.kind.as_str() {
                "response.output_text.delta" => {
                    if let Some(delta) = event.delta {
                        let _ = tx.send(Ok(BackendEvent::TextDelta { delta })).await;
                    }
                }
                "response.reasoning_summary.delta" | "response.reasoning.delta" => {
                    if let Some(delta) = event.delta {
                        let _ = tx.send(Ok(BackendEvent::ThoughtDelta { delta })).await;
                    }
                }
                "response.function_call_arguments.delta" => {
                    has_tool_calls = true;
                    let _ = tx.send(Ok(BackendEvent::ToolCallChunk {
                        index: 0,
                        id: event.call_id,
                        name: event.name,
                        arguments_delta: event.delta.unwrap_or_default(),
                    })).await;
                }
                "response.function_call_arguments.done" => {
                    // Argument streaming completed; the complete call is emitted by response.output_item.done
                    has_tool_calls = true;
                }
                "response.output_item.done" => {
                    if let Some(item) = event.item {
                        if item.kind.as_deref() == Some("function_call") {
                            has_tool_calls = true;
                            let call_id = item.call_id.or(item.id).unwrap_or_default();
                            let name = item.name.unwrap_or_default();
                            let arguments = item.arguments.unwrap_or_default();
                            let _ = tx.send(Ok(BackendEvent::ToolCallComplete {
                                call: ToolCall::new(call_id, name, arguments),
                            })).await;
                        }
                    }
                }
                "response.completed" => {
                    let usage = event.response.as_ref().and_then(|r| r.usage.as_ref()).map(|u| u.to_token_usage());
                    let finish_reason = if has_tool_calls {
                        TurnFinishReason::ToolCalls
                    } else {
                        TurnFinishReason::Stop
                    };
                    let _ = tx.send(Ok(BackendEvent::Finished {
                        finish_reason,
                        usage,
                    })).await;
                    break;
                }
                "response.failed" | "error" => {
                    let err_msg = event
                        .error
                        .and_then(|e| e.message)
                        .unwrap_or_else(|| "Unknown API error from OpenAI".to_string());
                    let _ = tx.send(Err(BackendError::Internal(err_msg))).await;
                    break;
                }
                _ => {
                    // Ignore other non-essential notifications
                }
            }
        }
    });

    let receiver_stream = ReceiverStream::new(rx);
    Ok(Box::pin(receiver_stream))
}
