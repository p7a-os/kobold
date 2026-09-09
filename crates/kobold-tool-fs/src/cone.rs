use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

/// Maximum file bytes allowed to be returned by read operations before truncation.
pub const MAX_READ_BYTES: usize = 64 * 1024;

/// Error message returned when a requested path attempts to escape the root cone.
pub fn outside_cone_error(requested: &str) -> String {
    format!("'{requested}' is outside the working directory cone")
}

/// Lexically normalise path components without consulting the filesystem.
/// Symlinks are intentionally not followed here.
pub fn normalise_path(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Settle a path's ancestor spelling against the filesystem and reason about the rest lexically.
/// Resolves symlinked prefixes while preventing existence probing of unreached leaves.
pub fn resolve_ancestors(p: &Path) -> PathBuf {
    let lexical = normalise_path(p);
    let mut trailing: Vec<OsString> = Vec::new();
    let mut cur = lexical.clone();

    loop {
        if let Ok(real) = cur.canonicalize() {
            let mut out = real;
            for part in trailing.iter().rev() {
                out.push(part);
            }
            return normalise_path(&out);
        }
        let (Some(name), Some(parent)) = (cur.file_name(), cur.parent()) else {
            return lexical;
        };
        trailing.push(name.to_owned());
        cur = parent.to_path_buf();
    }
}

/// Resolve a requested path against the workspace root, refusing anything landing outside.
/// Collapses `..`, resolves symlinks, and ensures destination is strictly within the root directory cone.
pub fn resolve_in_cone(root: &Path, requested: &str) -> Result<PathBuf, String> {
    let requested = requested.trim();
    if requested.is_empty() {
        return Err("no path specified".to_string());
    }

    let root = root
        .canonicalize()
        .map_err(|e| format!("cannot resolve workspace root directory: {e}"))?;

    let joined = {
        let p = Path::new(requested);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            root.join(p)
        }
    };

    // 1. Ancestor resolution check
    let resolved_ancestor = resolve_ancestors(&joined);
    if !resolved_ancestor.starts_with(&root) {
        return Err(outside_cone_error(requested));
    }

    // 2. Full filesystem canonicalization if path already exists
    if let Ok(real) = joined.canonicalize() {
        if !real.starts_with(&root) {
            return Err(outside_cone_error(requested));
        }
        Ok(real)
    } else {
        // If file does not exist yet (e.g. for write operations), return normalised path
        Ok(resolved_ancestor)
    }
}
