use std::path::PathBuf;
use async_trait::async_trait;
use kobold_types::{PruningPolicy, Tool, ToolCall, ToolDefinition, ToolError, ToolOutput};
use serde::Deserialize;

use crate::cone::{resolve_in_cone, MAX_READ_BYTES};

#[derive(Debug, Deserialize)]
struct ReadArgs {
    path: String,
    start_line: Option<usize>,
    end_line: Option<usize>,
}

/// Tool for reading file contents safely within the workspace cone.
#[derive(Debug, Clone)]
pub struct ReadFileTool {
    workspace_root: PathBuf,
}

impl ReadFileTool {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self { workspace_root }
    }
}

#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "read_file",
            "Read UTF-8 text file contents within the workspace cone. Supports optional 1-indexed line range slicing.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file, relative to the workspace root or within the cone."
                    },
                    "start_line": {
                        "type": "integer",
                        "description": "Optional 1-indexed starting line number."
                    },
                    "end_line": {
                        "type": "integer",
                        "description": "Optional 1-indexed ending line number (inclusive)."
                    }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
        )
        .with_mutating(false)
    }

    fn is_mutating(&self) -> bool {
        false
    }

    async fn execute(&self, call: &ToolCall) -> Result<ToolOutput, ToolError> {
        let args: ReadArgs = match serde_json::from_str(&call.arguments) {
            Ok(a) => a,
            Err(e) => {
                return Ok(ToolOutput::error(
                    &call.id,
                    format!("Invalid arguments for read_file: {e}"),
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
                format!("'{}' is a directory, use list_dir instead", args.path),
            ));
        }

        let bytes = match tokio::fs::read(&resolved_path).await {
            Ok(b) => b,
            Err(e) => {
                return Ok(ToolOutput::error(
                    &call.id,
                    format!("Cannot read '{}': {e}", args.path),
                ));
            }
        };

        let total_bytes = bytes.len();
        let content_str = String::from_utf8_lossy(&bytes);

        let output_text = if args.start_line.is_some() || args.end_line.is_some() {
            let lines: Vec<&str> = content_str.lines().collect();
            let total_lines = lines.len();

            let start = args.start_line.unwrap_or(1).max(1);
            let end = args.end_line.unwrap_or(total_lines).min(total_lines);

            if start > total_lines {
                format!(
                    "[start_line {} exceeds total line count {}]",
                    start, total_lines
                )
            } else if start > end {
                format!(
                    "[invalid range: start_line {} is greater than end_line {}]",
                    start, end
                )
            } else {
                let sliced = &lines[start - 1..end];
                let mut out = String::new();
                for (idx, line) in sliced.iter().enumerate() {
                    let line_no = start + idx;
                    out.push_str(&format!("{line_no:>4}: {line}\n"));
                }
                out
            }
        } else if total_bytes > MAX_READ_BYTES {
            let clipped = &bytes[..MAX_READ_BYTES];
            let mut s = String::from_utf8_lossy(clipped).into_owned();
            s.push_str(&format!(
                "\n\n[truncated: {total_bytes} bytes total, first {MAX_READ_BYTES} shown]"
            ));
            s
        } else {
            content_str.into_owned()
        };

        let output = ToolOutput::success(&call.id, output_text)
            .with_pruning(PruningPolicy::KeepLast { key: args.path });

        Ok(output)
    }
}
