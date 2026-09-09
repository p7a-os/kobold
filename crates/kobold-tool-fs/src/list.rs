use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use async_trait::async_trait;
use kobold_types::{PruningPolicy, Tool, ToolCall, ToolDefinition, ToolError, ToolOutput};
use serde::Deserialize;

use crate::cone::resolve_in_cone;

const MAX_LIST_ENTRIES: usize = 500;

#[derive(Debug, Deserialize)]
struct ListArgs {
    #[serde(default = "default_current_dir")]
    path: String,
    #[serde(default)]
    recursive: bool,
    max_depth: Option<usize>,
}

fn default_current_dir() -> String {
    ".".to_string()
}

/// Tool for listing directory entries safely within the workspace cone.
#[derive(Debug, Clone)]
pub struct ListDirTool {
    workspace_root: PathBuf,
}

impl ListDirTool {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self { workspace_root }
    }
}

#[async_trait]
impl Tool for ListDirTool {
    fn name(&self) -> &str {
        "list_dir"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "list_dir",
            "List contents of a directory within the workspace cone, including names, types, and sizes.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Directory path to list, relative to workspace root. Defaults to root ('.')."
                    },
                    "recursive": {
                        "type": "boolean",
                        "description": "Whether to list subdirectories recursively. Defaults to false."
                    },
                    "max_depth": {
                        "type": "integer",
                        "description": "Maximum recursion depth when recursive is true. Defaults to 3."
                    }
                },
                "additionalProperties": false
            }),
        )
        .with_mutating(false)
    }

    fn is_mutating(&self) -> bool {
        false
    }

    async fn execute(&self, call: &ToolCall) -> Result<ToolOutput, ToolError> {
        let args: ListArgs = if call.arguments.trim().is_empty() {
            ListArgs {
                path: ".".to_string(),
                recursive: false,
                max_depth: None,
            }
        } else {
            match serde_json::from_str(&call.arguments) {
                Ok(a) => a,
                Err(e) => {
                    return Ok(ToolOutput::error(
                        &call.id,
                        format!("Invalid arguments for list_dir: {e}"),
                    ));
                }
            }
        };

        let resolved_path = match resolve_in_cone(&self.workspace_root, &args.path) {
            Ok(p) => p,
            Err(e) => return Ok(ToolOutput::error(&call.id, e)),
        };

        if !resolved_path.exists() {
            return Ok(ToolOutput::error(
                &call.id,
                format!("Directory '{}' does not exist", args.path),
            ));
        }

        if !resolved_path.is_dir() {
            return Ok(ToolOutput::error(
                &call.id,
                format!("'{}' is a file, use read_file instead", args.path),
            ));
        }

        let max_depth = if args.recursive {
            args.max_depth.unwrap_or(3)
        } else {
            1
        };

        let (entries, truncated) = collect_entries_bfs(&resolved_path, max_depth).await;

        let mut output_str = format!("Directory listing for '{}':\n", args.path);
        for entry in &entries {
            output_str.push_str(entry);
            output_str.push('\n');
        }

        if truncated {
            output_str.push_str(&format!(
                "\n[truncated: listing exceeded limit of {MAX_LIST_ENTRIES} entries]"
            ));
        }

        let output = ToolOutput::success(&call.id, output_str)
            .with_pruning(PruningPolicy::KeepLast { key: args.path });

        Ok(output)
    }
}

struct QueueItem {
    dir_path: PathBuf,
    depth: usize,
}

async fn collect_entries_bfs(base: &Path, max_depth: usize) -> (Vec<String>, bool) {
    let mut entries = Vec::new();
    let mut truncated = false;
    let mut queue = VecDeque::new();

    queue.push_back(QueueItem {
        dir_path: base.to_path_buf(),
        depth: 0,
    });

    while let Some(item) = queue.pop_front() {
        if item.depth >= max_depth {
            continue;
        }

        let mut dir = match tokio::fs::read_dir(&item.dir_path).await {
            Ok(d) => d,
            Err(_) => continue,
        };

        let mut children = Vec::new();
        while let Ok(Some(entry)) = dir.next_entry().await {
            children.push(entry);
        }

        // Sort alphabetically for deterministic ordering
        children.sort_by_key(|e| e.file_name());

        for entry in children {
            if entries.len() >= MAX_LIST_ENTRIES {
                truncated = true;
                return (entries, truncated);
            }

            let path = entry.path();
            let rel_path = path.strip_prefix(base).unwrap_or(&path).to_string_lossy();
            let file_type = entry.file_type().await.ok();

            if let Some(ft) = file_type {
                if ft.is_dir() {
                    entries.push(format!("[DIR]  {rel_path}/"));
                    if item.depth + 1 < max_depth {
                        queue.push_back(QueueItem {
                            dir_path: path,
                            depth: item.depth + 1,
                        });
                    }
                } else if ft.is_symlink() {
                    entries.push(format!("[LNK]  {rel_path}"));
                } else {
                    let size = entry.metadata().await.map(|m| m.len()).unwrap_or(0);
                    entries.push(format!("[FILE] {rel_path} ({size} bytes)"));
                }
            }
        }
    }

    (entries, truncated)
}
