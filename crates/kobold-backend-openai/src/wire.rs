use kobold_types::{Message, Role, TokenUsage, ToolDefinition};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize)]
pub struct ResponseCreate<'a> {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub model: &'a str,
    pub store: bool,
    pub input: Vec<InputItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningConfig<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDefinitionWire>>,
}

#[derive(Debug, Serialize)]
pub struct ReasoningConfig<'a> {
    pub effort: &'a str,
    pub summary: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum InputItem {
    Message {
        #[serde(rename = "type")]
        kind: &'static str,
        role: &'static str,
        content: Vec<ContentPartWire>,
    },
    FunctionCall {
        #[serde(rename = "type")]
        kind: &'static str,
        call_id: String,
        name: String,
        arguments: String,
    },
    FunctionCallOutput {
        #[serde(rename = "type")]
        kind: &'static str,
        call_id: String,
        output: String,
    },
}

#[derive(Debug, Serialize)]
pub struct ContentPartWire {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub text: String,
}

#[derive(Debug, Serialize)]
pub struct ToolDefinitionWire {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

impl ToolDefinitionWire {
    pub fn from_tool_def(def: &ToolDefinition) -> Self {
        Self {
            kind: "function",
            name: def.name.clone(),
            description: def.description.clone(),
            parameters: def.parameters.clone(),
        }
    }
}

/// Convert conversation messages from `kobold-types` into wire input items.
pub fn convert_messages_to_input(messages: &[Message]) -> Vec<InputItem> {
    let mut items = Vec::new();

    for msg in messages {
        match msg.role {
            Role::Tool => {
                let call_id = msg.tool_call_id.clone().unwrap_or_default();
                if !call_id.is_empty() {
                    let output = msg.text();
                    items.push(InputItem::FunctionCallOutput {
                        kind: "function_call_output",
                        call_id,
                        output,
                    });
                }
            }
            Role::Assistant => {
                // If assistant requested tool calls, output function_call items
                for call in &msg.tool_calls {
                    if !call.id.is_empty() {
                        items.push(InputItem::FunctionCall {
                            kind: "function_call",
                            call_id: call.id.clone(),
                            name: call.name.clone(),
                            arguments: call.arguments.clone(),
                        });
                    }
                }
                // If assistant has text, output message item with type "output_text"
                let text = msg.text();
                if !text.is_empty() {
                    items.push(InputItem::Message {
                        kind: "message",
                        role: "assistant",
                        content: vec![ContentPartWire {
                            kind: "output_text",
                            text,
                        }],
                    });
                }
            }
            Role::User => {
                let text = msg.text();
                items.push(InputItem::Message {
                    kind: "message",
                    role: "user",
                    content: vec![ContentPartWire {
                        kind: "input_text",
                        text,
                    }],
                });
            }
            Role::System => {
                let text = msg.text();
                items.push(InputItem::Message {
                    kind: "message",
                    role: "system",
                    content: vec![ContentPartWire {
                        kind: "input_text",
                        text,
                    }],
                });
            }
        }
    }

    items
}

// ---------- Incoming Wire Events ----------

#[derive(Debug, Clone, Deserialize)]
pub struct WireEvent {
    #[serde(rename = "type")]
    pub kind: String,

    #[serde(default)]
    pub delta: Option<String>,

    #[serde(default)]
    pub call_id: Option<String>,

    #[serde(default)]
    pub name: Option<String>,

    #[serde(default)]
    pub arguments: Option<String>,

    #[serde(default)]
    pub item: Option<WireItem>,

    #[serde(default)]
    pub response: Option<WireResponse>,

    #[serde(default)]
    pub error: Option<WireError>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WireItem {
    #[serde(rename = "type")]
    pub kind: Option<String>,

    #[serde(default)]
    pub id: Option<String>,

    #[serde(default)]
    pub call_id: Option<String>,

    #[serde(default)]
    pub name: Option<String>,

    #[serde(default)]
    pub arguments: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WireResponse {
    #[serde(default)]
    pub id: Option<String>,

    #[serde(default)]
    pub status: Option<String>,

    #[serde(default)]
    pub usage: Option<WireUsage>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WireUsage {
    #[serde(default)]
    pub input_tokens: u32,

    #[serde(default)]
    pub output_tokens: u32,

    #[serde(default)]
    pub total_tokens: u32,
}

impl WireUsage {
    pub fn to_token_usage(&self) -> TokenUsage {
        TokenUsage::new(self.input_tokens, self.output_tokens)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct WireError {
    #[serde(default)]
    pub message: Option<String>,

    #[serde(default)]
    pub code: Option<String>,
}
