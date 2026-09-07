//! What a child process is given from Kobold's environment.
//!
//! One list, used by every spawn Kobold makes, because two allowlists that
//! drift apart is how one of them quietly stops matching the reasoning that
//! justified it.
//!
//! The child does not inherit. Kobold's own environment is where
//! `LLM_API_KEY` lives -- `run.sh` puts it there -- so anything spawned from
//! here would otherwise receive the user's credentials along with whatever
//! else the shell exported. That was a live defect for MCP servers and is
//! fixed; this is the same guard applied at the one other place Kobold
//! starts a process.
//!
//! **For the adapter it is what makes the credential-on-stdin decision mean
//! anything.** The key is handed over on stdin specifically so it is not in
//! an environment that `/proc/PID/environ` exposes, that grandchildren
//! inherit, and that crash dumps capture -- and none of that holds if the
//! child inherits `LLM_API_KEY` anyway.
//!
//! Each entry is a decision:
//!
//! - `PATH` -- a child invoked as `npx` or `python3` cannot find its own
//!   interpreter without it, which is most MCP servers.
//! - `HOME` -- where language runtimes look for their per-user caches;
//!   without it npm and pip pick surprising defaults.
//! - `USER` -- read by enough tooling to be worth passing, and not a secret.
//! - `TMPDIR` -- without it a child writes to `/tmp` regardless of what the
//!   platform or the user configured.
//! - `LANG`, and `LC_*` by prefix -- text handling. A child that decodes
//!   UTF-8 differently because the locale vanished is a confusing failure.
//!
//! A confined adapter will want a stricter list than an MCP server the user
//! chose to install -- an adapter denied the network has little legitimate
//! need for a rich environment. That is the confinement work's call, decided
//! fresh rather than inherited from here.

const BASE: &[&str] = &["PATH", "HOME", "USER", "TMPDIR", "LANG"];

/// Clear the child's environment and put back only the base list.
///
/// Must be called before any `env()` the caller adds, or `env_clear` wipes
/// those out again and the whole thing silently passes nothing.
pub fn restrict(cmd: &mut tokio::process::Command) {
    cmd.env_clear();
    for key in BASE {
        if let Some(value) = std::env::var_os(key) {
            cmd.env(key, value);
        }
    }
    // Locale and adapter variables are matched by prefix.
    for (key, value) in std::env::vars_os() {
        let name = key.to_string_lossy();
        if name.starts_with("LC_") || name.starts_with("ACP_") {
            cmd.env(key, value);
        }
    }
}
