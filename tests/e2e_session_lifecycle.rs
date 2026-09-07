//! End-to-end tests for CLI session management: `kobold --detach`, `kobold list`, and `kobold kill`.

use std::path::PathBuf;
use std::process::Command;
use tempfile::tempdir;

fn kobold_binary() -> PathBuf {
    let mut path = std::env::current_exe().expect("current test exe");
    path.pop(); // deps
    path.pop(); // debug
    path.push("kobold");
    if path.exists() {
        return path;
    }
    PathBuf::from("target/debug/kobold")
}

#[test]
fn test_e2e_cli_session_detach_list_kill() {
    let bin = kobold_binary();
    assert!(bin.exists(), "kobold binary must exist at {:?}", bin);

    let dir = tempdir().unwrap();
    let session_id = format!("e2e-{}", uuid::Uuid::now_v7());

    // 1. Spawn a detached session in background via `kobold --detach`
    let output = Command::new(&bin)
        .arg("--mock")
        .arg("-w")
        .arg(dir.path())
        .arg("--session")
        .arg(&session_id)
        .arg("--detach")
        .output()
        .expect("run kobold --detach");

    assert!(
        output.status.success(),
        "kobold --detach failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&session_id),
        "stdout must mention session id: {stdout}"
    );

    // 2. Run `kobold list` and check that our session is listed
    let list_output = Command::new(&bin)
        .arg("list")
        .output()
        .expect("run kobold list");

    assert!(list_output.status.success());
    let list_stdout = String::from_utf8_lossy(&list_output.stdout);
    assert!(
        list_stdout.contains(&session_id),
        "kobold list must include {session_id}: {list_stdout}"
    );

    // 3. Terminate session via `kobold kill <session_id>`
    let kill_output = Command::new(&bin)
        .arg("kill")
        .arg(&session_id)
        .output()
        .expect("run kobold kill");

    assert!(
        kill_output.status.success(),
        "kobold kill failed: {}",
        String::from_utf8_lossy(&kill_output.stderr)
    );
    let kill_stdout = String::from_utf8_lossy(&kill_output.stdout);
    assert!(
        kill_stdout.contains("terminated session"),
        "kill stdout: {kill_stdout}"
    );

    // 4. Verify session is no longer in `kobold list`
    let list_after = Command::new(&bin)
        .arg("list")
        .output()
        .expect("run kobold list");
    let list_after_stdout = String::from_utf8_lossy(&list_after.stdout);
    assert!(
        !list_after_stdout.contains(&session_id),
        "session must be removed from list"
    );
}
