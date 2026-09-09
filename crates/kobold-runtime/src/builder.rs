use std::path::{Path, PathBuf};
use std::sync::Arc;

use kobold_backend_openai::{OpenAiBackend, OpenAiConfig, DEFAULT_MODEL, DEFAULT_REASONING_EFFORT};
use kobold_context::budget::TokenBudget;
use kobold_context::history::ConversationHistory;
use kobold_context::prompt::SystemPromptBuilder;
use kobold_kernel::Kernel;
use kobold_tool_bash::checkpoint::CheckpointManager;
use kobold_tool_bash::sandbox::{MicroVmExecutor, SandboxMode};
use kobold_tool_bash::{create_host_bash_tool, BashTool};
use kobold_tool_fs::create_fs_tools;
use kobold_types::{ApprovalPolicy, Backend, EventSink, Tool};
use uuid::Uuid;

use crate::error::RuntimeError;
use crate::session::AgentSession;
use crate::transcript::TranscriptLog;

/// Builder for constructing and configuring an `AgentSession`.
pub struct SessionBuilder {
    workspace_root: PathBuf,
    session_id: Option<String>,
    branch: String,
    parent_fork: Option<(String, usize)>,
    backend: Option<Arc<dyn Backend>>,
    sandbox_mode: SandboxMode,
    tools: Vec<Arc<dyn Tool>>,
    disable_default_tools: bool,
    policy: Option<Arc<dyn ApprovalPolicy>>,
    event_sink: Option<Arc<dyn EventSink>>,
    token_budget: Option<TokenBudget>,
    max_tool_steps: usize,
    zdr_enabled: bool,
    transcript_path: Option<PathBuf>,
    agents_md_content: Option<String>,
}

impl Default for SessionBuilder {
    fn default() -> Self {
        Self {
            workspace_root: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            session_id: None,
            branch: "main".to_string(),
            parent_fork: None,
            backend: None,
            sandbox_mode: SandboxMode::HostProcess,
            tools: Vec::new(),
            disable_default_tools: false,
            policy: None,
            event_sink: None,
            token_budget: None,
            max_tool_steps: 25,
            zdr_enabled: false,
            transcript_path: None,
            agents_md_content: None,
        }
    }
}

impl SessionBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_workspace_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.workspace_root = root.into();
        self
    }

    pub fn with_session_id(mut self, id: impl Into<String>) -> Self {
        self.session_id = Some(id.into());
        self
    }

    pub fn with_branch(mut self, branch: impl Into<String>) -> Self {
        self.branch = branch.into();
        self
    }

    pub fn with_fork(mut self, parent_branch: impl Into<String>, parent_seq: usize) -> Self {
        self.parent_fork = Some((parent_branch.into(), parent_seq));
        self
    }

    pub fn with_backend(mut self, backend: Arc<dyn Backend>) -> Self {
        self.backend = Some(backend);
        self
    }

    pub fn with_openai_backend(
        mut self,
        api_key: impl Into<String>,
        model: Option<&str>,
        reasoning_effort: Option<&str>,
    ) -> Self {
        let key = api_key.into();
        let config = OpenAiConfig::new(key)
            .with_model(model.unwrap_or(DEFAULT_MODEL))
            .with_reasoning_effort(reasoning_effort.unwrap_or(DEFAULT_REASONING_EFFORT))
            .with_zdr(self.zdr_enabled);
        self.backend = Some(Arc::new(OpenAiBackend::new(config)));
        self
    }

    pub fn with_sandbox_mode(mut self, mode: SandboxMode) -> Self {
        self.sandbox_mode = mode;
        self
    }

    pub fn with_tool(mut self, tool: Arc<dyn Tool>) -> Self {
        self.tools.push(tool);
        self
    }

    pub fn with_tools(mut self, tools: Vec<Arc<dyn Tool>>) -> Self {
        self.tools.extend(tools);
        self
    }

    pub fn without_default_tools(mut self) -> Self {
        self.disable_default_tools = true;
        self
    }

    pub fn with_policy(mut self, policy: Arc<dyn ApprovalPolicy>) -> Self {
        self.policy = Some(policy);
        self
    }

    pub fn with_event_sink(mut self, sink: Arc<dyn EventSink>) -> Self {
        self.event_sink = Some(sink);
        self
    }

    pub fn with_token_budget(mut self, budget: TokenBudget) -> Self {
        self.token_budget = Some(budget);
        self
    }

    pub fn with_max_tool_steps(mut self, steps: usize) -> Self {
        self.max_tool_steps = steps;
        self
    }

    pub fn with_zdr(mut self, enabled: bool) -> Self {
        self.zdr_enabled = enabled;
        self
    }

    pub fn with_transcript_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.transcript_path = Some(path.into());
        self
    }

    pub fn with_agents_md(mut self, content: impl Into<String>) -> Self {
        self.agents_md_content = Some(content.into());
        self
    }

    /// Assemble and initialize the agent session.
    pub fn build(self) -> Result<AgentSession, RuntimeError> {
        let session_id = self
            .session_id
            .unwrap_or_else(|| Uuid::now_v7().to_string());

        let workspace_root = if let Ok(canonical) = self.workspace_root.canonicalize() {
            canonical
        } else {
            self.workspace_root.clone()
        };

        // 1. Resolve or detect git branch
        let git_branch = detect_git_branch(&workspace_root).unwrap_or_else(|| "main".to_string());

        // 2. Open or create transcript log
        let transcript = if let Some(path) = self.transcript_path {
            Arc::new(TranscriptLog::open(path)?)
        } else {
            Arc::new(TranscriptLog::open_default(&workspace_root)?)
        };

        // 3. Initialize ConversationHistory
        let mut history = if let Some((ref p_branch, p_seq)) = self.parent_fork {
            // Replay parent branch up to p_seq
            let branch_recs = transcript.read_branch_records(p_branch)?;
            let mut hist = ConversationHistory::new();
            for rec in branch_recs {
                if rec.seq <= p_seq {
                    hist.append_message(rec.to_message());
                }
            }
            hist
        } else {
            // Check if existing records exist on this branch
            let branch_recs = transcript.read_branch_records(&self.branch)?;
            if !branch_recs.is_empty() {
                let mut hist = ConversationHistory::new();
                for rec in branch_recs {
                    hist.append_message(rec.to_message());
                }
                hist
            } else {
                ConversationHistory::new()
            }
        };

        // 4. If history is brand new, build and insert system prompt
        if history.is_empty() {
            let mut prompt_builder = SystemPromptBuilder::new()
                .with_cwd(workspace_root.display().to_string())
                .with_git_branch(&git_branch)
                .with_zdr(self.zdr_enabled);

            if let Some(content) = self.agents_md_content {
                prompt_builder = prompt_builder.with_agents_md(content);
            } else {
                prompt_builder = prompt_builder.discover_workspace_rules(&workspace_root);
            }

            let system_msg = prompt_builder.build();
            history.append_message(system_msg);
        }

        // 5. Backend resolution
        let backend = match self.backend {
            Some(b) => b,
            None => {
                let api_key = std::env::var("LLM_API_KEY")
                    .or_else(|_| std::env::var("OPENAI_API_KEY"))
                    .map_err(|_| {
                        RuntimeError::Config(
                            "No backend provided and neither LLM_API_KEY nor OPENAI_API_KEY is set in the environment".into(),
                        )
                    })?;
                let config = OpenAiConfig::new(api_key)
                    .with_model(DEFAULT_MODEL)
                    .with_reasoning_effort(DEFAULT_REASONING_EFFORT)
                    .with_zdr(self.zdr_enabled);
                Arc::new(OpenAiBackend::new(config))
            }
        };

        // 6. Assemble tools
        let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
        if !self.disable_default_tools {
            // Add FS tools bound to workspace cone
            tools.extend(create_fs_tools(workspace_root.clone()));

            // Add Bash tool configured for selected sandbox mode
            let bash_tool: Arc<dyn Tool> = match self.sandbox_mode {
                SandboxMode::HostProcess => create_host_bash_tool(workspace_root.clone()),
                SandboxMode::MicroVm => {
                    let executor = Arc::new(MicroVmExecutor::new(3, 1024, None));
                    Arc::new(BashTool::new(executor))
                }
            };
            tools.push(bash_tool);
        }
        tools.extend(self.tools);

        // 7. Initialize APFS CoW CheckpointManager
        let checkpoint_mgr = Some(CheckpointManager::default_for_workspace(&workspace_root));

        // 8. Assemble Kernel
        let mut kernel_builder = Kernel::builder()
            .with_backend(backend)
            .with_tools(tools)
            .with_history(history)
            .with_max_tool_steps(self.max_tool_steps);

        if let Some(budget) = self.token_budget {
            kernel_builder = kernel_builder.with_budget(budget);
        }
        if let Some(policy) = self.policy {
            kernel_builder = kernel_builder.with_policy(policy);
        }
        if let Some(sink) = self.event_sink.clone() {
            kernel_builder = kernel_builder.with_event_sink(sink);
        }

        let kernel = kernel_builder
            .build()
            .map_err(|e| RuntimeError::Config(format!("Failed to build kernel: {e}")))?;

        let event_sink = self
            .event_sink
            .unwrap_or_else(|| Arc::new(kobold_types::NoopEventSink));

        Ok(AgentSession::new(
            session_id,
            workspace_root,
            self.branch,
            self.parent_fork,
            kernel,
            transcript,
            self.sandbox_mode,
            event_sink,
            checkpoint_mgr,
        ))
    }
}

/// Helper to detect git branch from `.git/HEAD` without spawning subprocesses.
fn detect_git_branch(workspace_root: &Path) -> Option<String> {
    let head_path = workspace_root.join(".git").join("HEAD");
    if head_path.is_file() {
        if let Ok(content) = std::fs::read_to_string(&head_path) {
            let line = content.trim();
            if let Some(ref_path) = line.strip_prefix("ref: refs/heads/") {
                return Some(ref_path.to_string());
            }
            if !line.is_empty() {
                // Detached HEAD (commit hash)
                return Some(line.chars().take(8).collect());
            }
        }
    }
    None
}
