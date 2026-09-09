use std::collections::HashMap;
use std::sync::Arc;

use futures_util::StreamExt;
use kobold_context::budget::{TokenBudget, TokenEstimator};
use kobold_context::compact::compact_context;
use kobold_context::history::ConversationHistory;
use kobold_types::{
    ApprovalAction, ApprovalPolicy, Backend, BackendEvent, ContentPart, EventSink, KernelEvent,
    Message, Tool, ToolCall, ToolDefinition, ToolOutput, TurnFinishReason,
};

use crate::error::KernelError;
use crate::turn::TurnResult;

/// Primary async turn orchestrator driving the Kobold agentic loop.
pub struct Kernel {
    backend: Arc<dyn Backend>,
    tools: HashMap<String, Arc<dyn Tool>>,
    history: ConversationHistory,
    budget: TokenBudget,
    estimator: TokenEstimator,
    policy: Arc<dyn ApprovalPolicy>,
    event_sink: Arc<dyn EventSink>,
    max_tool_steps: usize,
}

impl Kernel {
    pub fn builder() -> KernelBuilder {
        KernelBuilder::default()
    }

    /// Read-only reference to the active conversation history.
    pub fn history(&self) -> &ConversationHistory {
        &self.history
    }

    /// Mutable reference to conversation history.
    pub fn history_mut(&mut self) -> &mut ConversationHistory {
        &mut self.history
    }

    /// Token budget configuration.
    pub fn budget(&self) -> &TokenBudget {
        &self.budget
    }

    /// Hybrid token estimator.
    pub fn estimator(&self) -> &TokenEstimator {
        &self.estimator
    }

    /// Registered tool map.
    pub fn tools(&self) -> &HashMap<String, Arc<dyn Tool>> {
        &self.tools
    }

    /// Execute a complete conversational turn, processing user input through inference,
    /// approval checks, sequential tool dispatch, and error feedback until completion.
    pub async fn step(&mut self, user_prompt: &str) -> Result<TurnResult, KernelError> {
        let turn_index = self.history.turn_count();
        self.history.append_user(user_prompt);

        self.event_sink
            .emit(KernelEvent::TurnStarted { turn_index })
            .await?;

        let mut tool_calls_executed = 0;
        let mut last_text = String::new();
        let mut last_thought = None;
        let mut last_usage = None;
        let mut step_count = 0;

        loop {
            step_count += 1;
            if step_count > self.max_tool_steps {
                let err = KernelError::MaxIterationsExceeded(self.max_tool_steps);
                self.event_sink
                    .emit(KernelEvent::Error {
                        error: err.to_string(),
                    })
                    .await?;
                self.event_sink
                    .emit(KernelEvent::TurnCompleted {
                        finish_reason: TurnFinishReason::Length,
                        usage: last_usage,
                    })
                    .await?;
                return Err(err);
            }

            // 1. Pre-flight context compaction
            compact_context(&mut self.history, &self.budget, &mut self.estimator);

            // 2. Collect tool definitions advertised to the model
            let tool_defs: Vec<ToolDefinition> =
                self.tools.values().map(|t| t.definition()).collect();

            // 3. Initiate backend streaming
            let mut stream = self
                .backend
                .stream(self.history.messages(), &tool_defs)
                .await?;

            let mut step_text = String::new();
            let mut step_thought = String::new();
            let mut requested_tool_calls: Vec<ToolCall> = Vec::new();
            let mut step_finish_reason = None;

            // 4. Consume backend streaming events
            while let Some(event_res) = stream.next().await {
                let event = event_res?;
                match event {
                    BackendEvent::TextDelta { delta } => {
                        step_text.push_str(&delta);
                        self.event_sink
                            .emit(KernelEvent::TextDelta { delta })
                            .await?;
                    }
                    BackendEvent::ThoughtDelta { delta } => {
                        step_thought.push_str(&delta);
                        self.event_sink
                            .emit(KernelEvent::ThoughtDelta { delta })
                            .await?;
                    }
                    BackendEvent::ToolCallChunk { .. } => {
                        // Incremental chunks aggregated by backend before ToolCallComplete
                    }
                    BackendEvent::ToolCallComplete { call } => {
                        requested_tool_calls.push(call);
                    }
                    BackendEvent::Finished {
                        finish_reason,
                        usage,
                    } => {
                        step_finish_reason = Some(finish_reason);
                        if let Some(u) = usage {
                            last_usage = Some(u);
                            self.estimator.reconcile(self.history.len(), u);
                        }
                    }
                }
            }

            if !step_text.is_empty() {
                last_text = step_text.clone();
            }
            if !step_thought.is_empty() {
                last_thought = Some(step_thought.clone());
            }

            // 5. Append model assistant message to conversation history
            let assistant_msg = if !requested_tool_calls.is_empty() {
                let mut msg = Message::assistant_tool_calls(requested_tool_calls.clone());
                if !step_thought.is_empty() {
                    msg.content.push(ContentPart::thought(step_thought));
                }
                if !step_text.is_empty() {
                    msg.content.push(ContentPart::text(step_text));
                }
                msg
            } else if !step_thought.is_empty() {
                Message::assistant_with_thought(step_thought, step_text)
            } else {
                Message::assistant(step_text)
            };
            self.history.append_message(assistant_msg);

            // 6. If no tool calls were requested, the model concluded its turn
            if requested_tool_calls.is_empty() {
                let finish_reason = step_finish_reason.unwrap_or(TurnFinishReason::Stop);
                self.event_sink
                    .emit(KernelEvent::TurnCompleted {
                        finish_reason: finish_reason.clone(),
                        usage: last_usage,
                    })
                    .await?;

                return Ok(TurnResult {
                    turn_index,
                    finish_reason,
                    tool_calls_executed,
                    usage: last_usage,
                    final_response: last_text,
                    thought: last_thought,
                });
            }

            // 7. Sequential tool execution (D-15) mediated by ApprovalPolicy (D-14)
            for call in &requested_tool_calls {
                let action = self.policy.check(call).await;
                match action {
                    ApprovalAction::Approve => {
                        self.event_sink
                            .emit(KernelEvent::ApprovalResolved {
                                call_id: call.id.clone(),
                                approved: true,
                            })
                            .await?;
                    }
                    ApprovalAction::Deny { reason } => {
                        self.event_sink
                            .emit(KernelEvent::ApprovalResolved {
                                call_id: call.id.clone(),
                                approved: false,
                            })
                            .await?;
                        // Return policy denial as error output to the model (D-16)
                        let output = ToolOutput::error(
                            &call.id,
                            format!("Tool execution denied by policy: {}", reason),
                        );
                        self.history.append_tool_output(&output);
                        continue;
                    }
                    ApprovalAction::NeedUserApproval { reason } => {
                        self.event_sink
                            .emit(KernelEvent::ApprovalRequested {
                                call: call.clone(),
                                reason: reason.clone(),
                            })
                            .await?;
                        // If policy indicated approval required but denied
                        let output = ToolOutput::error(
                            &call.id,
                            format!("Execution paused: user approval required ({})", reason),
                        );
                        self.history.append_tool_output(&output);
                        continue;
                    }
                }

                // Execute the tool and capture result
                self.event_sink
                    .emit(KernelEvent::ToolExecutionStarted { call: call.clone() })
                    .await?;

                let output = match self.tools.get(&call.name) {
                    Some(tool) => match tool.execute(call).await {
                        Ok(out) => out,
                        Err(err) => {
                            // Non-zero exits or tool execution failures returned for self-correction (D-16)
                            ToolOutput::error(&call.id, format!("Tool execution error: {}", err))
                        }
                    },
                    None => ToolOutput::error(&call.id, format!("Tool not found: {}", call.name)),
                };

                self.event_sink
                    .emit(KernelEvent::ToolExecutionCompleted {
                        output: output.clone(),
                    })
                    .await?;

                self.history.append_tool_output(&output);
                tool_calls_executed += 1;
            }

            // Loop continues: next iteration prompts the backend with newly accumulated tool outputs
        }
    }
}

/// Fluent builder for constructing a Kernel instance.
#[derive(Default)]
pub struct KernelBuilder {
    backend: Option<Arc<dyn Backend>>,
    tools: Vec<Arc<dyn Tool>>,
    history: Option<ConversationHistory>,
    budget: Option<TokenBudget>,
    estimator: Option<TokenEstimator>,
    policy: Option<Arc<dyn ApprovalPolicy>>,
    event_sink: Option<Arc<dyn EventSink>>,
    max_tool_steps: usize,
}

impl KernelBuilder {
    pub fn with_backend(mut self, backend: Arc<dyn Backend>) -> Self {
        self.backend = Some(backend);
        self
    }

    pub fn with_tool(mut self, tool: Arc<dyn Tool>) -> Self {
        self.tools.push(tool);
        self
    }

    pub fn with_tools(mut self, tools: Vec<Arc<dyn Tool>>) -> Self {
        self.tools.extend(tools);
        self
    }

    pub fn with_history(mut self, history: ConversationHistory) -> Self {
        self.history = Some(history);
        self
    }

    pub fn with_budget(mut self, budget: TokenBudget) -> Self {
        self.budget = Some(budget);
        self
    }

    pub fn with_estimator(mut self, estimator: TokenEstimator) -> Self {
        self.estimator = Some(estimator);
        self
    }

    pub fn with_policy(mut self, policy: Arc<dyn ApprovalPolicy>) -> Self {
        self.policy = Some(policy);
        self
    }

    pub fn with_event_sink(mut self, event_sink: Arc<dyn EventSink>) -> Self {
        self.event_sink = Some(event_sink);
        self
    }

    pub fn with_max_tool_steps(mut self, steps: usize) -> Self {
        self.max_tool_steps = steps;
        self
    }

    pub fn build(self) -> Result<Kernel, &'static str> {
        let backend = self.backend.ok_or("backend is required")?;
        let history = self.history.unwrap_or_default();
        let budget = self.budget.unwrap_or_default();
        let estimator = self.estimator.unwrap_or_default();
        let policy = self
            .policy
            .unwrap_or_else(|| Arc::new(kobold_types::AllowAllPolicy));
        let event_sink = self
            .event_sink
            .unwrap_or_else(|| Arc::new(kobold_types::NoopEventSink));
        let max_tool_steps = if self.max_tool_steps == 0 {
            25
        } else {
            self.max_tool_steps
        };

        let mut tool_map = HashMap::new();
        for tool in self.tools {
            tool_map.insert(tool.name().to_string(), tool);
        }

        Ok(Kernel {
            backend,
            tools: tool_map,
            history,
            budget,
            estimator,
            policy,
            event_sink,
            max_tool_steps,
        })
    }
}
