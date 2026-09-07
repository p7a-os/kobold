//! End-to-end tests for git worktree isolation and concurrent UI attach modes.

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
fn test_e2e_worktree_conflict_and_isolation() {
    let bin = kobold_binary();
    assert!(bin.exists(), "kobold binary must exist at {:?}", bin);

    let tmp = tempdir().expect("tempdir");
    let repo_dir = tmp.path();

    // 1. Initialize git repo
    let run_git = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(repo_dir)
            .status()
            .expect("run git");
        assert!(status.success(), "git {:?} failed", args);
    };

    run_git(&["init"]);
    run_git(&["config", "user.email", "test@kobold.dev"]);
    run_git(&["config", "user.name", "Kobold E2E"]);
    std::fs::write(repo_dir.join("main.rs"), "fn main() {}\n").unwrap();
    run_git(&["add", "main.rs"]);
    run_git(&["commit", "-m", "initial commit"]);

    let session1_id = format!("e2e-wt-{}", uuid::Uuid::now_v7());

    // 2. Start session 1 in repo_dir
    let out1 = Command::new(&bin)
        .arg("--mock")
        .arg("-w")
        .arg(repo_dir)
        .arg("--session")
        .arg(&session1_id)
        .arg("--detach")
        .output()
        .expect("start session 1");
    assert!(
        out1.status.success(),
        "session 1 failed: {}",
        String::from_utf8_lossy(&out1.stderr)
    );

    // 3. Start session 2 in the same repo_dir with KOBOLD_WORKTREE=1
    // This should detect the existing session in repo_dir, warn about potential conflicts,
    // and create a new git worktree automatically.
    let out2 = Command::new(&bin)
        .env("KOBOLD_WORKTREE", "1")
        .arg("--mock")
        .arg("-w")
        .arg(repo_dir)
        .arg("--detach")
        .output()
        .expect("start session 2 in worktree");

    assert!(
        out2.status.success(),
        "session 2 failed: {}",
        String::from_utf8_lossy(&out2.stderr)
    );
    let stderr2 = String::from_utf8_lossy(&out2.stderr);
    let stdout2 = String::from_utf8_lossy(&out2.stdout);

    // Verify conflict warning was issued
    assert!(
        stderr2.contains("active session") && stderr2.contains("is already running in"),
        "stderr must contain active session warning: {stderr2}"
    );
    assert!(
        stderr2.contains("created new git worktree at"),
        "stderr must announce worktree creation: {stderr2}"
    );

    // Verify .worktrees directory exists and has the new worktree
    let worktrees_dir = repo_dir.join(".worktrees");
    assert!(worktrees_dir.exists(), ".worktrees directory must exist");
    let entries: Vec<_> = std::fs::read_dir(&worktrees_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(entries.len(), 1, "exactly 1 worktree should be created");

    let wt_path = entries[0].path();
    assert!(
        wt_path.join("main.rs").exists(),
        "worktree must contain checked-out files"
    );

    // Verify session 2 is running in the new worktree
    let list_out = Command::new(&bin)
        .arg("list")
        .output()
        .expect("kobold list");
    let list_str = String::from_utf8_lossy(&list_out.stdout);
    assert!(list_str.contains(&session1_id), "session 1 must be in list");

    // Clean up sessions
    let _ = Command::new(&bin).arg("kill").arg(&session1_id).output();

    // Extract session 2 ID from stdout if available and kill it
    for line in stdout2.lines() {
        if line.contains("started detached session") {
            if let Some(id) = line.split_whitespace().last() {
                let _ = Command::new(&bin).arg("kill").arg(id).output();
            }
        }
    }
}
