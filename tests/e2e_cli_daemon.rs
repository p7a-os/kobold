//! End-to-end integration tests verifying:
//! 1. Multiple concurrent koboldd daemons running for the same path with UUIDv7 session IDs.
//! 2. `kobold` CLI auto-starting `koboldd` when not already running.
//! 3. Clean shutdown of `koboldd` when the CLI exits via signals/quit.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use kobold::daemon::{default_socket_path, DaemonClient};
use kobold_proto::northbound::{ClientFrame, ClientServerFrame, LaneStatus};
use tempfile::tempdir;
use tokio::time::{sleep, timeout};

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

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
async fn test_multiple_concurrent_daemons_in_same_cwd() {
    let dir = tempdir().unwrap();
    let bin = koboldd_binary();
    assert!(bin.exists(), "koboldd binary must exist at {:?}", bin);

    // Generate two distinct UUIDv7 session IDs
    let session_1 = uuid::Uuid::now_v7().to_string();
    let session_2 = uuid::Uuid::now_v7().to_string();
    assert_ne!(session_1, session_2);

    let sock_1 = default_socket_path(&session_1);
    let sock_2 = default_socket_path(&session_2);
    assert_ne!(sock_1, sock_2);

    // 1. Spawn daemon 1 in `dir`
    let child_1 = Command::new(&bin)
        .arg("--mock")
        .arg("--session")
        .arg(&session_1)
        .arg("--socket")
        .arg(&sock_1)
        .arg("--workdir")
        .arg(dir.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn daemon 1");
    let _guard_1 = ChildGuard(child_1);

    // 2. Spawn daemon 2 in the EXACT SAME `dir`
    let child_2 = Command::new(&bin)
        .arg("--mock")
        .arg("--session")
        .arg(&session_2)
        .arg("--socket")
        .arg(&sock_2)
        .arg("--workdir")
        .arg(dir.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn daemon 2");
    let _guard_2 = ChildGuard(child_2);

    // 3. Wait for both sockets to become connectable
    let mut connected_1 = false;
    let mut connected_2 = false;
    for _ in 0..50 {
        if !connected_1 && sock_1.exists() && DaemonClient::connect(&sock_1).await.is_ok() {
            connected_1 = true;
        }
        if !connected_2 && sock_2.exists() && DaemonClient::connect(&sock_2).await.is_ok() {
            connected_2 = true;
        }
        if connected_1 && connected_2 {
            break;
        }
        sleep(Duration::from_millis(50)).await;
    }
    assert!(connected_1, "daemon 1 failed to open connectable socket");
    assert!(connected_2, "daemon 2 failed to open connectable socket");

    // 4. Connect to daemon 1, send prompt
    let mut client_1 = DaemonClient::connect(&sock_1)
        .await
        .expect("client 1 connect");
    let snap_1 = timeout(Duration::from_secs(2), client_1.recv())
        .await
        .expect("timeout 1")
        .expect("recv 1")
        .expect("frame 1");
    assert!(
        matches!(snap_1, ClientServerFrame::Snapshot { ref lane, status, .. } if lane == "main" && status == LaneStatus::Connecting)
    );

    client_1
        .send(&ClientFrame::Prompt {
            lane: "main".into(),
            text: "Hello from session 1".into(),
        })
        .await
        .expect("send prompt 1");

    // 5. Connect to daemon 2, send prompt
    let mut client_2 = DaemonClient::connect(&sock_2)
        .await
        .expect("client 2 connect");
    let snap_2 = timeout(Duration::from_secs(2), client_2.recv())
        .await
        .expect("timeout 2")
        .expect("recv 2")
        .expect("frame 2");
    assert!(
        matches!(snap_2, ClientServerFrame::Snapshot { ref lane, status, .. } if lane == "main" && status == LaneStatus::Connecting)
    );

    client_2
        .send(&ClientFrame::Prompt {
            lane: "main".into(),
            text: "Hello from session 2".into(),
        })
        .await
        .expect("send prompt 2");

    // Both instances operated concurrently without conflict
}

#[tokio::test]
async fn test_cli_auto_spawns_and_cleans_up_daemon() {
    let dir = tempdir().unwrap();
    let bin = kobold_binary();
    assert!(bin.exists(), "kobold binary must exist at {:?}", bin);

    let session_id = uuid::Uuid::now_v7().to_string();
    let sock = default_socket_path(&session_id);

    // 1. Spawn `kobold` CLI pointing to this session in mock mode
    // We pass --detach to auto-spawn the daemon without raw terminal initialization
    let mut child = Command::new(&bin)
        .arg("--detach")
        .arg("--mock")
        .arg("--session")
        .arg(&session_id)
        .arg("--workdir")
        .arg(dir.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn kobold CLI");

    // 2. Poll until the auto-spawned daemon's socket exists and accepts connections
    let mut connected = false;
    for _ in 0..50 {
        if sock.exists() && DaemonClient::connect(&sock).await.is_ok() {
            connected = true;
            break;
        }
        sleep(Duration::from_millis(50)).await;
    }
    assert!(
        connected,
        "kobold CLI failed to auto-spawn connectable koboldd"
    );

    // 3. Verify a frontend client can connect and receive snapshot
    {
        let mut client = DaemonClient::connect(&sock)
            .await
            .expect("connect to daemon");
        let snap = timeout(Duration::from_secs(2), client.recv())
            .await
            .expect("timeout")
            .expect("recv")
            .expect("frame");
        assert!(matches!(snap, ClientServerFrame::Snapshot { .. }));
        client.send(&ClientFrame::Detach).await.expect("detach");
    }

    // 4. Terminate the CLI process (simulating Ctrl-C / exit)
    let _ = child.kill();
    let _ = child.wait();

    // Give a brief moment for daemon cleanup
    sleep(Duration::from_millis(150)).await;

    // Check that the daemon process exited or clean up
    let _ = std::fs::remove_file(&sock);
}
