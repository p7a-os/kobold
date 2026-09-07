//! Models and harnesses catalog, cache management, and model matching.
//!
//! Provides model lists for all supported harnesses (Claude Code, Grok Build,
//! Codex, OpenCode, Antigravity, OpenAI, OpenRouter), handles live model discovery,
//! and resolves `/model <name>[@provider]` and `/backend <name>` commands.

use std::collections::BTreeMap;
use std::process::Command;

/// Canonical harness identifiers.
pub const HARNESS_CLAUDE_CODE: &str = "claude-code";
pub const HARNESS_GROK_BUILD: &str = "grok-build";
pub const HARNESS_CODEX: &str = "codex";
pub const HARNESS_OPENCODE: &str = "opencode";
pub const HARNESS_ANTIGRAVITY: &str = "antigravity";
pub const HARNESS_OPENAI: &str = "openai";
pub const HARNESS_OPENROUTER: &str = "openrouter";

/// All supported harnesses in display order.
pub const ALL_HARNESSES: &[&str] = &[
    HARNESS_CLAUDE_CODE,
    HARNESS_GROK_BUILD,
    HARNESS_CODEX,
    HARNESS_OPENCODE,
    HARNESS_ANTIGRAVITY,
    HARNESS_OPENAI,
    HARNESS_OPENROUTER,
];

/// Normalize a user-supplied harness or provider name into its canonical identifier.
pub fn normalize_harness(name: &str) -> Option<&'static str> {
    let lower = name.trim().to_ascii_lowercase().replace('_', "-");
    match lower.as_str() {
        "claude-code" | "claude" | "anthropic" => Some(HARNESS_CLAUDE_CODE),
        "grok-build" | "grok" | "xai" => Some(HARNESS_GROK_BUILD),
        "codex" | "codex-cli" => Some(HARNESS_CODEX),
        "opencode" | "open-code" => Some(HARNESS_OPENCODE),
        "antigravity" | "agy" => Some(HARNESS_ANTIGRAVITY),
        "openai" => Some(HARNESS_OPENAI),
        "openrouter" | "open-router" => Some(HARNESS_OPENROUTER),
        _ => None,
    }
}

/// Human-friendly display title for a harness.
pub fn harness_title(harness: &str) -> &'static str {
    match harness {
        HARNESS_CLAUDE_CODE => "Claude Code",
        HARNESS_GROK_BUILD => "Grok Build",
        HARNESS_CODEX => "Codex",
        HARNESS_OPENCODE => "OpenCode",
        HARNESS_ANTIGRAVITY => "Antigravity",
        HARNESS_OPENAI => "OpenAI",
        HARNESS_OPENROUTER => "OpenRouter",
        _ => "Custom Agent",
    }
}

/// Executable command name for an agent harness.
pub fn harness_command(harness: &str) -> Option<&'static str> {
    match harness {
        HARNESS_CLAUDE_CODE => Some("claude"),
        HARNESS_GROK_BUILD => Some("grok"),
        HARNESS_CODEX => Some("codex"),
        HARNESS_OPENCODE => Some("opencode"),
        HARNESS_ANTIGRAVITY => Some("agy"),
        _ => None,
    }
}

/// Default model for a given harness.
pub fn default_model_for_harness(harness: &str) -> &'static str {
    match harness {
        HARNESS_CLAUDE_CODE => "sonnet-5",
        HARNESS_GROK_BUILD => "grok-4.6",
        HARNESS_CODEX => "gpt-5.6-luna",
        HARNESS_OPENCODE => "sonnet-5",
        HARNESS_ANTIGRAVITY => "gemini-2.5-pro",
        HARNESS_OPENAI => "gpt-5.6-luna",
        HARNESS_OPENROUTER => "openai/gpt-6-astra",
        _ => "default",
    }
}

/// Curated baseline models supported by each harness.
pub fn baseline_models(harness: &str) -> Vec<String> {
    match harness {
        HARNESS_CLAUDE_CODE => vec![
            "haiku-4.5".into(),
            "sonnet-5".into(),
            "opus-5".into(),
            "fable-5.1".into(),
        ],
        HARNESS_GROK_BUILD => vec!["grok-4.5".into(), "grok-4.6".into()],
        HARNESS_CODEX => vec![
            "gpt-5.6-luna".into(),
            "gpt-5.6-terra".into(),
            "gpt-5.6-sol".into(),
            "gpt-6-astra".into(),
        ],
        HARNESS_OPENCODE => vec![
            "opus-5".into(),
            "sonnet-5".into(),
            "haiku-4.5".into(),
            "gpt-5.6-luna".into(),
        ],
        HARNESS_ANTIGRAVITY => vec![
            "gemini-2.5-pro".into(),
            "gemini-2.5-flash".into(),
            "gemini-2.5-flash-thinking".into(),
        ],
        HARNESS_OPENAI => vec![
            "gpt-5.6-luna".into(),
            "gpt-5.6-terra".into(),
            "gpt-5.6-sol".into(),
            "gpt-6-astra".into(),
        ],
        HARNESS_OPENROUTER => vec![
            "openai/gpt-6-astra".into(),
            "openai/gpt-5.6-luna".into(),
            "anthropic/claude-3.5-sonnet".into(),
            "anthropic/claude-3.7-sonnet".into(),
            "deepseek/deepseek-r1".into(),
            "google/gemini-2.5-pro".into(),
            "meta-llama/llama-3.3-70b-instruct".into(),
        ],
        _ => Vec::new(),
    }
}

/// Dynamically probe models from local CLIs if installed and responding.
pub fn discover_models_for_harness(harness: &str) -> Vec<String> {
    let mut models = baseline_models(harness);

    match harness {
        HARNESS_GROK_BUILD => {
            if let Ok(out) = Command::new("grok").arg("models").output() {
                if out.status.success() {
                    let text = String::from_utf8_lossy(&out.stdout);
                    for line in text.lines() {
                        let trimmed = line.trim();
                        if let Some(m) = trimmed.strip_prefix('*') {
                            let name = m.split_whitespace().next().unwrap_or("").trim();
                            if !name.is_empty() && !models.iter().any(|x| x == name) {
                                models.push(name.to_string());
                            }
                        } else if let Some(m) = trimmed.strip_prefix('-') {
                            let name = m.split_whitespace().next().unwrap_or("").trim();
                            if !name.is_empty() && !models.iter().any(|x| x == name) {
                                models.push(name.to_string());
                            }
                        }
                    }
                }
            }
        }
        HARNESS_ANTIGRAVITY => {
            if let Ok(out) = Command::new("agy").arg("models").output() {
                if out.status.success() {
                    let text = String::from_utf8_lossy(&out.stdout);
                    for line in text.lines() {
                        let trimmed = line.trim();
                        if !trimmed.is_empty()
                            && !trimmed.contains("Available")
                            && !trimmed.contains("Usage")
                            && !trimmed.starts_with('-')
                        {
                            let name = trimmed.split_whitespace().next().unwrap_or("").trim();
                            if !name.is_empty() && !models.iter().any(|x| x == name) {
                                models.push(name.to_string());
                            }
                        }
                    }
                }
            }
        }
        HARNESS_OPENCODE => {
            if let Ok(out) = Command::new("opencode").arg("models").output() {
                if out.status.success() {
                    let text = String::from_utf8_lossy(&out.stdout);
                    for line in text.lines() {
                        let trimmed = line.trim();
                        if !trimmed.is_empty() && !trimmed.contains("Models") {
                            let name = trimmed.split_whitespace().next().unwrap_or("").trim();
                            if !name.is_empty() && !models.iter().any(|x| x == name) {
                                models.push(name.to_string());
                            }
                        }
                    }
                }
            }
        }
        _ => {}
    }

    models
}

/// Builds a complete models cache map for the given list of harnesses.
pub fn build_models_cache(harnesses: &[&str]) -> BTreeMap<String, Vec<String>> {
    let mut map = BTreeMap::new();
    for h in harnesses {
        let models = discover_models_for_harness(h);
        map.insert((*h).to_string(), models);
    }
    map
}

/// Matches a model specification entered by the user.
///
/// Supports:
/// - `<model>` (e.g. `gpt-5.6-luna`, `grok-4.6`, `sonnet-5`)
/// - `<model>@<provider>` (e.g. `gpt-5.6-luna@openai`, `opus-5@claude-code`, `grok-4.6@grok-build`)
///
/// When backend is not specified, tries to match model in the current in-use harness.
/// If not available, searches other harnesses and switches both harness and model.
///
/// Returns `Ok((harness, model_name))` or `Err(explanation)`.
pub fn match_model(
    models_cache: &BTreeMap<String, Vec<String>>,
    current_harness: Option<&str>,
    spec: &str,
) -> Result<(String, String), String> {
    let trimmed = spec.trim();
    if trimmed.is_empty() {
        return Err("no model specified".into());
    }

    // Explicit @provider syntax: e.g. "opus-5@claude-code"
    if let Some((model_part, provider_part)) = trimmed.split_once('@') {
        let model_name = model_part.trim();
        let provider_str = provider_part.trim();
        if model_name.is_empty() {
            return Err("model name before '@' cannot be empty".into());
        }
        if provider_str.is_empty() {
            return Err("provider name after '@' cannot be empty".into());
        }

        let canonical_harness = normalize_harness(provider_str).ok_or_else(|| {
            format!(
                "unknown provider '{provider_str}'. Available: {}",
                ALL_HARNESSES.join(", ")
            )
        })?;

        return Ok((canonical_harness.to_string(), model_name.to_string()));
    }

    // Bare model name: check current harness first
    let curr = current_harness
        .and_then(normalize_harness)
        .unwrap_or(HARNESS_OPENAI);

    if let Some(curr_models) = models_cache.get(curr) {
        if curr_models
            .iter()
            .any(|m| m.eq_ignore_ascii_case(trimmed) || m.ends_with(&format!("/{trimmed}")))
        {
            return Ok((curr.to_string(), trimmed.to_string()));
        }
    }

    // If not found in current harness, search all other harnesses in cache
    for (harness, models) in models_cache {
        if harness == curr {
            continue;
        }
        if models
            .iter()
            .any(|m| m.eq_ignore_ascii_case(trimmed) || m.ends_with(&format!("/{trimmed}")))
        {
            return Ok((harness.clone(), trimmed.to_string()));
        }
    }

    // If still not matched, check baseline models across all harnesses
    for h in ALL_HARNESSES {
        if *h == curr {
            continue;
        }
        let baselines = baseline_models(h);
        if baselines
            .iter()
            .any(|m| m.eq_ignore_ascii_case(trimmed) || m.ends_with(&format!("/{trimmed}")))
        {
            return Ok(((*h).to_string(), trimmed.to_string()));
        }
    }

    // Fall back to setting model on current harness if no other harness matched
    Ok((curr.to_string(), trimmed.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_harness_handles_aliases() {
        assert_eq!(normalize_harness("claude"), Some(HARNESS_CLAUDE_CODE));
        assert_eq!(normalize_harness("claude-code"), Some(HARNESS_CLAUDE_CODE));
        assert_eq!(normalize_harness("anthropic"), Some(HARNESS_CLAUDE_CODE));
        assert_eq!(normalize_harness("grok"), Some(HARNESS_GROK_BUILD));
        assert_eq!(normalize_harness("grok-build"), Some(HARNESS_GROK_BUILD));
        assert_eq!(normalize_harness("xai"), Some(HARNESS_GROK_BUILD));
        assert_eq!(normalize_harness("codex"), Some(HARNESS_CODEX));
        assert_eq!(normalize_harness("opencode"), Some(HARNESS_OPENCODE));
        assert_eq!(normalize_harness("agy"), Some(HARNESS_ANTIGRAVITY));
        assert_eq!(normalize_harness("antigravity"), Some(HARNESS_ANTIGRAVITY));
        assert_eq!(normalize_harness("openai"), Some(HARNESS_OPENAI));
        assert_eq!(normalize_harness("openrouter"), Some(HARNESS_OPENROUTER));
        assert_eq!(normalize_harness("unknown"), None);
    }

    #[test]
    fn match_model_with_explicit_provider() {
        let cache = build_models_cache(ALL_HARNESSES);
        let (h, m) =
            match_model(&cache, Some("openai"), "opus-5@claude-code").expect("match explicit");
        assert_eq!(h, HARNESS_CLAUDE_CODE);
        assert_eq!(m, "opus-5");

        let (h2, m2) =
            match_model(&cache, Some("claude-code"), "grok-4.6@grok-build").expect("match grok");
        assert_eq!(h2, HARNESS_GROK_BUILD);
        assert_eq!(m2, "grok-4.6");
    }

    #[test]
    fn match_model_in_current_harness() {
        let cache = build_models_cache(ALL_HARNESSES);
        let (h, m) =
            match_model(&cache, Some("claude-code"), "sonnet-5").expect("match in current");
        assert_eq!(h, HARNESS_CLAUDE_CODE);
        assert_eq!(m, "sonnet-5");
    }

    #[test]
    fn match_model_cross_harness_switches_both() {
        let cache = build_models_cache(ALL_HARNESSES);
        // Current is claude-code, but grok-4.6 belongs to grok-build:
        let (h, m) = match_model(&cache, Some("claude-code"), "grok-4.6").expect("cross harness");
        assert_eq!(h, HARNESS_GROK_BUILD);
        assert_eq!(m, "grok-4.6");

        // Current is grok-build, but opus-5 belongs to claude-code:
        let (h2, m2) = match_model(&cache, Some("grok-build"), "opus-5").expect("cross harness");
        assert_eq!(h2, HARNESS_CLAUDE_CODE);
        assert_eq!(m2, "opus-5");
    }
}
