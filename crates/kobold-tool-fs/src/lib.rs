pub mod cone;
pub mod edit;
pub mod list;
pub mod read;
pub mod write;

use std::path::PathBuf;
use std::sync::Arc;
use kobold_types::Tool;

pub use cone::{normalise_path, outside_cone_error, resolve_in_cone, MAX_READ_BYTES};
pub use edit::EditFileTool;
pub use list::ListDirTool;
pub use read::ReadFileTool;
pub use write::WriteFileTool;

/// Construct the full standard suite of filesystem tools bound to a workspace root cone.
pub fn create_fs_tools(workspace_root: PathBuf) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(ReadFileTool::new(workspace_root.clone())),
        Arc::new(WriteFileTool::new(workspace_root.clone())),
        Arc::new(EditFileTool::new(workspace_root.clone())),
        Arc::new(ListDirTool::new(workspace_root)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use kobold_types::{PruningPolicy, ToolCall};
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_tool_mutating_flags() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();

        let read_tool = ReadFileTool::new(root.clone());
        let list_tool = ListDirTool::new(root.clone());
        let write_tool = WriteFileTool::new(root.clone());
        let edit_tool = EditFileTool::new(root.clone());

        assert!(!read_tool.is_mutating());
        assert!(!read_tool.definition().is_mutating);

        assert!(!list_tool.is_mutating());
        assert!(!list_tool.definition().is_mutating);

        assert!(write_tool.is_mutating());
        assert!(write_tool.definition().is_mutating);

        assert!(edit_tool.is_mutating());
        assert!(edit_tool.definition().is_mutating);
    }

    #[tokio::test]
    async fn test_write_and_read_file() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();

        let write_tool = WriteFileTool::new(root.clone());
        let read_tool = ReadFileTool::new(root.clone());

        // 1. Write file with subdirectories
        let write_call = ToolCall::new(
            "w1",
            "write_file",
            serde_json::json!({
                "path": "nested/sub/hello.txt",
                "content": "Line 1\nLine 2\nLine 3\nLine 4\nLine 5\n"
            })
            .to_string(),
        );

        let out = write_tool.execute(&write_call).await.unwrap();
        assert!(!out.is_error);
        assert!(out.content.contains("Successfully wrote"));
        assert!(matches!(out.pruning, PruningPolicy::CollapseAfterTurns { .. }));

        // 2. Read full file
        let read_call = ToolCall::new(
            "r1",
            "read_file",
            serde_json::json!({
                "path": "nested/sub/hello.txt"
            })
            .to_string(),
        );

        let out = read_tool.execute(&read_call).await.unwrap();
        assert!(!out.is_error);
        assert_eq!(
            out.content,
            "Line 1\nLine 2\nLine 3\nLine 4\nLine 5\n"
        );
        assert!(matches!(out.pruning, PruningPolicy::KeepLast { .. }));

        // 3. Read with line range
        let read_slice_call = ToolCall::new(
            "r2",
            "read_file",
            serde_json::json!({
                "path": "nested/sub/hello.txt",
                "start_line": 2,
                "end_line": 4
            })
            .to_string(),
        );

        let out = read_tool.execute(&read_slice_call).await.unwrap();
        assert!(!out.is_error);
        assert!(out.content.contains("   2: Line 2"));
        assert!(out.content.contains("   3: Line 3"));
        assert!(out.content.contains("   4: Line 4"));
        assert!(!out.content.contains("Line 1"));
        assert!(!out.content.contains("Line 5"));
    }

    #[tokio::test]
    async fn test_cone_escape_refusal() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();

        let read_tool = ReadFileTool::new(root.clone());
        let write_tool = WriteFileTool::new(root.clone());

        // Attempt escape via ..
        let escape_read = ToolCall::new(
            "c1",
            "read_file",
            serde_json::json!({ "path": "../../../etc/passwd" }).to_string(),
        );
        let out = read_tool.execute(&escape_read).await.unwrap();
        assert!(out.is_error);
        assert!(out.content.contains("outside the working directory cone"));

        // Attempt escape via absolute path
        let escape_write = ToolCall::new(
            "c2",
            "write_file",
            serde_json::json!({ "path": "/tmp/forbidden.txt", "content": "bad" }).to_string(),
        );
        let out = write_tool.execute(&escape_write).await.unwrap();
        assert!(out.is_error);
        assert!(out.content.contains("outside the working directory cone"));
    }

    #[tokio::test]
    async fn test_edit_file_exact_and_multiple() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();

        let write_tool = WriteFileTool::new(root.clone());
        let edit_tool = EditFileTool::new(root.clone());
        let read_tool = ReadFileTool::new(root.clone());

        // Setup test file
        let write_call = ToolCall::new(
            "w1",
            "write_file",
            serde_json::json!({
                "path": "code.rs",
                "content": "fn foo() -> i32 {\n    let x = 42;\n    let y = 42;\n    x + y\n}\n"
            })
            .to_string(),
        );
        write_tool.execute(&write_call).await.unwrap();

        // 1. Single replacement when multiple exist and allow_multiple is false -> error
        let edit_call_fail = ToolCall::new(
            "e1",
            "edit_file",
            serde_json::json!({
                "path": "code.rs",
                "target": "42",
                "replacement": "100",
                "allow_multiple": false
            })
            .to_string(),
        );
        let out = edit_tool.execute(&edit_call_fail).await.unwrap();
        assert!(out.is_error);
        assert!(out.content.contains("Found 2 occurrences"));

        // 2. Allow multiple -> success
        let edit_call_ok = ToolCall::new(
            "e2",
            "edit_file",
            serde_json::json!({
                "path": "code.rs",
                "target": "42",
                "replacement": "100",
                "allow_multiple": true
            })
            .to_string(),
        );
        let out = edit_tool.execute(&edit_call_ok).await.unwrap();
        assert!(!out.is_error);
        assert!(out.content.contains("Successfully replaced 2 occurrences"));

        // Verify edited content
        let read_call = ToolCall::new(
            "r1",
            "read_file",
            serde_json::json!({ "path": "code.rs" }).to_string(),
        );
        let out = read_tool.execute(&read_call).await.unwrap();
        assert!(out.content.contains("let x = 100;"));
        assert!(out.content.contains("let y = 100;"));
    }

    #[tokio::test]
    async fn test_list_dir() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();

        let write_tool = WriteFileTool::new(root.clone());
        let list_tool = ListDirTool::new(root.clone());

        // Write files in root and subdir
        let w1 = ToolCall::new(
            "w1",
            "write_file",
            serde_json::json!({ "path": "root.txt", "content": "root file" }).to_string(),
        );
        let w2 = ToolCall::new(
            "w2",
            "write_file",
            serde_json::json!({ "path": "src/main.rs", "content": "fn main() {}" }).to_string(),
        );
        let w3 = ToolCall::new(
            "w3",
            "write_file",
            serde_json::json!({ "path": "src/util/helper.rs", "content": "pub fn h() {}" }).to_string(),
        );

        write_tool.execute(&w1).await.unwrap();
        write_tool.execute(&w2).await.unwrap();
        write_tool.execute(&w3).await.unwrap();

        // 1. Non-recursive listing
        let list_call = ToolCall::new(
            "l1",
            "list_dir",
            serde_json::json!({ "path": ".", "recursive": false }).to_string(),
        );
        let out = list_tool.execute(&list_call).await.unwrap();
        assert!(!out.is_error);
        assert!(out.content.contains("[FILE] root.txt"));
        assert!(out.content.contains("[DIR]  src/"));
        assert!(!out.content.contains("main.rs"));

        // 2. Recursive listing
        let list_rec_call = ToolCall::new(
            "l2",
            "list_dir",
            serde_json::json!({ "path": ".", "recursive": true, "max_depth": 3 }).to_string(),
        );
        let out = list_tool.execute(&list_rec_call).await.unwrap();
        assert!(!out.is_error);
        assert!(out.content.contains("[FILE] root.txt"));
        assert!(out.content.contains("[DIR]  src/"));
        assert!(out.content.contains("[FILE] src/main.rs"));
        assert!(out.content.contains("[DIR]  src/util/"));
        assert!(out.content.contains("[FILE] src/util/helper.rs"));
    }
}
