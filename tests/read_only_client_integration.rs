//! Integration tests for read-write vs read-only concurrent UI attachments to koboldd.

use std::time::Duration;
use tempfile::tempdir;
use tokio::sync::{mpsc, watch};

use kobold_core::daemon::{Daemon, DaemonClient, DaemonConfig};
use kobold_proto::northbound::{ClientFrame, ClientServerFrame};
use kobold_proto::{Command, IncomingFrame};

#[tokio::test]
async fn only_one_ui_is_read_write_and_following_are_read_only_until_promoted() {
    let tmp = tempdir().expect("tempdir");
    let socket = tmp.path().join("rw_test.sock");
    let config = DaemonConfig::new(&socket, tmp.path());
    let daemon = Daemon::bind(config).expect("bind daemon");

    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<Command>();
    let (adapter_tx, adapter_rx) = mpsc::channel::<IncomingFrame>(32);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let daemon_handle = tokio::spawn(async move {
        let _ = daemon.run(cmd_tx, adapter_rx, shutdown_rx).await;
    });

    // 1. Connect Client 1 (should be read-write)
    let client1 = DaemonClient::connect(&socket)
        .await
        .expect("connect client 1");
    let (c1_tx, mut c1_rx) = client1.into_channels();

    // Verify client 1 receives initial hydration snapshot
    let snapshot = tokio::time::timeout(Duration::from_secs(2), c1_rx.recv())
        .await
        .expect("timeout waiting for snapshot")
        .expect("c1 snapshot");
    assert!(matches!(snapshot, ClientServerFrame::Snapshot { .. }));

    // 2. Connect Client 2 (should be read-only)
    let client2 = DaemonClient::connect(&socket)
        .await
        .expect("connect client 2");
    let (c2_tx, mut c2_rx) = client2.into_channels();

    let mut c2_got_ro_notice = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline {
        if let Ok(Some(ClientServerFrame::Notice { text })) =
            tokio::time::timeout(Duration::from_millis(200), c2_rx.recv()).await
        {
            if text.contains("read-only") {
                c2_got_ro_notice = true;
                break;
            }
        }
    }
    assert!(c2_got_ro_notice, "client 2 must receive read-only notice");

    // 3. Client 2 (read-only) attempts to send a prompt
    c2_tx
        .send(ClientFrame::Prompt {
            lane: "main".into(),
            text: "malicious prompt from ro client".into(),
        })
        .expect("send frame from c2");

    // Client 2's command should NOT produce an outgoing Command to the adapter.
    // Instead, client 2 receives a notice that command was ignored.
    let mut got_rejected_notice = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline {
        if let Ok(Some(ClientServerFrame::Notice { text })) =
            tokio::time::timeout(Duration::from_millis(200), c2_rx.recv()).await
        {
            if text.contains("Command ignored: client is attached in read-only mode") {
                got_rejected_notice = true;
                break;
            }
        }
    }
    assert!(
        got_rejected_notice,
        "read-only client command must be rejected with notice"
    );

    // Verify adapter never received any command from client 2's attempt
    assert!(
        cmd_rx.try_recv().is_err(),
        "adapter must not receive commands from read-only client"
    );

    // 4. Client 1 (read-write) sends a prompt
    c1_tx
        .send(ClientFrame::Prompt {
            lane: "main".into(),
            text: "legit prompt from rw client".into(),
        })
        .expect("send from c1");

    // Verify adapter receives the prompt command from Client 1
    let cmd = tokio::time::timeout(Duration::from_secs(1), cmd_rx.recv())
        .await
        .expect("timeout waiting for command")
        .expect("received command");
    assert!(matches!(cmd, Command::Send { text, .. } if text == "legit prompt from rw client"));

    // 5. Adapter emits an AG-UI event -> both Client 1 and Client 2 can read it
    adapter_tx
        .send(IncomingFrame::Event {
            lane: "main".into(),
            event: kobold_proto::agui::Incoming::TextMessageContent {
                base: kobold_proto::agui::Base::default(),
                message_id: "m1".into(),
                delta: "Hello from model".into(),
            },
        })
        .await
        .expect("send adapter frame");

    let mut c1_saw_event = false;
    let mut c2_saw_event = false;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline && (!c1_saw_event || !c2_saw_event) {
        if !c1_saw_event {
            if let Ok(Some(ClientServerFrame::Event { .. })) =
                tokio::time::timeout(Duration::from_millis(100), c1_rx.recv()).await
            {
                c1_saw_event = true;
            }
        }
        if !c2_saw_event {
            if let Ok(Some(ClientServerFrame::Event { .. })) =
                tokio::time::timeout(Duration::from_millis(100), c2_rx.recv()).await
            {
                c2_saw_event = true;
            }
        }
    }

    assert!(c1_saw_event, "read-write client must receive stream event");
    assert!(
        c2_saw_event,
        "read-only client must also receive stream event"
    );

    // Adapter finishes the first turn so lane becomes Ready again
    adapter_tx
        .send(IncomingFrame::Event {
            lane: "main".into(),
            event: kobold_proto::agui::Incoming::RunFinished {
                base: kobold_proto::agui::Base::default(),
                thread_id: "main".into(),
                run_id: "r1".into(),
                usage: None,
                result: None,
                outcome: None,
            },
        })
        .await
        .expect("send run finished");

    // 6. Client 1 disconnects (drops c1_tx/c1_rx) -> Client 2 is promoted to read-write!
    drop(c1_tx);
    drop(c1_rx);

    let mut c2_promoted = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline {
        if let Ok(Some(ClientServerFrame::Notice { text })) =
            tokio::time::timeout(Duration::from_millis(200), c2_rx.recv()).await
        {
            if text.contains("promoted to read-write mode") {
                c2_promoted = true;
                break;
            }
        }
    }
    assert!(c2_promoted, "client 2 must be promoted to read-write mode");

    // 7. Client 2 now sends a prompt successfully
    c2_tx
        .send(ClientFrame::Prompt {
            lane: "main".into(),
            text: "promoted prompt from client 2".into(),
        })
        .expect("send from newly promoted client 2");

    let cmd2 = tokio::time::timeout(Duration::from_secs(1), cmd_rx.recv())
        .await
        .expect("timeout waiting for promoted command")
        .expect("received command");
    assert!(matches!(cmd2, Command::Send { text, .. } if text == "promoted prompt from client 2"));

    // Cleanup
    let _ = shutdown_tx.send(true);
    let _ = daemon_handle.await;
}
