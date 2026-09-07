//! End-to-end tests verifying ACP mid-flight cancellation and turn steering.

use kobold::daemon::DaemonClient;
use kobold_proto::agui;
use kobold_proto::northbound::{ClientFrame, ClientServerFrame, LaneStatus};
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
async fn test_e2e_acp_midturn_steering_and_cancellation() {
    let koboldd_bin = binary_path("koboldd");
    let acp_adapter_bin = binary_path("kobold-adapter-acp");
    let mock_agent_bin = binary_path("mock-acp-agent");

    let dir = tempdir().unwrap();
    let sock = dir.path().join("steering.sock");
    let session_id = format!("steering-sess-{}", uuid::Uuid::now_v7());

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
        .env("ACP_AGENT_ARGS", "--agent steering")
        .spawn()
        .expect("spawn koboldd daemon");
    let _guard = ProcessGuard(daemon_proc);

    let mut client = connect_to_daemon(&sock).await;

    // Ingest initial snapshot
    let snap = timeout(Duration::from_secs(5), client.recv())
        .await
        .expect("timeout waiting for snapshot")
        .expect("recv snapshot")
        .expect("snapshot frame");
    assert!(matches!(snap, ClientServerFrame::Snapshot { .. }));

    // 1. Dispatch long-running prompt
    client
        .send(&ClientFrame::Prompt {
            lane: "main".into(),
            text: "Perform large multi-file refactor".into(),
        })
        .await
        .expect("send prompt");

    // Expect initial deltas
    let mut saw_chunk = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline && !saw_chunk {
        if let Ok(Ok(Some(ClientServerFrame::Event {
            event: agui::Incoming::TextMessageContent { ref delta, .. },
            ..
        }))) = timeout(Duration::from_millis(500), client.recv()).await
        {
            if delta.contains("Processing chunk") {
                saw_chunk = true;
                break;
            }
        }
    }
    assert!(saw_chunk, "Initial text chunk must be received");

    // 2. Cancel in-flight turn mid-flight!
    client
        .send(&ClientFrame::CancelTurn {
            lane: "main".into(),
        })
        .await
        .expect("send CancelTurn");

    // Verify lane status transitions back to Ready or receives cancellation
    let mut status_reset_to_ready = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline && !status_reset_to_ready {
        if let Ok(Ok(Some(ClientServerFrame::StatusChange { status, .. }))) =
            timeout(Duration::from_millis(500), client.recv()).await
        {
            if status == LaneStatus::Ready {
                status_reset_to_ready = true;
                break;
            }
        }
    }
    assert!(
        status_reset_to_ready,
        "Lane status must reset to Ready upon cancellation"
    );

    // 3. Immediately dispatch steering prompt!
    client
        .send(&ClientFrame::Prompt {
            lane: "main".into(),
            text: "Pivot: run test suite instead".into(),
        })
        .await
        .expect("send steering prompt");

    // Verify that the second prompt runs and completes cleanly
    let mut saw_steered_chunk = false;
    let mut second_run_finished = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    while tokio::time::Instant::now() < deadline && !second_run_finished {
        if let Ok(Ok(Some(frame))) = timeout(Duration::from_millis(500), client.recv()).await {
            match frame {
                ClientServerFrame::Event {
                    event: agui::Incoming::TextMessageContent { ref delta, .. },
                    ..
                } => {
                    if delta.contains("Steered task completed successfully") {
                        saw_steered_chunk = true;
                    }
                }
                ClientServerFrame::Event {
                    event: agui::Incoming::RunFinished { .. },
                    ..
                } if saw_steered_chunk => {
                    second_run_finished = true;
                    break;
                }
                _ => {}
            }
        }
    }

    assert!(saw_steered_chunk, "Steered prompt delta must be received");
    assert!(
        second_run_finished,
        "Steered prompt must finish cleanly without collision"
    );
}
