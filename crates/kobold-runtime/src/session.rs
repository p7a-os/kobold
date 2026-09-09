use std::path::{Path, PathBuf};
use std::sync::Arc;

use kobold_context::history::ConversationHistory;
use kobold_kernel::{Kernel, TurnResult};
use kobold_tool_bash::checkpoint::CheckpointManager;
use kobold_tool_bash::sandbox::SandboxMode;
use kobold_types::EventSink;

use crate::error::RuntimeError;
use crate::transcript::{TranscriptLog, TranscriptRecord};

/// An active in-process agent session bound to a workspace root and persistent transcript DAG.
pub struct AgentSession {
    session_id: String,
    workspace_root: PathBuf,
    branch: String,
    parent_fork: Option<(String, usize)>,
    kernel: Kernel,
    transcript: Arc<TranscriptLog>,
    sandbox_mode: SandboxMode,
    event_sink: Arc<dyn EventSink>,
    checkpoint_mgr: Option<CheckpointManager>,
}

impl AgentSession {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        session_id: String,
        workspace_root: PathBuf,
        branch: String,
        parent_fork: Option<(String, usize)>,
        kernel: Kernel,
        transcript: Arc<TranscriptLog>,
        sandbox_mode: SandboxMode,
        event_sink: Arc<dyn EventSink>,
        checkpoint_mgr: Option<CheckpointManager>,
    ) -> Self {
        Self {
            session_id,
            workspace_root,
            branch,
            parent_fork,
            kernel,
            transcript,
            sandbox_mode,
            event_sink,
            checkpoint_mgr,
        }
    }

    /// Unique session identifier.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Bound workspace root path.
    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    /// Current transcript branch name.
    pub fn branch(&self) -> &str {
        &self.branch
    }

    /// Configured sandbox execution mode.
    pub fn sandbox_mode(&self) -> SandboxMode {
        self.sandbox_mode
    }

    /// Read-only reference to conversation history.
    pub fn history(&self) -> &ConversationHistory {
        self.kernel.history()
    }

    /// Reference to the transcript log.
    pub fn transcript(&self) -> &Arc<TranscriptLog> {
        &self.transcript
    }

    /// Reference to the event sink.
    pub fn event_sink(&self) -> &Arc<dyn EventSink> {
        &self.event_sink
    }

    /// Execute a user prompt as a full agent turn, appending all messages and tool
    /// interactions to the persistent transcript DAG.
    pub async fn prompt(&mut self, user_input: &str) -> Result<TurnResult, RuntimeError> {
        let prev_msg_count = self.kernel.history().len();

        // 1. Run the kernel step
        let turn_result = self.kernel.step(user_input).await?;

        // 2. Extract newly generated messages from history
        let all_msgs = self.kernel.history().messages();
        let new_msgs = if all_msgs.len() > prev_msg_count {
            &all_msgs[prev_msg_count..]
        } else {
            &[]
        };

        // 3. Append new messages to transcript DAG
        let start_seq = self.transcript.next_seq(&self.branch)?;
        let parent_info = if start_seq == 0 {
            self.parent_fork.as_ref().map(|(b, s)| (b.as_str(), *s))
        } else {
            None
        };

        for (i, msg) in new_msgs.iter().enumerate() {
            let cur_seq = start_seq + i;
            let is_last = i == new_msgs.len() - 1;
            let finish_reason = if is_last {
                Some(turn_result.finish_reason.clone())
            } else {
                None
            };
            let usage = if is_last { turn_result.usage } else { None };

            let record = TranscriptRecord::from_message(
                &self.session_id,
                &self.branch,
                cur_seq,
                if cur_seq == 0 { parent_info } else { None },
                msg,
                finish_reason,
                usage,
            );

            self.transcript.append(&record)?;
        }

        Ok(turn_result)
    }

    /// Fork conversation from current or specified branch checkpoint into a new parallel branch.
    pub fn fork(&self, _new_branch_name: &str, from_seq: usize) -> Result<Self, RuntimeError> {
        // Build replayed history for the fork point
        let mut forked_history = ConversationHistory::new();
        let branch_records = self.transcript.read_branch_records(&self.branch)?;

        for rec in branch_records {
            if rec.seq <= from_seq {
                forked_history.append_message(rec.to_message());
            }
        }

        // Reconstruct a new Kernel with the same backend, tools, budget, and estimator
        let mut builder = Kernel::builder()
            .with_history(forked_history)
            .with_budget(*self.kernel.budget())
            .with_estimator(self.kernel.estimator().clone())
            .with_event_sink(self.event_sink.clone());

        for tool in self.kernel.tools().values() {
            builder = builder.with_tool(tool.clone());
        }

        // Backend is re-used
        // Note: KernelBuilder needs a backend, we can rebuild using the builder pattern
        // but AgentSession can duplicate or re-wire via SessionBuilder.
        Err(RuntimeError::Transcript(
            "Use SessionBuilder::fork_from_session for full fork assembly".into(),
        ))
    }

    /// Create an APFS CoW checkpoint of the workspace root (Apple Silicon).
    pub fn create_checkpoint(&self, name: &str) -> Result<PathBuf, RuntimeError> {
        if let Some(ref mgr) = self.checkpoint_mgr {
            mgr.create_checkpoint(name)
                .map_err(|e| RuntimeError::Io(std::io::Error::other(e.to_string())))
        } else {
            Err(RuntimeError::Config("Checkpoint manager is not configured".into()))
        }
    }

    /// Rollback workspace root to a named APFS CoW checkpoint.
    pub fn rollback_checkpoint(&self, name: &str) -> Result<(), RuntimeError> {
        if let Some(ref mgr) = self.checkpoint_mgr {
            mgr.rewind_to_checkpoint(name)
                .map_err(|e| RuntimeError::Io(std::io::Error::other(e.to_string())))
        } else {
            Err(RuntimeError::Config("Checkpoint manager is not configured".into()))
        }
    }
}
