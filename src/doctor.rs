//! Agent detection and parallel health checking (`kobold doctor`).
//!
//! Detects installed coding agents (Claude Code, Grok, Antigravity, Codex, OpenCode),
//! queries their versions, probes them with a lightweight prompt in parallel,
//! and writes results to `.kobold/settings.json`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::process::Command;

use crate::settings::{AgentSetting, Settings};

/// Information about a supported agent definition.
#[derive(Clone, Debug)]
pub struct AgentSpec {
    pub id: &'static str,
    pub name: &'static str,
    pub primary_bin: &'static str,
    pub fallback_bins: &'static [&'static str],
    pub probe_args: &'static [&'static str],
    pub version_args: &'static [&'static str],
}

pub const SUPPORTED_AGENTS: &[AgentSpec] = &[
    AgentSpec {
        id: "claude",
        name: "Claude Code",
        primary_bin: "claude",
        fallback_bins: &[],
        probe_args: &["-p", "Say hi"],
        version_args: &["--version"],
    },
    AgentSpec {
        id: "grok",
        name: "Grok",
        primary_bin: "grok",
        fallback_bins: &[],
        probe_args: &["-p", "Say hi"],
        version_args: &["--version"],
    },
    AgentSpec {
        id: "antigravity",
        name: "Antigravity",
        primary_bin: "agy",
        fallback_bins: &["antigravity"],
        probe_args: &["-p", "Say hi"],
        version_args: &["--version"],
    },
    AgentSpec {
        id: "codex",
        name: "Codex",
        primary_bin: "codex",
        fallback_bins: &[],
        probe_args: &["exec", "Say hi"],
        version_args: &["--version"],
    },
    AgentSpec {
        id: "opencode",
        name: "OpenCode",
        primary_bin: "opencode",
        fallback_bins: &[],
        probe_args: &["run", "Say hi"],
        version_args: &["--version"],
    },
];

/// Result of checking an agent.
#[derive(Clone, Debug)]
pub struct AgentHealth {
    pub id: String,
    pub name: String,
    pub command: String,
    pub path: Option<PathBuf>,
    pub version: Option<String>,
    pub detected: bool,
    pub working: bool,
    pub status: String,
    pub duration: Duration,
}

/// Find an executable on PATH or common candidate directories.
pub fn find_binary(bin: &str) -> Option<PathBuf> {
    if let Some(p) = kobold_core::sandbox::resolve(bin) {
        if p.is_file() {
            return Some(p);
        }
    }

    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join(bin);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    if let Ok(home) = std::env::var("HOME") {
        let home_path = PathBuf::from(home);
        let common = [
            home_path.join(".local/bin").join(bin),
            home_path.join(".cargo/bin").join(bin),
            home_path.join(".npm-global/bin").join(bin),
            PathBuf::from("/opt/homebrew/bin").join(bin),
            PathBuf::from("/usr/local/bin").join(bin),
            PathBuf::from("/usr/bin").join(bin),
        ];
        for candidate in common {
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    None
}

/// Resolve spec binary by checking primary then fallbacks.
pub fn resolve_spec_binary(spec: &AgentSpec) -> (String, Option<PathBuf>) {
    if let Some(p) = find_binary(spec.primary_bin) {
        return (spec.primary_bin.to_string(), Some(p));
    }
    for &fallback in spec.fallback_bins {
        if let Some(p) = find_binary(fallback) {
            return (fallback.to_string(), Some(p));
        }
    }
    (spec.primary_bin.to_string(), None)
}

/// Detect installed agents without running full probes.
pub async fn detect_all_agents() -> Vec<(AgentSpec, Option<PathBuf>, Option<String>)> {
    let mut results = Vec::new();
    for spec in SUPPORTED_AGENTS {
        let (_cmd, path) = resolve_spec_binary(spec);
        let version = if let Some(ref p) = path {
            query_version(p, spec.version_args).await
        } else {
            None
        };
        results.push((spec.clone(), path, version));
    }
    results
}

/// Query an agent's version string.
pub async fn query_version(path: &Path, args: &[&str]) -> Option<String> {
    let mut cmd = Command::new(path);
    cmd.args(args);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let child = cmd.spawn().ok()?;
    let output = tokio::time::timeout(Duration::from_secs(3), child.wait_with_output())
        .await
        .ok()?
        .ok()?;

    if output.status.success() {
        let out = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !out.is_empty() {
            let first_line = out.lines().next().unwrap_or("").trim().to_string();
            return Some(first_line);
        }
    }
    None
}

/// Check one agent with a minimal probe request and timeout.
pub async fn probe_agent(spec: AgentSpec, timeout: Duration) -> AgentHealth {
    let start = Instant::now();
    let (cmd_name, path) = resolve_spec_binary(&spec);

    let Some(bin_path) = path.clone() else {
        return AgentHealth {
            id: spec.id.to_string(),
            name: spec.name.to_string(),
            command: cmd_name,
            path: None,
            version: None,
            detected: false,
            working: false,
            status: "Not found on PATH".to_string(),
            duration: start.elapsed(),
        };
    };

    let version = query_version(&bin_path, spec.version_args).await;

    let mut cmd = Command::new(&bin_path);
    cmd.args(spec.probe_args);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let probe_res = tokio::time::timeout(timeout, async {
        let child = cmd.spawn()?;
        child.wait_with_output().await
    })
    .await;

    let duration = start.elapsed();

    match probe_res {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let combined = format!("{stdout} {stderr}").trim().to_string();

            if output.status.success() {
                let note = if let Some(ref ver) = version {
                    format!("OK ({ver}, {:.1}s)", duration.as_secs_f32())
                } else {
                    format!("OK ({:.1}s)", duration.as_secs_f32())
                };
                AgentHealth {
                    id: spec.id.to_string(),
                    name: spec.name.to_string(),
                    command: cmd_name,
                    path: Some(bin_path),
                    version,
                    detected: true,
                    working: true,
                    status: note,
                    duration,
                }
            } else {
                let err_detail = if !combined.is_empty() {
                    combined
                        .lines()
                        .next()
                        .unwrap_or("error")
                        .chars()
                        .take(60)
                        .collect::<String>()
                } else {
                    format!("exit code {:?}", output.status.code())
                };
                AgentHealth {
                    id: spec.id.to_string(),
                    name: spec.name.to_string(),
                    command: cmd_name,
                    path: Some(bin_path),
                    version,
                    detected: true,
                    working: false,
                    status: format!("Failed: {err_detail}"),
                    duration,
                }
            }
        }
        Ok(Err(e)) => AgentHealth {
            id: spec.id.to_string(),
            name: spec.name.to_string(),
            command: cmd_name,
            path: Some(bin_path),
            version,
            detected: true,
            working: false,
            status: format!("Spawn error: {e}"),
            duration,
        },
        Err(_) => AgentHealth {
            id: spec.id.to_string(),
            name: spec.name.to_string(),
            command: cmd_name,
            path: Some(bin_path),
            version,
            detected: true,
            working: false,
            status: format!("Timed out after {}s", timeout.as_secs()),
            duration,
        },
    }
}

/// Run health checks in parallel across all specified agents.
pub async fn check_all_agents(specs: &[AgentSpec], timeout: Duration) -> Vec<AgentHealth> {
    let mut tasks = Vec::new();
    for spec in specs {
        let s = spec.clone();
        tasks.push(tokio::spawn(async move { probe_agent(s, timeout).await }));
    }

    let mut results = Vec::new();
    for t in tasks {
        if let Ok(h) = t.await {
            results.push(h);
        }
    }
    results
}

/// Save health check results into `.kobold/settings.json`.
pub fn update_settings_with_health(
    root: &Path,
    results: &[AgentHealth],
) -> std::io::Result<Settings> {
    let (mut settings, _) = Settings::load(root);
    for h in results {
        settings.agents.insert(
            h.id.clone(),
            AgentSetting {
                name: h.name.clone(),
                command: h.command.clone(),
                enabled: h.detected && h.working,
                detected: h.detected,
                working: h.working,
                status: h.status.clone(),
            },
        );
    }
    settings.save(root)?;
    Ok(settings)
}

/// Print formatted doctor output to stdout.
pub fn print_doctor_report(results: &[AgentHealth]) {
    println!("\n\x1b[1mKobold Agent Doctor\x1b[0m");
    println!("Checking agent connectivity and execution status in parallel:\n");
    println!(
        "  {:<16} {:<8} {:<32} {:<30}",
        "AGENT", "STATUS", "PATH", "DETAILS"
    );
    println!("  {}", "-".repeat(90));

    let mut any_working = false;
    for h in results {
        let status_mark = if h.working {
            any_working = true;
            "\x1b[32m✓ OK\x1b[0m"
        } else if h.detected {
            "\x1b[33m✗ FAIL\x1b[0m"
        } else {
            "\x1b[90m- MISSING\x1b[0m"
        };

        let path_str = h
            .path
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "not found".to_string());

        let short_path = if path_str.len() > 30 {
            format!("...{}", &path_str[path_str.len() - 27..])
        } else {
            path_str
        };

        println!(
            "  {:<16} {:<17} {:<32} {}",
            h.name, status_mark, short_path, h.status
        );
    }
    println!("  {}", "-".repeat(90));

    if any_working {
        println!("\x1b[32m✓ At least one agent is operational and ready to supervise.\x1b[0m\n");
    } else {
        println!("\x1b[33m! No active agents passed verification. You can also configure direct LLM providers.\x1b[0m\n");
    }
}

/// Entry point for `kobold doctor` command.
pub async fn run_doctor(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    println!("kobold doctor: probing agents in parallel...");
    let results = check_all_agents(SUPPORTED_AGENTS, Duration::from_secs(12)).await;
    print_doctor_report(&results);
    let _ = update_settings_with_health(root, &results);
    println!(
        "Saved agent status to {}/.kobold/settings.json",
        root.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specs_contain_all_required_agents() {
        let ids: Vec<&str> = SUPPORTED_AGENTS.iter().map(|s| s.id).collect();
        assert!(ids.contains(&"claude"));
        assert!(ids.contains(&"grok"));
        assert!(ids.contains(&"antigravity"));
        assert!(ids.contains(&"codex"));
        assert!(ids.contains(&"opencode"));
    }

    #[tokio::test]
    async fn find_binary_finds_sh() {
        let p = find_binary("sh");
        assert!(p.is_some());
    }
}
