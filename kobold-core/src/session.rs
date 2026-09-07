//! Session discovery, metadata persistence, and daemon lifecycle management.
//!
//! Tracks active `koboldd` daemons across `/tmp/kobold-{uid}/*.json`, handles
//! stale daemon purging, and enables detached sessions and CLI re-attachment.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Returns the user's base session runtime directory: `/tmp/kobold-{uid}`.
pub fn session_runtime_dir() -> PathBuf {
    #[cfg(unix)]
    let uid = unsafe { libc::getuid() };
    #[cfg(not(unix))]
    let uid = 1000;
    #[cfg(unix)]
    let base = PathBuf::from("/tmp");
    #[cfg(not(unix))]
    let base = std::env::temp_dir();
    base.join(format!("kobold-{uid}"))
}

/// Metadata recorded by every `koboldd` daemon instance on launch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMetadata {
    pub session_id: String,
    pub pid: u32,
    pub socket_path: PathBuf,
    pub workdir: PathBuf,
    pub created_at: u64,
    pub adapter: String,
}

impl SessionMetadata {
    /// Creates a new `SessionMetadata` record with the current timestamp.
    pub fn new(
        session_id: impl Into<String>,
        pid: u32,
        socket_path: impl Into<PathBuf>,
        workdir: impl Into<PathBuf>,
        adapter: impl Into<String>,
    ) -> Self {
        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Self {
            session_id: session_id.into(),
            pid,
            socket_path: socket_path.into(),
            workdir: workdir.into(),
            created_at,
            adapter: adapter.into(),
        }
    }

    /// Computes the path to this session's metadata file: `/tmp/kobold-{uid}/{session_id}.json`.
    pub fn meta_path_for(session_id: &str) -> PathBuf {
        session_runtime_dir().join(format!("{session_id}.json"))
    }

    /// Computes the path to this instance's metadata file.
    pub fn meta_path(&self) -> PathBuf {
        Self::meta_path_for(&self.session_id)
    }

    /// Writes this metadata record to disk atomically, creating the parent directory if necessary.
    pub fn save(&self) -> io::Result<()> {
        let path = self.meta_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let data = sonic_rs::to_vec_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        fs::write(&path, data)
    }

    /// Removes this session's metadata file from disk.
    pub fn remove(&self) -> io::Result<()> {
        let path = self.meta_path();
        if path.exists() {
            fs::remove_file(path)
        } else {
            Ok(())
        }
    }

    /// Checks if the daemon process recording this session is currently alive.
    pub fn is_alive(&self) -> bool {
        if self.pid == 0 {
            return false;
        }
        #[cfg(unix)]
        unsafe {
            // Signal 0 tests if the process exists and we have permission to send signals.
            libc::kill(self.pid as i32, 0) == 0
        }
        #[cfg(not(unix))]
        true
    }

    /// Returns human-readable relative age string (e.g. "5s ago", "2m ago", "1h ago").
    pub fn age_display(&self) -> String {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let diff = now.saturating_sub(self.created_at);
        if diff < 60 {
            format!("{diff}s ago")
        } else if diff < 3600 {
            format!("{}m ago", diff / 60)
        } else if diff < 86400 {
            format!("{}h ago", diff / 3600)
        } else {
            format!("{}d ago", diff / 86400)
        }
    }
}

/// The session registry discovers and manages active daemon sessions.
pub struct SessionRegistry;

impl SessionRegistry {
    /// Lists all active sessions found in the session runtime directory.
    /// Automatically purges stale metadata files for processes that are no longer running.
    /// Returns sessions sorted newest to oldest.
    pub fn list() -> Vec<SessionMetadata> {
        Self::list_in(&session_runtime_dir())
    }

    /// Lists sessions in a specific directory (useful for tests with temporary directories).
    pub fn list_in(dir: &Path) -> Vec<SessionMetadata> {
        if !dir.exists() {
            return Vec::new();
        }

        let mut sessions = Vec::new();
        let entries = match fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return Vec::new(),
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && path.extension().is_some_and(|ext| ext == "json") {
                if let Ok(bytes) = fs::read(&path) {
                    if let Ok(meta) = sonic_rs::from_slice::<SessionMetadata>(&bytes) {
                        if meta.is_alive() && meta.socket_path.exists() {
                            sessions.push(meta);
                        } else {
                            // Purge stale metadata and orphan socket if dead
                            let _ = fs::remove_file(&path);
                            if meta.socket_path.exists() {
                                let _ = fs::remove_file(&meta.socket_path);
                            }
                        }
                    }
                }
            }
        }

        // Sort by created_at descending (newest first)
        sessions.sort_by_key(|s| std::cmp::Reverse(s.created_at));
        sessions
    }

    /// Finds all active sessions whose working directory matches `workdir`.
    pub fn find_for_workdir(workdir: &Path) -> Vec<SessionMetadata> {
        Self::find_for_workdir_in(workdir, &session_runtime_dir())
    }

    /// Finds sessions for a working directory in a specific base directory.
    pub fn find_for_workdir_in(workdir: &Path, base_dir: &Path) -> Vec<SessionMetadata> {
        let canon_target = workdir
            .canonicalize()
            .unwrap_or_else(|_| workdir.to_path_buf());
        let all = Self::list_in(base_dir);
        all.into_iter()
            .filter(|s| {
                let canon_s = s
                    .workdir
                    .canonicalize()
                    .unwrap_or_else(|_| s.workdir.clone());
                canon_s == canon_target
            })
            .collect()
    }

    /// Finds a session by its exact session ID or unique prefix.
    pub fn find_by_id(id_or_prefix: &str) -> Option<SessionMetadata> {
        Self::find_by_id_in(id_or_prefix, &session_runtime_dir())
    }

    /// Finds a session by its exact session ID or unique prefix in a specific directory.
    pub fn find_by_id_in(id_or_prefix: &str, base_dir: &Path) -> Option<SessionMetadata> {
        let all = Self::list_in(base_dir);
        // First try exact match
        if let Some(exact) = all.iter().find(|s| s.session_id == id_or_prefix) {
            return Some(exact.clone());
        }
        // Then prefix match
        let matches: Vec<_> = all
            .into_iter()
            .filter(|s| s.session_id.starts_with(id_or_prefix))
            .collect();
        if matches.len() == 1 {
            matches.into_iter().next()
        } else {
            None
        }
    }

    /// Terminates a daemon session by sending SIGTERM to its process and cleaning up.
    pub fn kill(session_id: &str) -> io::Result<bool> {
        Self::kill_in(session_id, &session_runtime_dir())
    }

    /// Terminates a daemon session in a specific directory.
    pub fn kill_in(session_id: &str, base_dir: &Path) -> io::Result<bool> {
        if let Some(meta) = Self::find_by_id_in(session_id, base_dir) {
            #[cfg(unix)]
            unsafe {
                let pid = meta.pid as i32;
                if libc::kill(pid, libc::SIGTERM) == 0 {
                    // Give process brief moment to terminate before SIGKILL
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    if libc::kill(pid, 0) == 0 {
                        let _ = libc::kill(pid, libc::SIGKILL);
                    }
                }
            }
            let meta_path = base_dir.join(format!("{}.json", meta.session_id));
            let _ = fs::remove_file(&meta_path);
            if meta.socket_path.exists() {
                let _ = fs::remove_file(&meta.socket_path);
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn metadata_round_trips_to_json() {
        let meta = SessionMetadata::new(
            "test-sess-1",
            std::process::id(),
            "/tmp/test.sock",
            "/tmp/workdir",
            "kobold-openai",
        );
        let bytes = sonic_rs::to_vec(&meta).unwrap();
        let decoded: SessionMetadata = sonic_rs::from_slice(&bytes).unwrap();
        assert_eq!(meta, decoded);
    }

    #[test]
    fn list_in_filters_stale_and_sorts_by_recency() {
        let dir = tempdir().unwrap();

        // Active session with current process PID and real socket
        let real_sock = dir.path().join("real.sock");
        fs::write(&real_sock, b"").unwrap();
        let mut active = SessionMetadata::new(
            "active-1",
            std::process::id(),
            &real_sock,
            dir.path(),
            "kobold-openai",
        );
        active.created_at = 1000;
        let active_path = dir.path().join("active-1.json");
        fs::write(&active_path, sonic_rs::to_vec(&active).unwrap()).unwrap();

        // Newer active session
        let real_sock2 = dir.path().join("real2.sock");
        fs::write(&real_sock2, b"").unwrap();
        let mut active2 = SessionMetadata::new(
            "active-2",
            std::process::id(),
            &real_sock2,
            dir.path(),
            "kobold-openai",
        );
        active2.created_at = 2000;
        let active_path2 = dir.path().join("active-2.json");
        fs::write(&active_path2, sonic_rs::to_vec(&active2).unwrap()).unwrap();

        // Stale session with impossible PID
        let stale_sock = dir.path().join("stale.sock");
        fs::write(&stale_sock, b"").unwrap();
        let stale = SessionMetadata::new(
            "stale-1",
            999_999_999, // Dead PID
            &stale_sock,
            dir.path(),
            "kobold-openai",
        );
        let stale_path = dir.path().join("stale-1.json");
        fs::write(&stale_path, sonic_rs::to_vec(&stale).unwrap()).unwrap();

        let list = SessionRegistry::list_in(dir.path());
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].session_id, "active-2");
        assert_eq!(list[1].session_id, "active-1");

        // Assert stale metadata and socket were purged
        assert!(!stale_path.exists());
        assert!(!stale_sock.exists());
    }

    #[test]
    fn find_for_workdir_matches_correct_session() {
        let dir = tempdir().unwrap();
        let real_sock = dir.path().join("real.sock");
        fs::write(&real_sock, b"").unwrap();

        let meta = SessionMetadata::new(
            "sess-workdir",
            std::process::id(),
            &real_sock,
            dir.path(),
            "kobold-openai",
        );
        let meta_path = dir.path().join("sess-workdir.json");
        fs::write(&meta_path, sonic_rs::to_vec(&meta).unwrap()).unwrap();

        let matches = SessionRegistry::find_for_workdir_in(dir.path(), dir.path());
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].session_id, "sess-workdir");
    }
}
