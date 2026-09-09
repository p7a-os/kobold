use std::path::Path;

use kobold_types::Message;

/// Default base agent instructions for Kobold.
pub const DEFAULT_BASE_PROMPT: &str = "\
You are Kobold, a fast, lightweight autonomous agent harness.
You solve software development tasks with verified evidence and minimal intervention.
Follow all project instructions, use available tools deliberately, and verify your results before reporting completion.";

/// Builder for constructing structured system prompts with workspace rules and environment context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemPromptBuilder {
    base_prompt: String,
    agents_md_content: Option<String>,
    cwd: Option<String>,
    git_branch: Option<String>,
    timestamp: Option<String>,
    zdr_enabled: bool,
}

impl Default for SystemPromptBuilder {
    fn default() -> Self {
        Self {
            base_prompt: DEFAULT_BASE_PROMPT.to_string(),
            agents_md_content: None,
            cwd: None,
            git_branch: None,
            timestamp: None,
            zdr_enabled: false,
        }
    }
}

impl SystemPromptBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_base_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.base_prompt = prompt.into();
        self
    }

    /// Provide AGENTS.md content directly (useful for tests or pre-read contents).
    pub fn with_agents_md(mut self, content: impl Into<String>) -> Self {
        self.agents_md_content = Some(content.into());
        self
    }

    /// Discover AGENTS.md in the specified workspace root directory.
    ///
    /// Strictly ignores CLAUDE.md in adherence with project decision D-29.
    pub fn discover_workspace_rules(mut self, workspace_root: &Path) -> Self {
        let agents_path = workspace_root.join("AGENTS.md");
        if agents_path.is_file() {
            if let Ok(content) = std::fs::read_to_string(&agents_path) {
                self.agents_md_content = Some(content);
            }
        }
        // Decision D-29: strictly ignore CLAUDE.md even if present
        self
    }

    pub fn with_cwd(mut self, cwd: impl Into<String>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn with_git_branch(mut self, branch: impl Into<String>) -> Self {
        self.git_branch = Some(branch.into());
        self
    }

    pub fn with_timestamp(mut self, timestamp: impl Into<String>) -> Self {
        self.timestamp = Some(timestamp.into());
        self
    }

    pub fn with_zdr(mut self, enabled: bool) -> Self {
        self.zdr_enabled = enabled;
        self
    }

    /// Render the full assembled system prompt text.
    pub fn render(&self) -> String {
        let mut sections = Vec::new();

        // 1. Base Agent Instructions
        sections.push(self.base_prompt.trim().to_string());

        // 2. Project Instructions (from AGENTS.md)
        if let Some(agents_md) = &self.agents_md_content {
            sections.push(format!(
                "# Project Instructions (from AGENTS.md)\n\n{}",
                agents_md.trim()
            ));
        }

        // 3. Environment Context
        let mut env_lines = Vec::new();
        if let Some(cwd) = &self.cwd {
            env_lines.push(format!("- Working Directory: {}", cwd));
        }
        if let Some(branch) = &self.git_branch {
            env_lines.push(format!("- Git Branch: {}", branch));
        }
        if let Some(time) = &self.timestamp {
            env_lines.push(format!("- Current Time: {}", time));
        }
        if self.zdr_enabled {
            env_lines.push("- Privacy Mode: [ZDR: ON] (Zero Data Retention active; prompt caching disabled)".to_string());
        } else {
            env_lines.push("- Privacy Mode: [ZDR: OFF] (Server-side prompt caching enabled)".to_string());
        }

        if !env_lines.is_empty() {
            sections.push(format!(
                "# Environment Context\n\n{}",
                env_lines.join("\n")
            ));
        }

        sections.join("\n\n")
    }

    /// Build a System message from the rendered system prompt.
    pub fn build(self) -> Message {
        Message::system(self.render())
    }
}
