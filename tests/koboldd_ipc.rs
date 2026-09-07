//! Integration tests for the Kobold daemon (`koboldd`) and Northbound AG-UI IPC seam.
//!
//! Verifies UDS socket lifecycle, multi-client pub/sub fanout, client disconnect
//! resilience, state hydration snapshots, prompt dispatching, and interrupt cycles.

use std::time::Duration;

use kobold::daemon::{Daemon, DaemonClient, DaemonConfig};
use kobold_proto::agui;
use kobold_proto::northbound::{ClientFrame, ClientServerFrame, LaneStatus, MessageRole};
use kobold_proto::{Command, IncomingFrame};
use tempfile::tempdir;
use tokio::sync::{mpsc, watch};
use tokio::time::timeout;

#[tokio::test]
async fn test_daemon_bind_and_initial_snapshot() {
    let dir = tempdir().unwrap();
    let sock = dir.path().join("test.sock");

    let config = DaemonConfig::new(&sock, dir.path());
    let daemon = Daemon::bind(config).expect("bind daemon");

    let (cmd_tx, _cmd_rx) = mpsc::unbounded_channel::<Command>();
    let (_adapter_tx, adapter_rx) = mpsc::channel::<IncomingFrame>(16);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let daemon_handle = tokio::spawn(async move {
        daemon.run(cmd_tx, adapter_rx, shutdown_rx).await.unwrap();
    });

    // Connect client
    let mut client = timeout(Duration::from_secs(2), DaemonClient::connect(&sock))
        .await
        .expect("connect timeout")
        .expect("connect client");

    // Client should immediately receive hydration snapshot for "main" lane
    let first_frame = timeout(Duration::from_secs(2), client.recv())
        .await
        .expect("recv timeout")
        .expect("read frame")
        .expect("frame present");

    match first_frame {
        ClientServerFrame::Snapshot {
            lane,
            branch,
            status,
            messages,
            ..
        } => {
            assert_eq!(lane, "main");
            assert_eq!(branch, "main");
            assert_eq!(status, LaneStatus::Connecting);
            assert_eq!(messages.len(), 0);
        }
        other => panic!("expected Snapshot, got {:?}", other),
    }

    let _ = shutdown_tx.send(true);
    let _ = daemon_handle.await;
}

#[tokio::test]
async fn test_daemon_turn_streaming() {
    let dir = tempdir().unwrap();
    let sock = dir.path().join("turn.sock");

    let config = DaemonConfig::new(&sock, dir.path());
    let daemon = Daemon::bind(config).expect("bind daemon");

    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<Command>();
    let (adapter_tx, adapter_rx) = mpsc::channel::<IncomingFrame>(16);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let daemon_handle = tokio::spawn(async move {
        daemon.run(cmd_tx, adapter_rx, shutdown_rx).await.unwrap();
    });

    let mut client = DaemonClient::connect(&sock).await.expect("connect");

    // Read initial snapshot
    let snapshot = client.recv().await.unwrap().expect("initial snapshot");
    assert!(matches!(snapshot, ClientServerFrame::Snapshot { .. }));

    // Mark lane ready via adapter transport event
    adapter_tx
        .send(IncomingFrame::Transport(kobold_proto::Transport::Connected))
        .await
        .unwrap();

    // Read transport status change to Ready
    let status_frame = client.recv().await.unwrap().expect("status frame");
    match status_frame {
        ClientServerFrame::StatusChange { lane, status } => {
            assert_eq!(lane, "main");
            assert_eq!(status, LaneStatus::Ready);
        }
        other => panic!("expected StatusChange Ready, got {:?}", other),
    }

    // Client sends prompt
    client
        .send(&ClientFrame::Prompt {
            lane: "main".into(),
            text: "Hello Kobold".into(),
        })
        .await
        .unwrap();

    // Adapter receives command
    let cmd = timeout(Duration::from_secs(2), cmd_rx.recv())
        .await
        .expect("timeout")
        .expect("cmd");
    match cmd {
        Command::Send { lane, text, .. } => {
            assert_eq!(lane, "main");
            assert_eq!(text, "Hello Kobold");
        }
        other => panic!("expected Command::Send, got {:?}", other),
    }

    // Client receives StatusChange::Waiting
    let waiting_frame = client.recv().await.unwrap().expect("waiting frame");
    assert!(matches!(
        waiting_frame,
        ClientServerFrame::StatusChange {
            status: LaneStatus::Waiting,
            ..
        }
    ));

    // Adapter streams text response
    adapter_tx
        .send(IncomingFrame::Event {
            lane: "main".into(),
            event: agui::Incoming::TextMessageContent {
                base: agui::Base::default(),
                message_id: "m1".into(),
                delta: "World".into(),
            },
        })
        .await
        .unwrap();

    // Client receives streamed text delta
    let delta_frame = client.recv().await.unwrap().expect("delta frame");
    match delta_frame {
        ClientServerFrame::Event { lane, event } => {
            assert_eq!(lane, "main");
            match event {
                agui::Incoming::TextMessageContent { delta, .. } => {
                    assert_eq!(delta, "World");
                }
                other => panic!("expected TextMessageContent, got {:?}", other),
            }
        }
        other => panic!("expected Event, got {:?}", other),
    }

    // Adapter finishes turn
    adapter_tx
        .send(IncomingFrame::Event {
            lane: "main".into(),
            event: agui::Incoming::RunFinished {
                base: agui::Base::default(),
                thread_id: "main".into(),
                run_id: "r1".into(),
                usage: None,
                result: None,
                outcome: None,
            },
        })
        .await
        .unwrap();

    // Client receives RunFinished and StatusChange::Ready
    let run_finished = client.recv().await.unwrap().expect("run finished");
    assert!(matches!(
        run_finished,
        ClientServerFrame::Event {
            event: agui::Incoming::RunFinished { .. },
            ..
        }
    ));

    let ready_again = client.recv().await.unwrap().expect("ready again");
    assert!(matches!(
        ready_again,
        ClientServerFrame::StatusChange {
            status: LaneStatus::Ready,
            ..
        }
    ));

    let _ = shutdown_tx.send(true);
    let _ = daemon_handle.await;
}

#[tokio::test]
async fn test_daemon_multi_client_fanout() {
    let dir = tempdir().unwrap();
    let sock = dir.path().join("fanout.sock");

    let config = DaemonConfig::new(&sock, dir.path());
    let daemon = Daemon::bind(config).expect("bind daemon");

    let (cmd_tx, _cmd_rx) = mpsc::unbounded_channel::<Command>();
    let (adapter_tx, adapter_rx) = mpsc::channel::<IncomingFrame>(16);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let daemon_handle = tokio::spawn(async move {
        daemon.run(cmd_tx, adapter_rx, shutdown_rx).await.unwrap();
    });

    // Connect Client A and Client B
    let mut client_a = DaemonClient::connect(&sock)
        .await
        .expect("connect client A");
    let mut client_b = DaemonClient::connect(&sock)
        .await
        .expect("connect client B");

    // Both receive initial snapshot
    let snap_a = client_a.recv().await.unwrap().expect("snap A");
    let snap_b = client_b.recv().await.unwrap().expect("snap B");
    assert!(matches!(snap_a, ClientServerFrame::Snapshot { .. }));
    assert!(matches!(snap_b, ClientServerFrame::Snapshot { .. }));

    // Mark lane ready
    adapter_tx
        .send(IncomingFrame::Transport(kobold_proto::Transport::Connected))
        .await
        .unwrap();

    // Both receive StatusChange::Ready
    let a_ready = client_a.recv().await.unwrap().expect("a ready");
    let mut b_ready = client_b.recv().await.unwrap().expect("b ready");
    if matches!(b_ready, ClientServerFrame::Notice { .. }) {
        b_ready = client_b
            .recv()
            .await
            .unwrap()
            .expect("b ready after notice");
    }
    assert!(matches!(
        a_ready,
        ClientServerFrame::StatusChange {
            status: LaneStatus::Ready,
            ..
        }
    ));
    assert!(matches!(
        b_ready,
        ClientServerFrame::StatusChange {
            status: LaneStatus::Ready,
            ..
        }
    ));

    // Client A sends a prompt
    client_a
        .send(&ClientFrame::Prompt {
            lane: "main".into(),
            text: "Broadcast test".into(),
        })
        .await
        .unwrap();

    // BOTH Client A and Client B must receive StatusChange::Waiting!
    let a_status = client_a.recv().await.unwrap().expect("a status");
    let b_status = client_b.recv().await.unwrap().expect("b status");

    assert!(matches!(
        a_status,
        ClientServerFrame::StatusChange {
            status: LaneStatus::Waiting,
            ..
        }
    ));
    assert!(matches!(
        b_status,
        ClientServerFrame::StatusChange {
            status: LaneStatus::Waiting,
            ..
        }
    ));

    // Adapter streams event
    adapter_tx
        .send(IncomingFrame::Event {
            lane: "main".into(),
            event: agui::Incoming::TextMessageContent {
                base: agui::Base::default(),
                message_id: "m-fanout".into(),
                delta: "Shared delta".into(),
            },
        })
        .await
        .unwrap();

    // BOTH clients receive the delta!
    let a_delta = client_a.recv().await.unwrap().expect("a delta");
    let b_delta = client_b.recv().await.unwrap().expect("b delta");

    match (a_delta, b_delta) {
        (
            ClientServerFrame::Event {
                event: agui::Incoming::TextMessageContent { delta: d_a, .. },
                ..
            },
            ClientServerFrame::Event {
                event: agui::Incoming::TextMessageContent { delta: d_b, .. },
                ..
            },
        ) => {
            assert_eq!(d_a, "Shared delta");
            assert_eq!(d_b, "Shared delta");
        }
        other => panic!("expected TextMessageContent on both, got {:?}", other),
    }

    let _ = shutdown_tx.send(true);
    let _ = daemon_handle.await;
}

#[tokio::test]
async fn test_daemon_client_disconnect_reconnect_resilience() {
    let dir = tempdir().unwrap();
    let sock = dir.path().join("resilience.sock");

    let config = DaemonConfig::new(&sock, dir.path());
    let daemon = Daemon::bind(config).expect("bind daemon");

    let (cmd_tx, _cmd_rx) = mpsc::unbounded_channel::<Command>();
    let (adapter_tx, adapter_rx) = mpsc::channel::<IncomingFrame>(16);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let daemon_handle = tokio::spawn(async move {
        daemon.run(cmd_tx, adapter_rx, shutdown_rx).await.unwrap();
    });

    // Client 1 connects and sends prompt
    {
        let mut client1 = DaemonClient::connect(&sock)
            .await
            .expect("connect client 1");
        let _ = client1.recv().await.unwrap(); // snapshot

        adapter_tx
            .send(IncomingFrame::Transport(kobold_proto::Transport::Connected))
            .await
            .unwrap();

        let _ = client1.recv().await.unwrap(); // status ready

        client1
            .send(&ClientFrame::Prompt {
                lane: "main".into(),
                text: "Detached message".into(),
            })
            .await
            .unwrap();

        let _ = client1.recv().await.unwrap(); // status waiting

        // Client 1 gracefully detaches
        client1.send(&ClientFrame::Detach).await.unwrap();
        // Client 1 is dropped and socket closed
    }

    // Adapter completes the turn while no frontend is connected
    adapter_tx
        .send(IncomingFrame::Event {
            lane: "main".into(),
            event: agui::Incoming::TextMessageContent {
                base: agui::Base::default(),
                message_id: "m-detached".into(),
                delta: "Completed offline".into(),
            },
        })
        .await
        .unwrap();

    adapter_tx
        .send(IncomingFrame::Event {
            lane: "main".into(),
            event: agui::Incoming::RunFinished {
                base: agui::Base::default(),
                thread_id: "main".into(),
                run_id: "r-detached".into(),
                usage: None,
                result: None,
                outcome: None,
            },
        })
        .await
        .unwrap();

    // Client 2 connects AFTER turn finished
    let mut client2 = DaemonClient::connect(&sock)
        .await
        .expect("connect client 2");

    // Client 2 receives hydration snapshot with full history including the completed turn!
    let snapshot = client2.recv().await.unwrap().expect("snapshot");
    match snapshot {
        ClientServerFrame::Snapshot {
            lane,
            status,
            messages,
            ..
        } => {
            assert_eq!(lane, "main");
            assert_eq!(status, LaneStatus::Ready);
            assert_eq!(messages.len(), 2);
            assert_eq!(messages[0].role, MessageRole::User);
            assert_eq!(messages[0].text, "Detached message");
            assert_eq!(messages[1].role, MessageRole::Model);
            assert_eq!(messages[1].text, "Completed offline");
        }
        other => panic!("expected Snapshot with hydrated history, got {:?}", other),
    }

    let _ = shutdown_tx.send(true);
    let _ = daemon_handle.await;
}

#[tokio::test]
async fn test_daemon_lane_fork() {
    let dir = tempdir().unwrap();
    let sock = dir.path().join("fork.sock");

    let config = DaemonConfig::new(&sock, dir.path());
    let daemon = Daemon::bind(config).expect("bind daemon");

    let (cmd_tx, _cmd_rx) = mpsc::unbounded_channel::<Command>();
    let (_adapter_tx, adapter_rx) = mpsc::channel::<IncomingFrame>(16);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let daemon_handle = tokio::spawn(async move {
        daemon.run(cmd_tx, adapter_rx, shutdown_rx).await.unwrap();
    });

    let mut client = DaemonClient::connect(&sock).await.expect("connect");
    let _ = client.recv().await.unwrap(); // initial snapshot

    // Request fork
    client
        .send(&ClientFrame::Fork {
            new_lane: "forked-lane".into(),
            parent_branch: "main".into(),
            parent_at: 0,
        })
        .await
        .unwrap();

    // Client receives snapshot for newly created forked lane
    let fork_snapshot = client.recv().await.unwrap().expect("fork snapshot");
    match fork_snapshot {
        ClientServerFrame::Snapshot { lane, .. } => {
            assert_eq!(lane, "forked-lane");
        }
        other => panic!("expected Snapshot for forked-lane, got {:?}", other),
    }

    let _ = shutdown_tx.send(true);
    let _ = daemon_handle.await;
}

#[tokio::test]
async fn test_daemon_interrupt_cycle() {
    let dir = tempdir().unwrap();
    let sock = dir.path().join("interrupt.sock");

    let config = DaemonConfig::new(&sock, dir.path());
    let daemon = Daemon::bind(config).expect("bind daemon");

    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<Command>();
    let (adapter_tx, adapter_rx) = mpsc::channel::<IncomingFrame>(16);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let daemon_handle = tokio::spawn(async move {
        daemon.run(cmd_tx, adapter_rx, shutdown_rx).await.unwrap();
    });

    let mut client = DaemonClient::connect(&sock).await.expect("connect");
    let _ = client.recv().await.unwrap(); // initial snapshot

    adapter_tx
        .send(IncomingFrame::Transport(kobold_proto::Transport::Connected))
        .await
        .unwrap();
    let _ = client.recv().await.unwrap(); // status ready

    // Adapter initiates an `ask` tool call sequence
    adapter_tx
        .send(IncomingFrame::Event {
            lane: "main".into(),
            event: agui::Incoming::ToolCallStart {
                base: agui::Base::default(),
                parent_message_id: None,
                tool_call_id: "c-ask-1".into(),
                tool_call_name: "ask".into(),
            },
        })
        .await
        .unwrap();

    adapter_tx
        .send(IncomingFrame::Event {
            lane: "main".into(),
            event: agui::Incoming::ToolCallArgs {
                base: agui::Base::default(),
                tool_call_id: "c-ask-1".into(),
                delta: r#"{"question":"Deploy changes?","options":["yes","no"],"multiple":false}"#
                    .into(),
            },
        })
        .await
        .unwrap();

    adapter_tx
        .send(IncomingFrame::Event {
            lane: "main".into(),
            event: agui::Incoming::ToolCallEnd {
                base: agui::Base::default(),
                tool_call_id: "c-ask-1".into(),
            },
        })
        .await
        .unwrap();

    // Client receives Snapshot containing the active interrupt
    let interrupt_frame = client.recv().await.unwrap().expect("interrupt frame");
    match interrupt_frame {
        ClientServerFrame::Snapshot {
            active_interrupt: Some(ask),
            ..
        } => {
            assert_eq!(ask.call_id, "c-ask-1");
            assert_eq!(ask.question, "Deploy changes?");
            assert_eq!(ask.options, vec!["yes", "no"]);
            assert!(!ask.multi_select);
        }
        other => panic!("expected Snapshot with active_interrupt, got {:?}", other),
    }

    // Client responds with answer
    client
        .send(&ClientFrame::SubmitInterrupt {
            lane: "main".into(),
            call_id: "c-ask-1".into(),
            answers: vec!["yes".into()],
        })
        .await
        .unwrap();

    // Adapter receives Command::ToolResult with the user's answer
    let cmd = timeout(Duration::from_secs(2), cmd_rx.recv())
        .await
        .expect("timeout")
        .expect("cmd");
    match cmd {
        Command::ToolResult {
            call_id,
            output,
            error,
            lane,
        } => {
            assert_eq!(lane, "main");
            assert_eq!(call_id, "c-ask-1");
            assert_eq!(output, "yes");
            assert!(!error);
        }
        other => panic!("expected ToolResult, got {:?}", other),
    }

    let _ = shutdown_tx.send(true);
    let _ = daemon_handle.await;
}
