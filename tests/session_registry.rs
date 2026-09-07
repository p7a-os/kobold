//! Integration tests for SessionRegistry filesystem operations, stale detection, and lifecycle.

use kobold_core::session::{SessionMetadata, SessionRegistry};
use std::fs;
use std::process::Command;
use tempfile::tempdir;

#[test]
fn test_session_registry_lifecycle_and_stale_cleanup() {
    let dir = tempdir().unwrap();
    let runtime_dir = dir.path().join("sessions");
    fs::create_dir_all(&runtime_dir).unwrap();

    // 1. Spawn a live dummy child process that sleeps
    let mut child = Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("spawn sleep child");

    let live_pid = child.id();
    let live_sock = runtime_dir.join("live.sock");
    fs::write(&live_sock, b"").unwrap();

    let live_meta = SessionMetadata::new(
        "live-session-1",
        live_pid,
        &live_sock,
        dir.path(),
        "kobold-openai",
    );
    let live_meta_path = runtime_dir.join("live-session-1.json");
    fs::write(&live_meta_path, sonic_rs::to_vec(&live_meta).unwrap()).unwrap();

    // 2. Create a dead session
    let dead_sock = runtime_dir.join("dead.sock");
    fs::write(&dead_sock, b"").unwrap();
    let dead_meta = SessionMetadata::new(
        "dead-session-1",
        999_999_999,
        &dead_sock,
        dir.path(),
        "kobold-openai",
    );
    let dead_meta_path = runtime_dir.join("dead-session-1.json");
    fs::write(&dead_meta_path, sonic_rs::to_vec(&dead_meta).unwrap()).unwrap();

    // 3. Query registry list
    let sessions = SessionRegistry::list_in(&runtime_dir);
    assert_eq!(sessions.len(), 1, "only the live session should remain");
    assert_eq!(sessions[0].session_id, "live-session-1");

    // 4. Assert dead metadata and orphan dead socket were cleaned up
    assert!(!dead_meta_path.exists(), "dead metadata must be purged");
    assert!(!dead_sock.exists(), "orphan dead socket must be purged");

    // 5. Kill the live session via SessionRegistry
    let killed = SessionRegistry::kill_in("live-session-1", &runtime_dir).expect("kill");
    assert!(killed, "session must be killed");
    assert!(!live_meta_path.exists(), "killed metadata must be purged");
    assert!(!live_sock.exists(), "killed socket must be purged");

    // Re-query list: must be empty
    assert!(SessionRegistry::list_in(&runtime_dir).is_empty());

    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn test_session_registry_find_by_id_and_prefix() {
    let dir = tempdir().unwrap();
    let runtime_dir = dir.path();

    let sock = runtime_dir.join("prefix.sock");
    fs::write(&sock, b"").unwrap();
    let meta = SessionMetadata::new(
        "01918a22-prefix-test",
        std::process::id(),
        &sock,
        dir.path(),
        "kobold-openai",
    );
    let meta_path = runtime_dir.join("01918a22-prefix-test.json");
    fs::write(&meta_path, sonic_rs::to_vec(&meta).unwrap()).unwrap();

    // Find by exact ID
    let found = SessionRegistry::find_by_id_in("01918a22-prefix-test", runtime_dir);
    assert!(found.is_some());
    assert_eq!(found.unwrap().session_id, "01918a22-prefix-test");

    // Find by prefix
    let found_prefix = SessionRegistry::find_by_id_in("01918a22", runtime_dir);
    assert!(found_prefix.is_some());
    assert_eq!(found_prefix.unwrap().session_id, "01918a22-prefix-test");

    // Non-matching prefix
    assert!(SessionRegistry::find_by_id_in("nonexistent", runtime_dir).is_none());
}
