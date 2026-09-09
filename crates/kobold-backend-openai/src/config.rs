use std::path::PathBuf;
use std::time::Duration;

pub const DEFAULT_OPENAI_WS_URL: &str = "wss://api.openai.com/v1/responses";
pub const DEFAULT_MODEL: &str = "gpt-5.6-luna";
pub const DEFAULT_REASONING_EFFORT: &str = "low";
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// Configuration settings for the OpenAI Responses WebSocket backend.
#[derive(Debug, Clone)]
pub struct OpenAiConfig {
    /// Secret API key for OpenAI authentication.
    pub api_key: String,

    /// Target model identifier (default: "gpt-5.6-luna").
    pub model: String,

    /// WebSocket endpoint URL.
    pub base_url: String,

    /// Optional reasoning effort level ("none", "low", "medium", "high", "max").
    pub reasoning_effort: Option<String>,

    /// Zero Data Retention toggle: when true, `store` is set to `false`.
    pub zdr_enabled: bool,

    /// Idle duration before dropping inactive WebSocket connections.
    pub idle_timeout: Duration,

    /// Optional Unix Domain Socket path for routing through Kobold's sandboxed egress broker.
    pub egress_socket: Option<PathBuf>,
}

impl OpenAiConfig {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            model: DEFAULT_MODEL.to_string(),
            base_url: DEFAULT_OPENAI_WS_URL.to_string(),
            reasoning_effort: Some(DEFAULT_REASONING_EFFORT.to_string()),
            zdr_enabled: false,
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            egress_socket: None,
        }
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    pub fn with_reasoning_effort(mut self, effort: impl Into<String>) -> Self {
        self.reasoning_effort = Some(effort.into());
        self
    }

    pub fn with_zdr(mut self, zdr_enabled: bool) -> Self {
        self.zdr_enabled = zdr_enabled;
        self
    }

    pub fn with_idle_timeout(mut self, timeout: Duration) -> Self {
        self.idle_timeout = timeout;
        self
    }

    pub fn with_egress_socket(mut self, socket_path: PathBuf) -> Self {
        self.egress_socket = Some(socket_path);
        self
    }
}
