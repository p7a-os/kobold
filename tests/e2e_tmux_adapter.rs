//! End-to-end tests for `koboldd` integrated with `kobold-adapter-tmux`.

use std::path::PathBuf;
use std::process::Command as StdCommand;
use std::time::Duration;
use tempfile::tempdir;

use kobold_core::daemon::{default_socket_path, DaemonClient};
use kobold_proto::northbound::{ClientFrame, ClientServerFrame};

fn find_binary(name: &str) -> PathBuf {
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
    let _ = StdCommand::new("cargo")
        .args(["build", "--bin", name])
        .output();
    if candidate.exists() {
        candidate
    } else {
        debug_path
    }
}

#[tokio::test]
async fn test_e2e_koboldd_with_tmux_adapter() {
    let koboldd_bin = find_binary("koboldd");
    let adapter_bin = find_binary("kobold-adapter-tmux");

    assert!(
        koboldd_bin.exists(),
        "koboldd must exist at {koboldd_bin:?}"
    );
    assert!(
        adapter_bin.exists(),
        "kobold-adapter-tmux must exist at {adapter_bin:?}"
    );

    let tmp = tempdir().expect("tempdir");
    let session_id = format!("e2e-tmux-{}", uuid::Uuid::now_v7());
    let socket = default_socket_path(&session_id);

    // Spawn koboldd with tmux adapter
    let mut child = StdCommand::new(&koboldd_bin)
        .arg("--socket")
        .arg(&socket)
        .arg("--session")
        .arg(&session_id)
        .arg("--workdir")
        .arg(tmp.path())
        .arg("--adapter")
        .arg(&adapter_bin)
        .spawn()
        .expect("spawn koboldd");

    // Wait for socket to become available
    let mut client = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        if socket.exists() {
            if let Ok(c) = DaemonClient::connect(&socket).await {
                client = Some(c);
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let client = client.expect("connect to koboldd with tmux adapter");
    let (client_tx, mut client_rx) = client.into_channels();

    // 1. Drain initial hydration snapshot
    let snapshot = client_rx.recv().await.expect("recv snapshot");
    assert!(matches!(snapshot, ClientServerFrame::Snapshot { .. }));

    // 2. Submit shell prompt to the PTY
    client_tx
        .send(ClientFrame::Prompt {
            lane: "main".into(),
            text: "echo 'e2e-tmux-harness-verified'".into(),
        })
        .expect("send prompt");

    // 3. Receive streamed text delta from PTY
    let mut accumulated = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        if let Ok(Some(ClientServerFrame::Event {
            event: kobold_proto::agui::Incoming::TextMessageContent { delta, .. },
            ..
        })) = tokio::time::timeout(Duration::from_millis(200), client_rx.recv()).await
        {
            accumulated.push_str(&delta);
            if accumulated.contains("e2e-tmux-harness-verified") {
                break;
            }
        }
    }

    assert!(
        accumulated.contains("e2e-tmux-harness-verified"),
        "expected stdout to contain token, got: {accumulated}"
    );

    // 4. Detach and kill daemon
    let _ = client_tx.send(ClientFrame::Detach);
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_file(&socket);
}
