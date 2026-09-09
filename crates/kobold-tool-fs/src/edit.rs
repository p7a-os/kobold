use std::path::PathBuf;
use async_trait::async_trait;
use kobold_types::{PruningPolicy, Tool, ToolCall, ToolDefinition, ToolError, ToolOutput};
use serde::Deserialize;

use crate::cone::resolve_in_cone;

#[derive(Debug, Deserialize)]
struct EditArgs {
    path: String,
    target: String,
    replacement: String,
    #[serde(default)]
    allow_multiple: bool,
}

/// Tool for performing precise text edits inside existing files within the workspace cone.
#[derive(Debug, Clone)]
pub struct EditFileTool {
    workspace_root: PathBuf,
}

impl EditFileTool {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self { workspace_root }
    }
}

#[async_trait]
impl Tool for EditFileTool {
    fn name(&self) -> &str {
        "edit_file"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "edit_file",
            "Perform targeted string replacements in an existing file within the workspace cone.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to edit, relative to workspace root."
                    },
                    "target": {
                        "type": "string",
                        "description": "The exact character sequence to replace."
                    },
                    "replacement": {
                        "type": "string",
                        "description": "The new replacement string."
                    },
                    "allow_multiple": {
                        "type": "boolean",
                        "description": "Whether to allow replacing multiple occurrences. Defaults to false."
                    }
                },
                "required": ["path", "target", "replacement"],
                "additionalProperties": false
            }),
        )
        .with_mutating(true)
    }

    fn is_mutating(&self) -> bool {
        true
    }

    async fn execute(&self, call: &ToolCall) -> Result<ToolOutput, ToolError> {
        let args: EditArgs = match serde_json::from_str(&call.arguments) {
            Ok(a) => a,
            Err(e) => {
                return Ok(ToolOutput::error(
                    &call.id,
                    format!("Invalid arguments for edit_file: {e}"),
                ));
            }
        };

        if args.target.is_empty() {
            return Ok(ToolOutput::error(
                &call.id,
                "The target string to replace cannot be empty".to_string(),
            ));
        }

        let resolved_path = match resolve_in_cone(&self.workspace_root, &args.path) {
            Ok(p) => p,
            Err(e) => return Ok(ToolOutput::error(&call.id, e)),
        };

        if !resolved_path.exists() {
            return Ok(ToolOutput::error(
                &call.id,
                format!("File '{}' does not exist", args.path),
            ));
        }

        if resolved_path.is_dir() {
            return Ok(ToolOutput::error(
                &call.id,
                format!("'{}' is a directory, not a file", args.path),
            ));
        }

        let content = match tokio::fs::read_to_string(&resolved_path).await {
            Ok(c) => c,
            Err(e) => {
                return Ok(ToolOutput::error(
                    &call.id,
                    format!("Cannot read '{}': {e}", args.path),
                ));
            }
        };

        let matches_count = content.matches(&args.target).count();
        if matches_count == 0 {
            return Ok(ToolOutput::error(
                &call.id,
                format!("Target string was not found in '{}'", args.path),
            ));
        }

        if matches_count > 1 && !args.allow_multiple {
            return Ok(ToolOutput::error(
                &call.id,
                format!(
                    "Found {matches_count} occurrences of target string in '{}', but allow_multiple is false",
                    args.path
                ),
            ));
        }

        let updated_content = if args.allow_multiple {
            content.replace(&args.target, &args.replacement)
        } else {
            content.replacen(&args.target, &args.replacement, 1)
        };

        if let Err(e) = tokio::fs::write(&resolved_path, updated_content.as_bytes()).await {
            return Ok(ToolOutput::error(
                &call.id,
                format!("Failed to write updated content to '{}': {e}", args.path),
            ));
        }

        let occurrences_str = if matches_count == 1 {
            "1 occurrence".to_string()
        } else {
            format!("{matches_count} occurrences")
        };

        let output = ToolOutput::success(
            &call.id,
            format!("Successfully replaced {occurrences_str} in '{}'", args.path),
        )
        .with_pruning(PruningPolicy::CollapseAfterTurns { turns: 2 });

        Ok(output)
    }
}
