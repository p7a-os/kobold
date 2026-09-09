use serde::{Deserialize, Serialize};

use crate::tool::{PruningPolicy, ToolCall, ToolOutput};

/// Role of a message participant in a conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// Content piece within a message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text { text: String },
    Thought { thought: String },
}

impl ContentPart {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    pub fn thought(thought: impl Into<String>) -> Self {
        Self::Thought {
            thought: thought.into(),
        }
    }
}

/// A conversation message exchanged between user, assistant, and tools.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentPart>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_pruning: Option<PruningPolicy>,
}

impl Message {
    /// Create a new system message.
    pub fn system(text: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: vec![ContentPart::text(text)],
            name: None,
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_pruning: None,
        }
    }

    /// Create a new user message.
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: vec![ContentPart::text(text)],
            name: None,
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_pruning: None,
        }
    }

    /// Create an assistant response with text content.
    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: vec![ContentPart::text(text)],
            name: None,
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_pruning: None,
        }
    }

    /// Create an assistant response with internal thought and text content.
    pub fn assistant_with_thought(thought: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: vec![ContentPart::thought(thought), ContentPart::text(text)],
            name: None,
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_pruning: None,
        }
    }

    /// Create an assistant message requesting tool calls.
    pub fn assistant_tool_calls(tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: Role::Assistant,
            content: Vec::new(),
            name: None,
            tool_calls,
            tool_call_id: None,
            tool_pruning: None,
        }
    }

    /// Create a tool response message.
    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: vec![ContentPart::text(content)],
            name: None,
            tool_calls: Vec::new(),
            tool_call_id: Some(tool_call_id.into()),
            tool_pruning: Some(PruningPolicy::default()),
        }
    }

    /// Create a tool response message with a specific pruning policy.
    pub fn tool_result_with_policy(
        tool_call_id: impl Into<String>,
        content: impl Into<String>,
        pruning: PruningPolicy,
    ) -> Self {
        Self {
            role: Role::Tool,
            content: vec![ContentPart::text(content)],
            name: None,
            tool_calls: Vec::new(),
            tool_call_id: Some(tool_call_id.into()),
            tool_pruning: Some(pruning),
        }
    }

    /// Create a tool response message from an executed ToolOutput.
    pub fn from_tool_output(output: &ToolOutput) -> Self {
        Self {
            role: Role::Tool,
            content: vec![ContentPart::text(&output.content)],
            name: None,
            tool_calls: Vec::new(),
            tool_call_id: Some(output.tool_call_id.clone()),
            tool_pruning: Some(output.pruning.clone()),
        }
    }

    /// Extract concatenated text content from the message.
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|part| match part {
                ContentPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }

    /// Extract concatenated thought content from the message, if any.
    pub fn thought(&self) -> Option<String> {
        let thoughts: Vec<_> = self
            .content
            .iter()
            .filter_map(|part| match part {
                ContentPart::Thought { thought } => Some(thought.as_str()),
                _ => None,
            })
            .collect();
        if thoughts.is_empty() {
            None
        } else {
            Some(thoughts.join(""))
        }
    }
}
