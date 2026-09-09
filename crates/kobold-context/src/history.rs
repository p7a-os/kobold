use std::ops::Range;

use kobold_types::{Message, Role, ToolCall, ToolOutput};

/// Conversation history maintaining deterministic message ordering and turn boundaries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConversationHistory {
    messages: Vec<Message>,
}

impl ConversationHistory {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
        }
    }

    pub fn with_system_prompt(system_text: impl Into<String>) -> Self {
        let mut history = Self::new();
        history.append_system(system_text);
        history
    }

    /// Read-only slice of all messages in history.
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// Mutable reference to message vector (for compaction operations).
    pub fn messages_mut(&mut self) -> &mut Vec<Message> {
        &mut self.messages
    }

    pub fn len(&self) -> usize {
        self.messages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// Append an arbitrary pre-built message to history.
    pub fn append_message(&mut self, message: Message) {
        self.messages.push(message);
    }

    /// Append a system instruction message.
    pub fn append_system(&mut self, text: impl Into<String>) {
        self.messages.push(Message::system(text));
    }

    /// Append a user prompt message.
    pub fn append_user(&mut self, text: impl Into<String>) {
        self.messages.push(Message::user(text));
    }

    /// Append an assistant text reply message.
    pub fn append_assistant(&mut self, text: impl Into<String>) {
        self.messages.push(Message::assistant(text));
    }

    /// Append an assistant reply with thought reasoning and text content.
    pub fn append_assistant_with_thought(
        &mut self,
        thought: impl Into<String>,
        text: impl Into<String>,
    ) {
        self.messages
            .push(Message::assistant_with_thought(thought, text));
    }

    /// Append an assistant message requesting tool calls.
    pub fn append_assistant_tool_calls(&mut self, calls: Vec<ToolCall>) {
        self.messages.push(Message::assistant_tool_calls(calls));
    }

    /// Append a tool execution output message, carrying its pruning policy.
    pub fn append_tool_output(&mut self, output: &ToolOutput) {
        self.messages.push(Message::from_tool_output(output));
    }

    /// Calculate the current turn index (0-based count of completed and active turns).
    pub fn current_turn_index(&self) -> usize {
        let ranges = self.turn_ranges();
        if ranges.is_empty() {
            0
        } else {
            ranges.len().saturating_sub(1)
        }
    }

    /// Count total turns present in the conversation history.
    pub fn turn_count(&self) -> usize {
        self.turn_ranges().len()
    }

    /// Identify atomic turn boundaries in the message sequence.
    ///
    /// Turn 0 is typically the system message (if present).
    /// Each subsequent turn begins with a User message and contains all subsequent
    /// Assistant messages and Tool results up to the next User message.
    pub fn turn_ranges(&self) -> Vec<Range<usize>> {
        let mut ranges = Vec::new();
        if self.messages.is_empty() {
            return ranges;
        }

        let mut current_start = 0;
        let mut i = 0;

        // If the first message is System, treat it as standalone Turn 0
        if self.messages[0].role == Role::System {
            ranges.push(0..1);
            current_start = 1;
            i = 1;
        }

        while i < self.messages.len() {
            if self.messages[i].role == Role::User && i > current_start {
                ranges.push(current_start..i);
                current_start = i;
            }
            i += 1;
        }

        if current_start < self.messages.len() {
            ranges.push(current_start..self.messages.len());
        }

        ranges
    }
}
