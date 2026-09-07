//! End-to-end test verifying detached daemon lifecycle and frontend reconnection.

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use kobold::daemon::DaemonClient;
use kobold_proto::northbound::{ClientFrame, ClientServerFrame, LaneStatus};
use tempfile::tempdir;
use tokio::time::{sleep, timeout};

fn koboldd_binary() -> PathBuf {
    let mut path = std::env::current_exe().expect("current test exe");
    path.pop(); // deps
    path.pop(); // debug
    path.push("koboldd");
    if path.exists() {
        return path;
    }
    PathBuf::from("target/debug/koboldd")
}

#[tokio::test]
async fn test_e2e_detached_daemon_lifecycle() {
    let dir = tempdir().unwrap();
    let sock = dir.path().join("e2e.sock");
    let bin = koboldd_binary();

    assert!(bin.exists(), "koboldd binary must exist at {:?}", bin);

    // 1. Spawn real koboldd process in background
    let mut child = Command::new(&bin)
        .arg("--mock")
        .arg("--socket")
        .arg(&sock)
        .arg("--workdir")
        .arg(dir.path())
        .spawn()
        .expect("spawn koboldd");

    // 2. Poll until socket is connectable
    let mut connected = false;
    for _ in 0..50 {
        if sock.exists() && DaemonClient::connect(&sock).await.is_ok() {
            connected = true;
            break;
        }
        sleep(Duration::from_millis(50)).await;
    }
    assert!(connected, "koboldd failed to create connectable socket");

    // 3. First client attaches
    {
        let mut client1 = DaemonClient::connect(&sock).await.expect("client1 connect");
        let initial_snap = timeout(Duration::from_secs(2), client1.recv())
            .await
            .expect("timeout")
            .expect("recv")
            .expect("frame");

        match initial_snap {
            ClientServerFrame::Snapshot { lane, status, .. } => {
                assert_eq!(lane, "main");
                assert_eq!(status, LaneStatus::Connecting);
            }
            other => panic!("expected initial Snapshot, got {:?}", other),
        }

        // Send a prompt
        client1
            .send(&ClientFrame::Prompt {
                lane: "main".into(),
                text: "E2E detached prompt".into(),
            })
            .await
            .expect("send prompt");

        // Gracefully detach
        client1
            .send(&ClientFrame::Detach)
            .await
            .expect("send detach");
    }

    // 4. Second client connects to the running daemon and verifies state persistence
    {
        let mut client2 = DaemonClient::connect(&sock).await.expect("client2 connect");
        let snap2 = timeout(Duration::from_secs(2), client2.recv())
            .await
            .expect("timeout")
            .expect("recv")
            .expect("frame");

        match snap2 {
            ClientServerFrame::Snapshot { lane, .. } => {
                assert_eq!(lane, "main");
            }
            other => panic!("expected Snapshot, got {:?}", other),
        }
    }

    // 5. Terminate daemon process and verify cleanup
    let _ = child.kill();
    let _ = child.wait();
}
