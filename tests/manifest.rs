//! The manifests stay valid TOML on every toolchain, not just the pinned one.
//!
//! A comment sat inside an inline table in `Cargo.toml` for months. TOML has
//! never allowed it -- an inline table must be one line and cannot carry a
//! comment -- but cargo 1.97.1 accepts it and 1.93.1 refuses to parse the
//! manifest at all, so it built fine on one machine and broke the other.
//!
//! Pinning the toolchain stops the two ends disagreeing, but it does not stop
//! this: the pin is the lenient version, so a new one of these would now go
//! unnoticed everywhere until someone built outside the pin. Hence a check that
//! does not depend on which cargo is parsing.

use std::path::{Path, PathBuf};

/// Every `Cargo.toml` in the workspace, `target` excluded.
fn manifests() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            // Vendored and built artefacts are not ours to police, and `target`
            // holds thousands of manifests from the registry.
            if name == "target" || name == ".git" {
                continue;
            }
            if path.is_dir() {
                walk(&path, out);
            } else if name == "Cargo.toml" {
                out.push(path);
            }
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut out = Vec::new();
    walk(root, &mut out);
    out.sort();
    out
}

#[test]
fn no_manifest_puts_a_comment_or_a_line_break_inside_an_inline_table() {
    let found = manifests();
    assert!(!found.is_empty(), "found no manifests to check");

    let mut problems = Vec::new();
    for path in &found {
        let text = std::fs::read_to_string(path).expect("manifest is readable");
        // Depth of nested `{ }`. Only braces outside a comment count, and a
        // string containing a brace would confuse this -- there are none in
        // these manifests, and a false positive is a readable failure rather
        // than a silent pass, which is the right way for it to be wrong.
        let mut depth = 0usize;
        for (n, line) in text.lines().enumerate() {
            let code = line.split('#').next().unwrap_or("");
            if depth > 0 && line.contains('#') {
                problems.push(format!(
                    "{}:{}: comment inside an inline table",
                    path.display(),
                    n + 1
                ));
            }
            let (opens, closes) = (code.matches('{').count(), code.matches('}').count());
            if depth == 0 && opens > closes {
                problems.push(format!(
                    "{}:{}: inline table spans lines",
                    path.display(),
                    n + 1
                ));
            }
            depth = (depth + opens).saturating_sub(closes);
        }
    }

    assert!(
        problems.is_empty(),
        "invalid TOML that some cargo versions accept:\n  {}",
        problems.join("\n  ")
    );
}

#[test]
fn the_toolchain_is_pinned_and_the_floor_is_not_above_it() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let pin = std::fs::read_to_string(dir.join("rust-toolchain.toml"))
        .expect("rust-toolchain.toml exists, or the two ends drift apart again");
    let channel = pin
        .lines()
        .find_map(|l| l.trim().strip_prefix("channel = "))
        .map(|v| v.trim_matches(['"', ' ']).to_owned())
        .expect("the pin names a channel");

    let manifest = std::fs::read_to_string(dir.join("Cargo.toml")).expect("manifest is readable");
    let floor = manifest
        .lines()
        .find_map(|l| l.trim().strip_prefix("rust-version = "))
        .map(|v| v.trim_matches(['"', ' ']).to_owned())
        .expect("the manifest names a minimum");

    // A floor above the pin would mean the pinned toolchain cannot build the
    // crate it is pinned for, which is a contradiction rather than a policy.
    let parts = |v: &str| {
        let mut it = v.split('.').filter_map(|p| p.parse::<u32>().ok());
        (it.next().unwrap_or(0), it.next().unwrap_or(0))
    };
    assert!(
        parts(&floor) <= parts(&channel),
        "rust-version {floor} is above the pinned toolchain {channel}"
    );
}
