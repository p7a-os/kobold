use std::path::PathBuf;
use async_trait::async_trait;
use kobold_types::{PruningPolicy, Tool, ToolCall, ToolDefinition, ToolError, ToolOutput};
use serde::Deserialize;

use crate::cone::resolve_in_cone;

#[derive(Debug, Deserialize)]
struct WriteArgs {
    path: String,
    content: String,
    #[serde(default = "default_true")]
    overwrite: bool,
}

fn default_true() -> bool {
    true
}

/// Tool for writing files safely within the workspace cone.
#[derive(Debug, Clone)]
pub struct WriteFileTool {
    workspace_root: PathBuf,
}

impl WriteFileTool {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self { workspace_root }
    }
}

#[async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &str {
        "write_file"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "write_file",
            "Write or overwrite a file within the workspace cone. Creates parent directories automatically.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the target file, relative to the workspace root or within the cone."
                    },
                    "content": {
                        "type": "string",
                        "description": "The exact content to write to the file."
                    },
                    "overwrite": {
                        "type": "boolean",
                        "description": "Whether to overwrite the file if it already exists. Defaults to true."
                    }
                },
                "required": ["path", "content"],
                "additionalProperties": false
            }),
        )
        .with_mutating(true)
    }

    fn is_mutating(&self) -> bool {
        true
    }

    async fn execute(&self, call: &ToolCall) -> Result<ToolOutput, ToolError> {
        let args: WriteArgs = match serde_json::from_str(&call.arguments) {
            Ok(a) => a,
            Err(e) => {
                return Ok(ToolOutput::error(
                    &call.id,
                    format!("Invalid arguments for write_file: {e}"),
                ));
            }
        };

        let resolved_path = match resolve_in_cone(&self.workspace_root, &args.path) {
            Ok(p) => p,
            Err(e) => return Ok(ToolOutput::error(&call.id, e)),
        };

        if resolved_path.is_dir() {
            return Ok(ToolOutput::error(
                &call.id,
                format!("'{}' is an existing directory, cannot overwrite as file", args.path),
            ));
        }

        if resolved_path.exists() && !args.overwrite {
            return Ok(ToolOutput::error(
                &call.id,
                format!("File '{}' already exists and overwrite is false", args.path),
            ));
        }

        if let Some(parent) = resolved_path.parent() {
            if !parent.exists() {
                if let Err(e) = tokio::fs::create_dir_all(parent).await {
                    return Ok(ToolOutput::error(
                        &call.id,
                        format!("Failed to create parent directories for '{}': {e}", args.path),
                    ));
                }
            }
        }

        let bytes = args.content.as_bytes();
        let bytes_len = bytes.len();

        if let Err(e) = tokio::fs::write(&resolved_path, bytes).await {
            return Ok(ToolOutput::error(
                &call.id,
                format!("Failed to write to '{}': {e}", args.path),
            ));
        }

        let output = ToolOutput::success(
            &call.id,
            format!("Successfully wrote {bytes_len} bytes to '{}'", args.path),
        )
        .with_pruning(PruningPolicy::CollapseAfterTurns { turns: 2 });

        Ok(output)
    }
}
