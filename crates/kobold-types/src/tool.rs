use serde::{Deserialize, Serialize};

use crate::error::ToolError;

/// Schema definition for a tool callable by the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
    #[serde(default)]
    pub is_mutating: bool,
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
            is_mutating: false,
        }
    }

    /// Mark this tool as performing filesystem or environmental mutations.
    pub fn with_mutating(mut self, is_mutating: bool) -> Self {
        self.is_mutating = is_mutating;
        self
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

/// Policy dictating how historical execution results for this tool call may be pruned or compacted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum PruningPolicy {
    /// Never prune or compact this output; retain verbatim in history.
    Never,

    /// Keep only the latest output sharing this key (e.g. file path for read_file).
    KeepLast { key: String },

    /// Retain the first and last lines, replacing the middle with an omission marker.
    HeadTail { head_lines: usize, tail_lines: usize },

    /// Keep verbatim for N completed turns, then compact to a one-line status stub.
    CollapseAfterTurns { turns: usize },

    /// Replace with a concise summary provided by the tool during compaction.
    Summary { summary: String },
}

impl Default for PruningPolicy {
    fn default() -> Self {
        Self::CollapseAfterTurns { turns: 2 }
    }
}

/// The result of executing a tool call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolOutput {
    pub tool_call_id: String,
    pub content: String,
    pub is_error: bool,
    #[serde(default)]
    pub pruning: PruningPolicy,
}

impl ToolOutput {
    /// Create a successful tool execution output.
    pub fn success(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            tool_call_id: tool_call_id.into(),
            content: content.into(),
            is_error: false,
            pruning: PruningPolicy::default(),
        }
    }

    /// Create an error-flagged tool execution output (e.g. non-zero exit code).
    pub fn error(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            tool_call_id: tool_call_id.into(),
            content: content.into(),
            is_error: true,
            pruning: PruningPolicy::default(),
        }
    }

    /// Attach a specific pruning policy to this output.
    pub fn with_pruning(mut self, pruning: PruningPolicy) -> Self {
        self.pruning = pruning;
        self
    }

    /// Set pruning policy to KeepLast with a resource key.
    pub fn with_keep_last(mut self, key: impl Into<String>) -> Self {
        self.pruning = PruningPolicy::KeepLast { key: key.into() };
        self
    }

    /// Set pruning policy to retain head and tail lines.
    pub fn with_head_tail(mut self, head_lines: usize, tail_lines: usize) -> Self {
        self.pruning = PruningPolicy::HeadTail {
            head_lines,
            tail_lines,
        };
        self
    }

    /// Set pruning policy to replace with a tool-authored summary.
    pub fn with_summary(mut self, summary: impl Into<String>) -> Self {
        self.pruning = PruningPolicy::Summary {
            summary: summary.into(),
        };
        self
    }
}

/// Interface contract for tools executable by the agent kernel.
#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    /// Canonical name of the tool.
    fn name(&self) -> &str;

    /// Definition schema advertised to the model.
    fn definition(&self) -> ToolDefinition;

    /// Whether this tool modifies the filesystem or environment.
    ///
    /// When `false`, read-only operations can execute directly in-process against
    /// the clean host filesystem without spawning or booting the microVM sandbox.
    fn is_mutating(&self) -> bool {
        self.definition().is_mutating
    }

    /// Execute the tool invocation.
    async fn execute(&self, call: &ToolCall) -> Result<ToolOutput, ToolError>;
}
