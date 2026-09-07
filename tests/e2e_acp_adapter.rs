//! End-to-end tests verifying koboldd running with the kobold-adapter-acp Southbound adapter.

use kobold::daemon::DaemonClient;
use kobold_proto::agui;
use kobold_proto::northbound::{ClientFrame, ClientServerFrame};
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;
use tempfile::tempdir;
use tokio::time::{sleep, timeout};

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

#[tokio::test]
async fn test_e2e_koboldd_with_acp_adapter() {
    let koboldd_bin = binary_path("koboldd");
    let acp_adapter_bin = binary_path("kobold-adapter-acp");

    assert!(
        koboldd_bin.exists(),
        "koboldd must exist at {:?}",
        koboldd_bin
    );
    assert!(
        acp_adapter_bin.exists(),
        "kobold-adapter-acp must exist at {:?}",
        acp_adapter_bin
    );

    let dir = tempdir().unwrap();
    let sock = dir.path().join("acp_e2e.sock");
    let session_id = format!("acp-e2e-{}", uuid::Uuid::now_v7());

    // 1. Spawn koboldd with --adapter set to kobold-adapter-acp (using --mock internally)
    let mut daemon_proc = Command::new(&koboldd_bin)
        .arg("--socket")
        .arg(&sock)
        .arg("--session")
        .arg(&session_id)
        .arg("--workdir")
        .arg(dir.path())
        .arg("--adapter")
        .arg(format!("{} --mock", acp_adapter_bin.display()))
        .spawn()
        .expect("spawn koboldd with acp adapter");

    // 2. Wait for socket to become connectable
    let mut client = None;
    for _ in 0..50 {
        if sock.exists() {
            if let Ok(c) = DaemonClient::connect(&sock).await {
                client = Some(c);
                break;
            }
        }
        sleep(Duration::from_millis(50)).await;
    }
    let mut client = client.expect("failed to connect to koboldd socket");

    // 3. Receive initial hydration snapshot
    let snap = timeout(Duration::from_secs(3), client.recv())
        .await
        .expect("timeout waiting for snapshot")
        .expect("recv snapshot")
        .expect("snapshot frame");

    assert!(matches!(snap, ClientServerFrame::Snapshot { .. }));

    // 4. Send a prompt to the ACP adapter
    client
        .send(&ClientFrame::Prompt {
            lane: "main".into(),
            text: "Hello from ACP E2E test".into(),
        })
        .await
        .expect("send prompt");

    // 5. Receive streamed deltas and completion from the ACP adapter
    let mut received_delta = false;
    let mut run_completed = false;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        if let Ok(Ok(Some(frame))) = timeout(Duration::from_millis(500), client.recv()).await {
            match frame {
                ClientServerFrame::Event {
                    event: agui::Incoming::TextMessageContent { ref delta, .. },
                    ..
                } => {
                    if delta.contains("mock reply to: Hello from ACP E2E test") {
                        received_delta = true;
                    }
                }
                ClientServerFrame::Event {
                    event: agui::Incoming::RunFinished { .. },
                    ..
                } => {
                    run_completed = true;
                    break;
                }
                _ => {}
            }
        }
    }

    assert!(received_delta, "must have received ACP mock text delta");
    assert!(
        run_completed,
        "must have received RunFinished from ACP adapter"
    );

    // 6. Terminate daemon
    let _ = daemon_proc.kill();
    let _ = daemon_proc.wait();
}
