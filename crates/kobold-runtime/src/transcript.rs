use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use kobold_context::history::ConversationHistory;
use kobold_types::{ContentPart, Message, Role, TokenUsage, ToolCall, TurnFinishReason};
use serde::{Deserialize, Serialize};

use crate::error::RuntimeError;

pub const DEFAULT_KOBOLD_DIR: &str = ".kobold";
pub const DEFAULT_TRANSCRIPT_FILE: &str = "transcript.jsonl";

/// An immutable record appended to the transcript DAG log.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TranscriptRecord {
    pub ts: u64,
    pub session_id: String,
    pub branch: String,
    pub seq: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_seq: Option<usize>,
    pub role: Role,
    pub content: Vec<ContentPart>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<TurnFinishReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<TokenUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

impl TranscriptRecord {
    pub fn from_message(
        session_id: &str,
        branch: &str,
        seq: usize,
        parent: Option<(&str, usize)>,
        msg: &Message,
        finish_reason: Option<TurnFinishReason>,
        usage: Option<TokenUsage>,
    ) -> Self {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let text = msg.text();
        let text_opt = if text.is_empty() { None } else { Some(text) };

        Self {
            ts,
            session_id: session_id.to_string(),
            branch: branch.to_string(),
            seq,
            parent: parent.map(|(p, _)| p.to_string()),
            parent_seq: parent.map(|(_, s)| s),
            role: msg.role,
            content: msg.content.clone(),
            tool_calls: msg.tool_calls.clone(),
            tool_call_id: msg.tool_call_id.clone(),
            finish_reason,
            usage,
            text: text_opt,
        }
    }

    pub fn to_message(&self) -> Message {
        Message {
            role: self.role,
            content: self.content.clone(),
            name: None,
            tool_calls: self.tool_calls.clone(),
            tool_call_id: self.tool_call_id.clone(),
            tool_pruning: None,
        }
    }
}

/// Append-only persistent transcript log for a workspace session.
pub struct TranscriptLog {
    path: PathBuf,
    file: Mutex<Option<File>>,
}

impl TranscriptLog {
    /// Open or create a transcript log at the specified path.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, RuntimeError> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;

        Ok(Self {
            path,
            file: Mutex::new(Some(file)),
        })
    }

    /// Open or create default transcript log in `<workspace_root>/.kobold/transcript.jsonl`.
    pub fn open_default(workspace_root: &Path) -> Result<Self, RuntimeError> {
        let path = workspace_root
            .join(DEFAULT_KOBOLD_DIR)
            .join(DEFAULT_TRANSCRIPT_FILE);
        Self::open(path)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append a record to the log, immediately flushing to disk.
    pub fn append(&self, record: &TranscriptRecord) -> Result<(), RuntimeError> {
        let mut guard = self.file.lock().map_err(|e| {
            RuntimeError::Transcript(format!("Transcript lock poisoned: {e}"))
        })?;

        if let Some(ref mut file) = *guard {
            let line = serde_json::to_string(record)?;
            writeln!(file, "{}", line)?;
            file.flush()?;
        }
        Ok(())
    }

    /// Read all valid records present in the transcript file.
    /// Skips truncated or malformed trailing records (e.g. from process crash).
    pub fn read_all_records(&self) -> Result<Vec<TranscriptRecord>, RuntimeError> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }

        let file = File::open(&self.path)?;
        let reader = BufReader::new(file);
        let mut records = Vec::new();

        for line_res in reader.lines() {
            let line = match line_res {
                Ok(l) => l,
                Err(_) => continue,
            };
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(rec) = serde_json::from_str::<TranscriptRecord>(&line) {
                records.push(rec);
            }
        }

        Ok(records)
    }

    /// Return the next sequence number for the specified branch.
    pub fn next_seq(&self, branch: &str) -> Result<usize, RuntimeError> {
        let records = self.read_all_records()?;
        let max_seq = records
            .into_iter()
            .filter(|r| r.branch == branch)
            .map(|r| r.seq)
            .max();

        Ok(max_seq.map(|s| s + 1).unwrap_or(0))
    }

    /// Reconstruct all records belonging to a branch by following parent pointers recursively.
    pub fn read_branch_records(&self, branch: &str) -> Result<Vec<TranscriptRecord>, RuntimeError> {
        let all_records = self.read_all_records()?;
        let mut branches: HashMap<String, Vec<TranscriptRecord>> = HashMap::new();

        for rec in all_records {
            branches.entry(rec.branch.clone()).or_default().push(rec);
        }

        // Sort each branch by seq
        for recs in branches.values_mut() {
            recs.sort_by_key(|r| r.seq);
        }

        fn collect_branch(
            target: &str,
            cutoff: Option<usize>,
            branches: &HashMap<String, Vec<TranscriptRecord>>,
            visited: &mut Vec<String>,
        ) -> Vec<TranscriptRecord> {
            if visited.contains(&target.to_string()) {
                return Vec::new();
            }
            visited.push(target.to_string());

            let Some(branch_recs) = branches.get(target) else {
                return Vec::new();
            };

            let mut result = Vec::new();
            // Check if first record has parent
            if let Some(first) = branch_recs.first() {
                if let (Some(parent_name), Some(parent_seq)) = (&first.parent, first.parent_seq) {
                    let parent_history =
                        collect_branch(parent_name, Some(parent_seq), branches, visited);
                    result.extend(parent_history);
                }
            }

            // Append records from this branch up to cutoff
            for rec in branch_recs {
                if let Some(max_s) = cutoff {
                    if rec.seq > max_s {
                        break;
                    }
                }
                result.push(rec.clone());
            }

            result
        }

        let mut visited = Vec::new();
        Ok(collect_branch(branch, None, &branches, &mut visited))
    }

    /// Replay the transcript DAG of a branch into a ConversationHistory.
    pub fn replay_to_history(&self, branch: &str) -> Result<ConversationHistory, RuntimeError> {
        let records = self.read_branch_records(branch)?;
        let mut history = ConversationHistory::new();
        for rec in records {
            history.append_message(rec.to_message());
        }
        Ok(history)
    }

    /// Past user messages in chronological order, for input history.
    pub fn user_history(&self) -> Result<Vec<String>, RuntimeError> {
        let records = self.read_all_records()?;
        let history = records
            .into_iter()
            .filter(|r| r.role == Role::User)
            .filter_map(|r| r.text)
            .collect();
        Ok(history)
    }
}
