pub mod checkpoint;
pub mod sandbox;
pub mod tool;

use std::path::PathBuf;
use std::sync::Arc;
use kobold_types::Tool;

pub use checkpoint::{CheckpointError, CheckpointManager};
pub use sandbox::{
    ExecutionResult, HostProcessExecutor, MicroVmExecutor, SandboxExecutor, SandboxMode,
    MAX_OUTPUT_BYTES,
};
pub use tool::BashTool;

/// Create a host-sandboxed bash tool instance rooted at `workspace_root`.
pub fn create_host_bash_tool(workspace_root: PathBuf) -> Arc<dyn Tool> {
    let executor = Arc::new(HostProcessExecutor::new(workspace_root));
    Arc::new(BashTool::new(executor))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kobold_types::{PruningPolicy, ToolCall};
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_tool_is_mutating() {
        let dir = tempdir().unwrap();
        let tool = create_host_bash_tool(dir.path().to_path_buf());

        assert!(tool.is_mutating());
        assert!(tool.definition().is_mutating);
    }

    #[tokio::test]
    async fn test_one_shot_bash_echo() {
        let dir = tempdir().unwrap();
        let tool = create_host_bash_tool(dir.path().to_path_buf());

        let call = ToolCall::new(
            "b1",
            "bash",
            serde_json::json!({ "command": "echo 'hello from bash'" }).to_string(),
        );

        let out = tool.execute(&call).await.unwrap();
        assert!(!out.is_error);
        assert_eq!(out.content.trim(), "hello from bash");
        assert!(matches!(out.pruning, PruningPolicy::HeadTail { .. }));
    }

    #[tokio::test]
    async fn test_bash_non_zero_exit_reports_error() {
        let dir = tempdir().unwrap();
        let tool = create_host_bash_tool(dir.path().to_path_buf());

        let call = ToolCall::new(
            "b2",
            "bash",
            serde_json::json!({ "command": "echo 'failing now' >&2; exit 7" }).to_string(),
        );

        let out = tool.execute(&call).await.unwrap();
        assert!(out.is_error);
        assert!(out.content.contains("Command failed with exit code 7"));
        assert!(out.content.contains("[stderr]\nfailing now"));
    }

    #[tokio::test]
    async fn test_bash_timeout() {
        let dir = tempdir().unwrap();
        let tool = create_host_bash_tool(dir.path().to_path_buf());

        let call = ToolCall::new(
            "b3",
            "bash",
            serde_json::json!({
                "command": "sleep 5",
                "timeout_secs": 1
            })
            .to_string(),
        );

        let out = tool.execute(&call).await.unwrap();
        assert!(out.is_error);
        assert!(out.content.contains("Command timed out after 1s"));
    }

    #[tokio::test]
    async fn test_persistent_bash_session() {
        let dir = tempdir().unwrap();
        let tool = create_host_bash_tool(dir.path().to_path_buf());

        // Command 1: Set shell variable in session
        let c1 = ToolCall::new(
            "s1",
            "bash",
            serde_json::json!({
                "command": "KOBOLD_TEST_VAR='value_42'",
                "session_id": "sess_1"
            })
            .to_string(),
        );
        let out1 = tool.execute(&c1).await.unwrap();
        assert!(!out1.is_error);

        // Command 2: Access shell variable from same session
        let c2 = ToolCall::new(
            "s2",
            "bash",
            serde_json::json!({
                "command": "echo \"var is $KOBOLD_TEST_VAR\"",
                "session_id": "sess_1"
            })
            .to_string(),
        );
        let out2 = tool.execute(&c2).await.unwrap();
        assert!(!out2.is_error);
        assert_eq!(out2.content.trim(), "var is value_42");
    }

    #[tokio::test]
    async fn test_apfs_cow_checkpoint_and_rewind() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let ck_dir = root.join(".kobold").join("checkpoints");

        let manager = CheckpointManager::new(root.clone(), ck_dir);

        // 1. Initial file state
        let initial_file = root.join("initial.txt");
        std::fs::write(&initial_file, "original version").unwrap();

        // 2. Snapshot checkpoint
        manager.create_checkpoint("cp_1").unwrap();
        assert_eq!(manager.list_checkpoints().unwrap(), vec!["cp_1"]);

        // 3. Mutate workspace (modify existing file, add new file)
        std::fs::write(&initial_file, "corrupted version").unwrap();
        let new_file = root.join("unexpected.txt");
        std::fs::write(&new_file, "rogue file").unwrap();

        assert_eq!(std::fs::read_to_string(&initial_file).unwrap(), "corrupted version");
        assert!(new_file.exists());

        // 4. Rewind to checkpoint
        manager.rewind_to_checkpoint("cp_1").unwrap();

        // 5. Verify restored state
        assert_eq!(std::fs::read_to_string(&initial_file).unwrap(), "original version");
        assert!(!new_file.exists());

        // 6. Delete checkpoint
        manager.delete_checkpoint("cp_1").unwrap();
        assert!(manager.list_checkpoints().unwrap().is_empty());
    }
}
