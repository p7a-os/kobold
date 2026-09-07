//! Persistent configuration at `.kobold/settings.json`.
//!
//! Precedence is env > file > default. Environment wins so a one-off run can
//! override without editing the file, and every field has a default so a
//! missing or partial file is never an error -- a malformed one degrades to
//! defaults with a note rather than refusing to start.

use std::path::Path;

use serde::{Deserialize, Serialize};

pub const FILE: &str = "settings.json";

/// One MCP server, as a command to run.
///
/// stdio only here: a server reached over HTTP has no command to spawn, and
/// wiring that is a separate decision about which endpoints kobold will talk
/// to unprompted.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct McpServer {
    /// Namespaces the server's tools. `files` makes its `search` appear as
    /// `files__search`, which is what the model calls and what kobold routes
    /// on, so it cannot be changed without changing both.
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    /// Environment given to this server, on top of the short base list in
    /// `mcp::stdio`. A server does NOT inherit kobold's environment, so
    /// anything it needs -- its own API token, a config path -- is named
    /// here, for that server only.
    ///
    /// Per server rather than global, and with no wildcard, so handing a
    /// credential to a subprocess is a deliberate act naming both the secret
    /// and who receives it.
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub model: String,
    /// `none` | `low` | `medium` | `high` | `xhigh` | `max`.
    pub reasoning_effort: String,
    pub voice: Voice,
    /// 256-colour index behind fenced code blocks. Assumes a dark terminal.
    pub code_bg: u8,
    /// MCP servers to connect to at startup. Their tools join the model's list
    /// alongside kobold's own, namespaced by server name.
    /// Which provider adapter to run. Empty means the one shipped beside
    /// Kobold; a path or a name on `PATH` selects another.
    ///
    /// A setting rather than a constant because the whole reason the
    /// provider is a separate process is that it can be a different one.
    #[serde(default)]
    pub adapter: String,
    /// Arguments for the adapter, if it takes any. Kobold's own takes none;
    /// a third-party one may.
    #[serde(default)]
    pub adapter_args: Vec<String>,
    /// Hosts the adapter is allowed to reach, and the only ones it can.
    ///
    /// Kobold denies the adapter a network entirely and brokers every
    /// connection against this list, so an entry here is a deliberate grant
    /// rather than a hint. **Empty means the default below**, not "allow
    /// everything" -- there is no spelling of this setting that opens the
    /// network, because a community adapter with unrestricted egress is the
    /// thing the sandbox exists to prevent.
    #[serde(default)]
    pub adapter_allow: Vec<String>,
    pub mcp_servers: Vec<McpServer>,
    /// Tools the API runs on its own side, named by type -- `web_search` and
    /// whatever else it offers.
    ///
    /// A list rather than a set of flags, so a tool the API adds tomorrow works
    /// by being typed here rather than by kobold learning about it. Empty by
    /// default: each one costs tokens in every request and can reach the
    /// network on the model's say-so, which is a decision rather than a default.
    pub server_tools: Vec<String>,
    /// Tokens the model can hold, for the gauge at the end of the bar.
    ///
    /// Set here because the server never says: no event carries a context
    /// limit, and a WebSocket has no per-request headers to put one in. So
    /// this is a local claim about the model, and a wrong one shows a wrong
    /// gauge.
    ///
    /// The default is deliberately the conservative direction. Assuming a
    /// smaller window than the model really has makes the gauge read fuller
    /// than it is, which is merely annoying; assuming a larger one lets it sit
    /// at a comfortable third of the way along right up until the conversation
    /// stops fitting. Set it to what the model actually holds.
    pub context_window: u32,
    /// Default agent/provider harness selected on setup.
    #[serde(default)]
    pub default_agent: Option<String>,
    /// Currently active harness (e.g. claude-code, grok-build, codex, openai, openrouter).
    #[serde(default)]
    pub current_harness: Option<String>,
    /// Cached supported models for each harness.
    #[serde(default)]
    pub models_cache: std::collections::BTreeMap<String, Vec<String>>,
    /// Configured external coding agents supervised by Kobold.
    #[serde(default)]
    pub agents: std::collections::BTreeMap<String, AgentSetting>,
    /// Configured model providers / gateways.
    #[serde(default)]
    pub providers: std::collections::BTreeMap<String, ProviderSetting>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct AgentSetting {
    pub name: String,
    pub command: String,
    pub enabled: bool,
    pub detected: bool,
    pub working: bool,
    pub status: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct ProviderSetting {
    pub name: String,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub api_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Voice {
    /// Speak replies from startup, without needing `/voice`.
    pub enabled: bool,
    /// Engine command. Empty means "use the bundled sidecar".
    pub command: String,
    /// Voice name passed to the engine. For the bundled Pocket TTS engine:
    /// alba, marius, javert, jean, fantine, cosette, eponine, azelma.
    pub voice: String,
    /// When set, audio is shipped here instead of played locally.
    pub audio_addr: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            model: "gpt-5.6-luna".to_owned(),
            reasoning_effort: "none".to_owned(),
            voice: Voice::default(),
            code_bg: 235,
            adapter: String::new(),
            adapter_args: Vec::new(),
            adapter_allow: Vec::new(),
            mcp_servers: Vec::new(),
            server_tools: Vec::new(),
            context_window: 128_000,
            default_agent: None,
            current_harness: None,
            models_cache: std::collections::BTreeMap::new(),
            agents: std::collections::BTreeMap::new(),
            providers: std::collections::BTreeMap::new(),
        }
    }
}

impl Settings {
    /// Returns the currently active harness identifier (e.g. "claude-code", "grok-build", "openai").
    pub fn active_harness(&self) -> &str {
        self.current_harness
            .as_deref()
            .or(self.default_agent.as_deref())
            .unwrap_or_else(|| {
                if let Some((agent_id, _a)) = self.agents.iter().find(|(_, a)| a.enabled) {
                    return agent_id.as_str();
                }
                if self.providers.contains_key("openrouter") {
                    return "openrouter";
                }
                "openai"
            })
    }

    /// Updates active harness, model, and adapter on disk in `.kobold/settings.json`.
    pub fn update_harness_and_model(
        &mut self,
        root: &Path,
        harness: &str,
        model: &str,
    ) -> std::io::Result<()> {
        let canonical = crate::catalog::normalize_harness(harness).unwrap_or(harness);
        self.current_harness = Some(canonical.to_string());
        self.model = model.to_string();

        // Update adapter settings according to chosen harness
        match canonical {
            crate::catalog::HARNESS_CLAUDE_CODE => {
                self.adapter = "kobold-adapter-acp".to_string();
                self.adapter_args = vec!["--agent-cmd".into(), "claude".into()];
                self.adapter_allow = Vec::new();
            }
            crate::catalog::HARNESS_GROK_BUILD => {
                self.adapter = "kobold-adapter-acp".to_string();
                self.adapter_args = vec!["--agent-cmd".into(), "grok".into()];
                self.adapter_allow = Vec::new();
            }
            crate::catalog::HARNESS_CODEX => {
                self.adapter = "kobold-adapter-acp".to_string();
                self.adapter_args = vec!["--agent-cmd".into(), "codex".into()];
                self.adapter_allow = Vec::new();
            }
            crate::catalog::HARNESS_OPENCODE => {
                self.adapter = "kobold-adapter-acp".to_string();
                self.adapter_args = vec!["--agent-cmd".into(), "opencode".into()];
                self.adapter_allow = Vec::new();
            }
            crate::catalog::HARNESS_ANTIGRAVITY => {
                self.adapter = "kobold-adapter-acp".to_string();
                self.adapter_args = vec!["--agent-cmd".into(), "agy".into()];
                self.adapter_allow = Vec::new();
            }
            crate::catalog::HARNESS_OPENROUTER => {
                self.adapter = "kobold-openai".to_string();
                self.adapter_args = Vec::new();
                self.adapter_allow = vec!["openrouter.ai".to_string()];
            }
            crate::catalog::HARNESS_OPENAI => {
                self.adapter = "kobold-openai".to_string();
                self.adapter_args = Vec::new();
                self.adapter_allow = vec!["api.openai.com".to_string()];
            }
            _ => {
                self.adapter = "kobold-openai".to_string();
                self.adapter_args = Vec::new();
                self.adapter_allow = vec!["api.openai.com".to_string()];
            }
        }

        self.save(root)
    }
}

impl Default for Voice {
    fn default() -> Self {
        Self {
            enabled: false,
            command: String::new(),
            voice: "alba".to_owned(),
            audio_addr: String::new(),
        }
    }
}

impl Settings {
    /// Load from `<root>/.kobold/settings.json`, then apply environment
    /// overrides. Returns the settings and any note worth showing the user.
    pub fn load(root: &Path) -> (Self, Option<String>) {
        let path = root.join(crate::transcript::DIR).join(FILE);
        let (mut settings, note) = match std::fs::read(&path) {
            Err(_) => (Self::default(), None),
            Ok(bytes) => match crate::json::from_slice::<Self>(&bytes) {
                Ok(s) => (s, None),
                Err(e) => (
                    Self::default(),
                    Some(format!("{}: {e}; using defaults", path.display())),
                ),
            },
        };
        settings.apply_env();
        (settings, note)
    }

    fn apply_env(&mut self) {
        if let Ok(v) = std::env::var("KOBOLD_MODEL") {
            if !v.is_empty() {
                self.model = v;
            }
        }
        if let Ok(v) = std::env::var("KOBOLD_EFFORT") {
            if !v.is_empty() {
                self.reasoning_effort = v;
            }
        }
        if let Ok(v) = std::env::var("KOBOLD_CONTEXT_WINDOW") {
            if let Ok(n) = v.trim().parse() {
                self.context_window = n;
            }
        }
        if let Ok(v) = std::env::var("KOBOLD_TTS_CMD") {
            if !v.is_empty() {
                self.voice.command = v;
            }
        }
        if let Ok(v) = std::env::var("KOBOLD_TTS_VOICE") {
            if !v.is_empty() {
                self.voice.voice = v;
            }
        }
        if let Ok(v) = std::env::var("KOBOLD_AUDIO_ADDR") {
            if !v.is_empty() {
                self.voice.audio_addr = v;
            }
        }
        if let Ok(v) = std::env::var("KOBOLD_CODE_BG") {
            if let Ok(n) = v.parse() {
                self.code_bg = n;
            }
        }
    }

    /// Write defaults if no file exists yet, so there is something to edit.
    pub fn ensure_file(root: &Path) -> std::io::Result<()> {
        let dir = root.join(crate::transcript::DIR);
        let path = dir.join(FILE);
        if path.exists() {
            return Ok(());
        }
        std::fs::create_dir_all(&dir)?;
        let json = serde_json::to_string_pretty(&Self::default())
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        std::fs::write(path, json)
    }

    /// Save full settings to `<root>/.kobold/settings.json`.
    pub fn save(&self, root: &Path) -> std::io::Result<()> {
        let dir = root.join(crate::transcript::DIR);
        let path = dir.join(FILE);
        std::fs::create_dir_all(&dir)?;
        let json =
            serde_json::to_string_pretty(self).map_err(|e| std::io::Error::other(e.to_string()))?;
        std::fs::write(path, json)
    }

    /// Update voice configuration on disk in `<root>/.kobold/settings.json`,
    /// preserving any other existing settings.
    pub fn update_voice(
        root: &Path,
        enabled: bool,
        voice_name: Option<&str>,
    ) -> std::io::Result<()> {
        let dir = root.join(crate::transcript::DIR);
        let path = dir.join(FILE);
        std::fs::create_dir_all(&dir)?;

        let mut val: serde_json::Value = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|_| serde_json::json!({})),
            Err(_) => serde_json::json!({}),
        };

        if !val.is_object() {
            val = serde_json::json!({});
        }

        let obj = val.as_object_mut().expect("checked is_object");
        let voice_val = obj.entry("voice").or_insert_with(|| serde_json::json!({}));
        if !voice_val.is_object() {
            *voice_val = serde_json::json!({});
        }
        let voice_obj = voice_val.as_object_mut().expect("checked is_object");
        voice_obj.insert("enabled".to_string(), serde_json::Value::Bool(enabled));
        if let Some(name) = voice_name {
            voice_obj.insert(
                "voice".to_string(),
                serde_json::Value::String(name.to_string()),
            );
        }

        let formatted =
            serde_json::to_string_pretty(&val).map_err(|e| std::io::Error::other(e.to_string()))?;
        std::fs::write(path, formatted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Per-test directory: tests run in parallel in one process, so a shared
    /// path makes them clobber each other and fail non-deterministically.
    fn temp(name: &str) -> std::path::PathBuf {
        let base = std::env::temp_dir().join(format!("kobold-set-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join(crate::transcript::DIR)).unwrap();
        base
    }

    #[test]
    fn missing_file_yields_defaults_without_error() {
        let (s, note) = Settings::load(std::path::Path::new("/nonexistent"));
        assert_eq!(s, Settings::default());
        assert!(note.is_none(), "a missing file is normal, not worth a note");
    }

    #[test]
    fn partial_file_fills_the_rest_from_defaults() {
        let root = temp("partial");
        std::fs::write(
            root.join(crate::transcript::DIR).join(FILE),
            br#"{"model":"gpt-5.6-sol"}"#,
        )
        .unwrap();
        let (s, note) = Settings::load(&root);
        assert!(note.is_none());
        assert_eq!(s.model, "gpt-5.6-sol");
        assert_eq!(s.reasoning_effort, Settings::default().reasoning_effort);
        assert_eq!(s.code_bg, Settings::default().code_bg);
    }

    #[test]
    fn malformed_file_degrades_instead_of_failing() {
        let root = temp("malformed");
        std::fs::write(root.join(crate::transcript::DIR).join(FILE), b"{not json").unwrap();
        let (s, note) = Settings::load(&root);
        assert_eq!(s.model, Settings::default().model);
        assert!(
            note.is_some(),
            "the user needs to know their file was ignored"
        );
    }

    #[test]
    fn round_trips_through_json() {
        let mut s = Settings::default();
        s.voice.enabled = true;
        s.voice.command = "/usr/bin/say".into();
        let text = crate::json::to_string(&s).unwrap();
        let back: Settings = crate::json::from_slice(text.as_bytes()).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn ensure_file_does_not_clobber_an_existing_one() {
        let root = temp("ensure");
        let path = root.join(crate::transcript::DIR).join(FILE);
        std::fs::write(&path, br#"{"model":"mine"}"#).unwrap();
        Settings::ensure_file(&root).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            r#"{"model":"mine"}"#
        );
    }

    #[test]
    fn update_voice_persists_enabled_and_voice_without_clobbering() {
        let root = temp("update-voice");
        let path = root.join(crate::transcript::DIR).join(FILE);
        std::fs::write(&path, br#"{"model":"custom-model"}"#).unwrap();

        // 1. Update voice to enabled with specific voice
        Settings::update_voice(&root, true, Some("fantine")).unwrap();
        let (s, note) = Settings::load(&root);
        assert!(note.is_none());
        assert_eq!(s.model, "custom-model", "other settings must be preserved");
        assert!(s.voice.enabled, "voice.enabled must be true");
        assert_eq!(s.voice.voice, "fantine", "voice.voice must be fantine");

        // 2. Update voice to disabled without changing voice persona
        Settings::update_voice(&root, false, None).unwrap();
        let (s, _) = Settings::load(&root);
        assert_eq!(s.model, "custom-model");
        assert!(!s.voice.enabled, "voice.enabled must be false");
        assert_eq!(
            s.voice.voice, "fantine",
            "voice.voice should remain fantine"
        );

        // 3. Toggle back on without specifying persona
        Settings::update_voice(&root, true, None).unwrap();
        let (s, _) = Settings::load(&root);
        assert!(s.voice.enabled, "voice.enabled must be true again");
        assert_eq!(
            s.voice.voice, "fantine",
            "voice.voice must still be fantine"
        );
    }
}
