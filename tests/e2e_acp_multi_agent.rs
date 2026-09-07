//! End-to-end multi-agent tests verifying Kobold's ACP adapter against:
//! 1. Claude Code (`claude-code`)
//! 2. Google Antigravity (`antigravity`)
//! 3. xAI Grok Build (`grok-build`)
//! 4. OpenAI Codex (`codex`)
//! 5. Meta Muse (`muse`)
//! 6. OpenCode (`opencode`)

use kobold::daemon::DaemonClient;
use kobold_proto::agui;
use kobold_proto::northbound::{ClientFrame, ClientServerFrame};
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;
use tempfile::tempdir;
use tokio::time::{sleep, timeout};

struct ProcessGuard(std::process::Child);

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn binary_path(name: &str) -> PathBuf {
    let mut path = std::env::current_exe().expect("current test exe");
    path.pop(); // deps
    path.pop(); // debug
    let candidate = path.join(name);
    if candidate.exists() {
        return candidate;
    }
    let debug_path = PathBuf::from("target/debug").join(name);
    if debug_path.exists() {
        return debug_path;
    }
    let _ = Command::new("cargo")
        .args(["build", "--bin", name])
        .output();
    if candidate.exists() {
        candidate
    } else {
        debug_path
    }
}

async fn connect_to_daemon(sock: &std::path::Path) -> DaemonClient {
    for _ in 0..60 {
        if sock.exists() {
            if let Ok(c) = DaemonClient::connect(sock).await {
                return c;
            }
        }
        sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out connecting to daemon socket at {:?}", sock);
}

#[tokio::test]
async fn test_e2e_acp_claude_code() {
    let koboldd_bin = binary_path("koboldd");
    let acp_adapter_bin = binary_path("kobold-adapter-acp");
    let mock_agent_bin = binary_path("mock-acp-agent");

    let dir = tempdir().unwrap();
    let sock = dir.path().join("claude_code.sock");
    let session_id = format!("claude-code-{}", uuid::Uuid::now_v7());

    let daemon_proc = Command::new(&koboldd_bin)
        .arg("--socket")
        .arg(&sock)
        .arg("--session")
        .arg(&session_id)
        .arg("--workdir")
        .arg(dir.path())
        .arg("--adapter")
        .arg(&acp_adapter_bin)
        .arg("--unconfined")
        .env("ACP_AGENT_CMD", &mock_agent_bin)
        .env("ACP_AGENT_ARGS", "--agent claude-code")
        .spawn()
        .expect("spawn koboldd with claude-code agent");
    let _guard = ProcessGuard(daemon_proc);

    let mut client = connect_to_daemon(&sock).await;

    // 1. Initial snapshot
    let snap = timeout(Duration::from_secs(5), client.recv())
        .await
        .expect("timeout waiting for snapshot")
        .expect("recv snapshot")
        .expect("snapshot frame");
    assert!(matches!(snap, ClientServerFrame::Snapshot { .. }));

    // 2. Submit user prompt
    client
        .send(&ClientFrame::Prompt {
            lane: "main".into(),
            text: "Analyze auth pipeline and run tests".into(),
        })
        .await
        .expect("send prompt");

    let mut saw_text_check = false;
    let mut saw_ask = false;
    let mut saw_approval_reply = false;
    let mut saw_run_finished = false;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline && !saw_run_finished {
        if let Ok(Ok(Some(frame))) = timeout(Duration::from_millis(500), client.recv()).await {
            match frame {
                ClientServerFrame::Event {
                    event: agui::Incoming::TextMessageContent { ref delta, .. },
                    ..
                } => {
                    if delta.contains("Running cargo check for verification.") {
                        saw_text_check = true;
                    }
                    if delta.contains("Check passed. Claude Code finished task.") {
                        saw_approval_reply = true;
                    }
                }
                ClientServerFrame::Snapshot {
                    active_interrupt: Some(ask),
                    ..
                } => {
                    if ask.question.contains("cargo check") {
                        saw_ask = true;
                        client
                            .send(&ClientFrame::SubmitInterrupt {
                                lane: "main".into(),
                                call_id: ask.call_id,
                                answers: vec!["allow".into()],
                            })
                            .await
                            .expect("submit interrupt");
                    }
                }
                ClientServerFrame::Event {
                    event: agui::Incoming::RunFinished { .. },
                    ..
                } => {
                    saw_run_finished = true;
                }
                _ => {}
            }
        }
    }

    assert!(saw_text_check, "must have seen Claude Code check message");
    assert!(saw_ask, "must have seen Claude Code permission ask");
    assert!(
        saw_approval_reply,
        "must have seen Claude Code post-approval completion"
    );
    assert!(saw_run_finished, "turn must complete with RunFinished");
}

#[tokio::test]
async fn test_e2e_acp_antigravity() {
    let koboldd_bin = binary_path("koboldd");
    let acp_adapter_bin = binary_path("kobold-adapter-acp");
    let mock_agent_bin = binary_path("mock-acp-agent");

    let dir = tempdir().unwrap();
    // Create dummy Cargo.toml so file_read tool succeeds
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"dummy\"\n",
    )
    .unwrap();

    let sock = dir.path().join("antigravity.sock");
    let session_id = format!("ag-{}", uuid::Uuid::now_v7());

    let daemon_proc = Command::new(&koboldd_bin)
        .arg("--socket")
        .arg(&sock)
        .arg("--session")
        .arg(&session_id)
        .arg("--workdir")
        .arg(dir.path())
        .arg("--adapter")
        .arg(&acp_adapter_bin)
        .arg("--unconfined")
        .env("ACP_AGENT_CMD", &mock_agent_bin)
        .env("ACP_AGENT_ARGS", "--agent antigravity")
        .spawn()
        .expect("spawn koboldd with antigravity agent");
    let _guard = ProcessGuard(daemon_proc);

    let mut client = connect_to_daemon(&sock).await;

    let _snap = timeout(Duration::from_secs(5), client.recv())
        .await
        .expect("timeout waiting for snapshot");

    client
        .send(&ClientFrame::Prompt {
            lane: "main".into(),
            text: "Plan and create greeting module".into(),
        })
        .await
        .expect("send prompt");

    let mut saw_ask = false;
    let mut saw_task_complete = false;
    let mut saw_run_finished = false;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline && !saw_run_finished {
        if let Ok(Ok(Some(frame))) = timeout(Duration::from_millis(500), client.recv()).await {
            match frame {
                ClientServerFrame::Event {
                    event: agui::Incoming::TextMessageContent { ref delta, .. },
                    ..
                } => {
                    if delta.contains("greeting.rs written") {
                        saw_task_complete = true;
                    }
                }
                ClientServerFrame::Snapshot {
                    active_interrupt: Some(ask),
                    ..
                } => {
                    if ask.question.contains("Update src/greeting.rs") {
                        saw_ask = true;
                        client
                            .send(&ClientFrame::SubmitInterrupt {
                                lane: "main".into(),
                                call_id: ask.call_id,
                                answers: vec!["allow".into()],
                            })
                            .await
                            .expect("submit interrupt");
                    }
                }
                ClientServerFrame::Event {
                    event: agui::Incoming::RunFinished { .. },
                    ..
                } => {
                    saw_run_finished = true;
                }
                _ => {}
            }
        }
    }

    assert!(saw_ask, "must have seen Antigravity permission ask");
    assert!(
        saw_task_complete,
        "must have seen Antigravity task completion"
    );
    assert!(saw_run_finished, "turn must complete with RunFinished");
}

#[tokio::test]
async fn test_e2e_acp_grok_build() {
    let koboldd_bin = binary_path("koboldd");
    let acp_adapter_bin = binary_path("kobold-adapter-acp");
    let mock_agent_bin = binary_path("mock-acp-agent");

    let dir = tempdir().unwrap();
    let sock = dir.path().join("grok_build.sock");
    let session_id = format!("grok-{}", uuid::Uuid::now_v7());

    let daemon_proc = Command::new(&koboldd_bin)
        .arg("--socket")
        .arg(&sock)
        .arg("--session")
        .arg(&session_id)
        .arg("--workdir")
        .arg(dir.path())
        .arg("--adapter")
        .arg(&acp_adapter_bin)
        .arg("--unconfined")
        .env("ACP_AGENT_CMD", &mock_agent_bin)
        .env("ACP_AGENT_ARGS", "--agent grok-build")
        .spawn()
        .expect("spawn koboldd with grok-build agent");
    let _guard = ProcessGuard(daemon_proc);

    let mut client = connect_to_daemon(&sock).await;

    let _snap = timeout(Duration::from_secs(5), client.recv())
        .await
        .expect("timeout waiting for snapshot");

    client
        .send(&ClientFrame::Prompt {
            lane: "main".into(),
            text: "Build workspace release".into(),
        })
        .await
        .expect("send prompt");

    let mut saw_ask = false;
    let mut saw_build_finish = false;
    let mut saw_run_finished = false;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline && !saw_run_finished {
        if let Ok(Ok(Some(frame))) = timeout(Duration::from_millis(500), client.recv()).await {
            match frame {
                ClientServerFrame::Event {
                    event: agui::Incoming::TextMessageContent { ref delta, .. },
                    ..
                } => {
                    if delta.contains("Grok Build finished in 1.4s") {
                        saw_build_finish = true;
                    }
                }
                ClientServerFrame::Snapshot {
                    active_interrupt: Some(ask),
                    ..
                } => {
                    if ask.question.contains("cargo build") {
                        saw_ask = true;
                        client
                            .send(&ClientFrame::SubmitInterrupt {
                                lane: "main".into(),
                                call_id: ask.call_id,
                                answers: vec!["allow".into()],
                            })
                            .await
                            .expect("submit interrupt");
                    }
                }
                ClientServerFrame::Event {
                    event: agui::Incoming::RunFinished { .. },
                    ..
                } => {
                    saw_run_finished = true;
                }
                _ => {}
            }
        }
    }

    assert!(saw_ask, "must have seen Grok Build permission ask");
    assert!(
        saw_build_finish,
        "must have seen Grok Build completion text"
    );
    assert!(saw_run_finished, "turn must complete with RunFinished");
}

#[tokio::test]
async fn test_e2e_acp_codex() {
    let koboldd_bin = binary_path("koboldd");
    let acp_adapter_bin = binary_path("kobold-adapter-acp");
    let mock_agent_bin = binary_path("mock-acp-agent");

    let dir = tempdir().unwrap();
    let sock = dir.path().join("codex.sock");
    let session_id = format!("codex-{}", uuid::Uuid::now_v7());

    let daemon_proc = Command::new(&koboldd_bin)
        .arg("--socket")
        .arg(&sock)
        .arg("--session")
        .arg(&session_id)
        .arg("--workdir")
        .arg(dir.path())
        .arg("--adapter")
        .arg(&acp_adapter_bin)
        .arg("--unconfined")
        .env("ACP_AGENT_CMD", &mock_agent_bin)
        .env("ACP_AGENT_ARGS", "--agent codex")
        .spawn()
        .expect("spawn koboldd with codex agent");
    let _guard = ProcessGuard(daemon_proc);

    let mut client = connect_to_daemon(&sock).await;

    let _snap = timeout(Duration::from_secs(5), client.recv())
        .await
        .expect("timeout waiting for snapshot");

    client
        .send(&ClientFrame::Prompt {
            lane: "main".into(),
            text: "Generate ACP verification function".into(),
        })
        .await
        .expect("send prompt");

    let mut saw_code_block = false;
    let mut saw_ask = false;
    let mut saw_patch_applied = false;
    let mut saw_run_finished = false;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline && !saw_run_finished {
        if let Ok(Ok(Some(frame))) = timeout(Duration::from_millis(500), client.recv()).await {
            match frame {
                ClientServerFrame::Event {
                    event: agui::Incoming::TextMessageContent { ref delta, .. },
                    ..
                } => {
                    if delta.contains("pub fn acp_ready()") {
                        saw_code_block = true;
                    }
                    if delta.contains("Patch codex.patch applied cleanly.") {
                        saw_patch_applied = true;
                    }
                }
                ClientServerFrame::Snapshot {
                    active_interrupt: Some(ask),
                    ..
                } => {
                    if ask.question.contains("git apply") {
                        saw_ask = true;
                        client
                            .send(&ClientFrame::SubmitInterrupt {
                                lane: "main".into(),
                                call_id: ask.call_id,
                                answers: vec!["allow".into()],
                            })
                            .await
                            .expect("submit interrupt");
                    }
                }
                ClientServerFrame::Event {
                    event: agui::Incoming::RunFinished { .. },
                    ..
                } => {
                    saw_run_finished = true;
                }
                _ => {}
            }
        }
    }

    assert!(saw_code_block, "must have seen generated code block");
    assert!(saw_ask, "must have seen Codex patch permission ask");
    assert!(
        saw_patch_applied,
        "must have seen patch application message"
    );
    assert!(saw_run_finished, "turn must complete with RunFinished");
}

#[tokio::test]
async fn test_e2e_acp_muse_meta_permission_denial() {
    let koboldd_bin = binary_path("koboldd");
    let acp_adapter_bin = binary_path("kobold-adapter-acp");
    let mock_agent_bin = binary_path("mock-acp-agent");

    let dir = tempdir().unwrap();
    let sock = dir.path().join("muse.sock");
    let session_id = format!("muse-{}", uuid::Uuid::now_v7());

    let daemon_proc = Command::new(&koboldd_bin)
        .arg("--socket")
        .arg(&sock)
        .arg("--session")
        .arg(&session_id)
        .arg("--workdir")
        .arg(dir.path())
        .arg("--adapter")
        .arg(&acp_adapter_bin)
        .arg("--unconfined")
        .env("ACP_AGENT_CMD", &mock_agent_bin)
        .env("ACP_AGENT_ARGS", "--agent muse")
        .spawn()
        .expect("spawn koboldd with muse agent");
    let _guard = ProcessGuard(daemon_proc);

    let mut client = connect_to_daemon(&sock).await;

    let _snap = timeout(Duration::from_secs(5), client.recv())
        .await
        .expect("timeout waiting for snapshot");

    client
        .send(&ClientFrame::Prompt {
            lane: "main".into(),
            text: "Large multi-crate refactoring".into(),
        })
        .await
        .expect("send prompt");

    let mut saw_proposal = false;
    let mut saw_ask = false;
    let mut saw_denial_text = false;
    let mut saw_run_finished = false;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline && !saw_run_finished {
        if let Ok(Ok(Some(frame))) = timeout(Duration::from_millis(500), client.recv()).await {
            match frame {
                ClientServerFrame::Event {
                    event: agui::Incoming::TextMessageContent { ref delta, .. },
                    ..
                } => {
                    if delta.contains("Proposed refactoring touches 3 crates") {
                        saw_proposal = true;
                    }
                    if delta.contains("Changeset rejected. Preserving original workspace state.") {
                        saw_denial_text = true;
                    }
                }
                ClientServerFrame::Snapshot {
                    active_interrupt: Some(ask),
                    ..
                } => {
                    if ask
                        .question
                        .contains("Apply multi-crate refactoring changeset")
                    {
                        saw_ask = true;
                        // Deny permission via CancelInterrupt
                        client
                            .send(&ClientFrame::CancelInterrupt {
                                lane: "main".into(),
                                call_id: ask.call_id,
                            })
                            .await
                            .expect("cancel interrupt");
                    }
                }
                ClientServerFrame::Event {
                    event: agui::Incoming::RunFinished { .. },
                    ..
                } => {
                    saw_run_finished = true;
                }
                _ => {}
            }
        }
    }

    assert!(saw_proposal, "must have seen Meta Muse changeset proposal");
    assert!(saw_ask, "must have seen Meta Muse permission ask");
    assert!(
        saw_denial_text,
        "must have seen rejection acknowledgment message"
    );
    assert!(
        saw_run_finished,
        "turn must complete with RunFinished after denial"
    );
}

#[tokio::test]
async fn test_e2e_acp_opencode_terminal_tool() {
    let koboldd_bin = binary_path("koboldd");
    let acp_adapter_bin = binary_path("kobold-adapter-acp");
    let mock_agent_bin = binary_path("mock-acp-agent");

    let dir = tempdir().unwrap();
    let sock = dir.path().join("opencode.sock");
    let session_id = format!("opencode-{}", uuid::Uuid::now_v7());

    let daemon_proc = Command::new(&koboldd_bin)
        .arg("--socket")
        .arg(&sock)
        .arg("--session")
        .arg(&session_id)
        .arg("--workdir")
        .arg(dir.path())
        .arg("--adapter")
        .arg(&acp_adapter_bin)
        .arg("--unconfined")
        .env("ACP_AGENT_CMD", &mock_agent_bin)
        .env("ACP_AGENT_ARGS", "--agent opencode")
        .spawn()
        .expect("spawn koboldd with opencode agent");
    let _guard = ProcessGuard(daemon_proc);

    let mut client = connect_to_daemon(&sock).await;

    let _snap = timeout(Duration::from_secs(5), client.recv())
        .await
        .expect("timeout waiting for snapshot");

    client
        .send(&ClientFrame::Prompt {
            lane: "main".into(),
            text: "Run test suite".into(),
        })
        .await
        .expect("send prompt");

    let mut saw_ask = false;
    let mut saw_tool_success = false;
    let mut saw_run_finished = false;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline && !saw_run_finished {
        if let Ok(Ok(Some(frame))) = timeout(Duration::from_millis(500), client.recv()).await {
            match frame {
                ClientServerFrame::Event {
                    event: agui::Incoming::TextMessageContent { ref delta, .. },
                    ..
                } => {
                    if delta.contains("All 14 test suites passed successfully.") {
                        saw_tool_success = true;
                    }
                }
                ClientServerFrame::Snapshot {
                    active_interrupt: Some(ask),
                    ..
                } => {
                    if ask.question.contains("npm test") {
                        saw_ask = true;
                        client
                            .send(&ClientFrame::SubmitInterrupt {
                                lane: "main".into(),
                                call_id: ask.call_id,
                                answers: vec!["allow".into()],
                            })
                            .await
                            .expect("submit interrupt");
                    }
                }
                ClientServerFrame::Event {
                    event: agui::Incoming::RunFinished { .. },
                    ..
                } => {
                    saw_run_finished = true;
                }
                _ => {}
            }
        }
    }

    assert!(saw_ask, "must have seen OpenCode permission ask");
    assert!(
        saw_tool_success,
        "must have seen OpenCode test result message"
    );
    assert!(saw_run_finished, "turn must complete with RunFinished");
}

#[tokio::test]
async fn test_e2e_acp_all_six_agents_matrix() {
    let koboldd_bin = binary_path("koboldd");
    let acp_adapter_bin = binary_path("kobold-adapter-acp");
    let mock_agent_bin = binary_path("mock-acp-agent");

    let agents = [
        ("claude-code", "Claude Code"),
        ("antigravity", "Antigravity"),
        ("grok-build", "Grok Build"),
        ("codex", "Codex"),
        ("muse", "Meta Muse"),
        ("opencode", "OpenCode"),
    ];

    for (agent_id, agent_name) in agents {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"dummy\"\n",
        )
        .unwrap();
        let sock = dir.path().join(format!("{agent_id}.sock"));
        let session_id = format!("matrix-{agent_id}-{}", uuid::Uuid::now_v7());

        let daemon_proc = Command::new(&koboldd_bin)
            .arg("--socket")
            .arg(&sock)
            .arg("--session")
            .arg(&session_id)
            .arg("--workdir")
            .arg(dir.path())
            .arg("--adapter")
            .arg(&acp_adapter_bin)
            .arg("--unconfined")
            .env("ACP_AGENT_CMD", &mock_agent_bin)
            .env("ACP_AGENT_ARGS", format!("--agent {agent_id}"))
            .spawn()
            .unwrap_or_else(|e| panic!("failed to spawn daemon for {agent_name}: {e}"));
        let _guard = ProcessGuard(daemon_proc);

        let mut client = connect_to_daemon(&sock).await;

        let snap = timeout(Duration::from_secs(5), client.recv())
            .await
            .expect("timeout waiting for snapshot")
            .expect("recv")
            .expect("frame");
        assert!(matches!(snap, ClientServerFrame::Snapshot { .. }));

        client
            .send(&ClientFrame::Prompt {
                lane: "main".into(),
                text: format!("Hello {agent_name}"),
            })
            .await
            .expect("send prompt");

        let mut completed = false;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while tokio::time::Instant::now() < deadline && !completed {
            if let Ok(Ok(Some(frame))) = timeout(Duration::from_millis(500), client.recv()).await {
                match frame {
                    ClientServerFrame::Snapshot {
                        active_interrupt: Some(ask),
                        ..
                    } => {
                        let _ = client
                            .send(&ClientFrame::SubmitInterrupt {
                                lane: "main".into(),
                                call_id: ask.call_id,
                                answers: vec!["allow".into()],
                            })
                            .await;
                    }
                    ClientServerFrame::Event {
                        event: agui::Incoming::RunFinished { .. },
                        ..
                    } => {
                        completed = true;
                    }
                    _ => {}
                }
            }
        }

        assert!(
            completed,
            "Agent {agent_name} ({agent_id}) must complete ACP turn successfully"
        );
    }
}
