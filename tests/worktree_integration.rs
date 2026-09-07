//! Integration tests for Git worktree creation and isolation.

use std::process::Command;
use tempfile::tempdir;

use kobold_core::worktree::{
    create_git_worktree, find_git_root, generate_worktree_name, is_git_repo,
};

#[test]
fn git_worktree_creation_and_exclusion() {
    let tmp = tempdir().expect("tempdir");
    let repo = tmp.path();

    // 1. Initialize git repo
    let run_git = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(repo)
            .status()
            .expect("run git");
        assert!(status.success(), "git {:?} failed", args);
    };

    run_git(&["init"]);
    run_git(&["config", "user.email", "test@kobold.dev"]);
    run_git(&["config", "user.name", "Kobold Test"]);

    std::fs::write(repo.join("README.md"), "# Kobold Repo\n").expect("write readme");
    run_git(&["add", "README.md"]);
    run_git(&["commit", "-m", "initial commit"]);

    // 2. Validate git detection
    assert!(is_git_repo(repo));
    let detected_root = find_git_root(repo).expect("find git root");
    // Canonicalize paths for comparison on platforms with symlinks (like /var vs /private/var on macOS)
    let canon_repo = std::fs::canonicalize(repo).expect("canon repo");
    let canon_root = std::fs::canonicalize(detected_root).expect("canon root");
    assert_eq!(canon_repo, canon_root);

    // 3. Create a worktree using a generated name
    let wt_name = generate_worktree_name();
    let wt_path = create_git_worktree(repo, &wt_name).expect("create worktree");

    assert!(wt_path.exists(), "worktree path must exist");
    assert!(
        wt_path.join("README.md").exists(),
        "worktree must contain checked-out files"
    );

    // 4. Verify .git/info/exclude contains .worktrees
    let exclude_path = repo.join(".git").join("info").join("exclude");
    assert!(exclude_path.exists(), "exclude file must exist");
    let exclude_content = std::fs::read_to_string(&exclude_path).expect("read exclude");
    assert!(
        exclude_content.contains(".worktrees"),
        "exclude file must contain .worktrees"
    );

    // 5. Verify parent repo git status is clean (untracked files do not show .worktrees)
    let status_output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(repo)
        .output()
        .expect("git status");
    let status_str = String::from_utf8_lossy(&status_output.stdout);
    assert!(
        status_str.trim().is_empty(),
        "parent repository status must remain clean, found: {status_str}"
    );

    // 6. Verify worktree is listed in `git worktree list`
    let list_output = Command::new("git")
        .args(["worktree", "list"])
        .current_dir(repo)
        .output()
        .expect("git worktree list");
    let list_str = String::from_utf8_lossy(&list_output.stdout);
    assert!(
        list_str.contains(&wt_name),
        "git worktree list must include {wt_name}: {list_str}"
    );
}
