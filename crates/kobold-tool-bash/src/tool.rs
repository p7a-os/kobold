use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use kobold_types::{PruningPolicy, Tool, ToolCall, ToolDefinition, ToolError, ToolOutput};
use serde::Deserialize;

use crate::sandbox::SandboxExecutor;

const DEFAULT_TIMEOUT_SECS: u64 = 30;

#[derive(Debug, Deserialize)]
struct BashArgs {
    command: String,
    session_id: Option<String>,
    timeout_secs: Option<u64>,
}

/// Unified bash execution tool implementing the `kobold_types::Tool` trait.
#[derive(Clone)]
pub struct BashTool {
    executor: Arc<dyn SandboxExecutor>,
}

impl BashTool {
    pub fn new(executor: Arc<dyn SandboxExecutor>) -> Self {
        Self { executor }
    }
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "bash",
            "Execute shell commands in the sandboxed workspace. Supports one-shot execution and persistent sessions.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The bash command string to execute."
                    },
                    "session_id": {
                        "type": "string",
                        "description": "Optional identifier for a persistent subshell session retaining environment and working directory."
                    },
                    "timeout_secs": {
                        "type": "integer",
                        "description": "Maximum execution duration in seconds before aborting. Defaults to 30."
                    }
                },
                "required": ["command"],
                "additionalProperties": false
            }),
        )
        .with_mutating(true)
    }

    fn is_mutating(&self) -> bool {
        true
    }

    async fn execute(&self, call: &ToolCall) -> Result<ToolOutput, ToolError> {
        let args: BashArgs = match serde_json::from_str(&call.arguments) {
            Ok(a) => a,
            Err(e) => {
                return Ok(ToolOutput::error(
                    &call.id,
                    format!("Invalid arguments for bash tool: {e}"),
                ));
            }
        };

        if args.command.trim().is_empty() {
            return Ok(ToolOutput::error(&call.id, "Command cannot be empty"));
        }

        let timeout_duration =
            Duration::from_secs(args.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS));

        let result = self
            .executor
            .execute(
                &args.command,
                args.session_id.as_deref(),
                timeout_duration,
            )
            .await?;

        let formatted = result.format_output();

        let output = if result.exit_code == 0 && !result.timed_out {
            ToolOutput::success(&call.id, formatted)
        } else {
            let prefix = if result.timed_out {
                format!("Command timed out after {}s", timeout_duration.as_secs())
            } else {
                format!("Command failed with exit code {}", result.exit_code)
            };

            let content = if formatted.is_empty() {
                prefix
            } else {
                format!("{prefix}\n{formatted}")
            };
            ToolOutput::error(&call.id, content)
        };

        // Apply HeadTail pruning policy for bash execution outputs (D-30)
        let output = output.with_pruning(PruningPolicy::HeadTail {
            head_lines: 20,
            tail_lines: 20,
        });

        Ok(output)
    }
}
