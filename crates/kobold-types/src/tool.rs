use serde::{Deserialize, Serialize};

use crate::error::ToolError;

/// Schema definition for a tool callable by the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

impl ToolDefinition {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
        }
    }
}

/// An invocation request for a tool produced by the model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

impl ToolCall {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            arguments: arguments.into(),
        }
    }
}

/// The result of executing a tool call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolOutput {
    pub tool_call_id: String,
    pub content: String,
    pub is_error: bool,
}

impl ToolOutput {
    /// Create a successful tool execution output.
    pub fn success(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            tool_call_id: tool_call_id.into(),
            content: content.into(),
            is_error: false,
        }
    }

    /// Create an error-flagged tool execution output (e.g. non-zero exit code).
    pub fn error(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            tool_call_id: tool_call_id.into(),
            content: content.into(),
            is_error: true,
        }
    }
}

/// Interface contract for tools executable by the agent kernel.
#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    /// Canonical name of the tool.
    fn name(&self) -> &str;

    /// Definition schema advertised to the model.
    fn definition(&self) -> ToolDefinition;

    /// Execute the tool invocation.
    async fn execute(&self, call: &ToolCall) -> Result<ToolOutput, ToolError>;
}
