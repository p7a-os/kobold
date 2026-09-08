//! Integration tests for the PTY & tmux Southbound adapter.

use std::time::Duration;
use tokio::sync::mpsc;

use kobold_adapter_tmux::bridge::TmuxBridge;
use kobold_adapter_tmux::pty::PtySession;
use kobold_proto::agui;
use kobold_proto::{Command, OutgoingFrame};

#[tokio::test]
async fn test_pty_bridge_command_execution_and_streaming() {
    let pty = PtySession::spawn_in_pty("bash", &["--noprofile".into(), "--norc".into()])
        .expect("spawn bash in pty");
    let mut bridge = TmuxBridge::new(pty, "main");

    let (frame_tx, mut frame_rx) = mpsc::unbounded_channel::<OutgoingFrame>();

    // Send command: echo a known token
    let cmd = Command::Send {
        lane: "main".into(),
        text: "echo 'kobold-pty-alive'".into(),
        previous_response_id: None,
        replay: Vec::new(),
    };

    let keep_running = bridge
        .handle_command(cmd, &frame_tx)
        .await
        .expect("handle send");
    assert!(keep_running);

    let mut output_text = String::new();
    let mut saw_run_finished = false;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while tokio::time::Instant::now() < deadline {
        if let Ok(Some(OutgoingFrame::Event { event, .. })) =
            tokio::time::timeout(Duration::from_millis(100), frame_rx.recv()).await
        {
            match event {
                agui::Outgoing::TextMessageContent { delta, .. } => {
                    output_text.push_str(&delta);
                }
                agui::Outgoing::RunFinished { .. } => {
                    saw_run_finished = true;
                    break;
                }
                _ => {}
            }
        }
    }

    assert!(
        output_text.contains("kobold-pty-alive"),
        "expected output to contain token, got: {output_text}"
    );
    assert!(saw_run_finished, "expected RunFinished event");

    // Clean exit on Quit
    let should_continue = bridge
        .handle_command(Command::Quit, &frame_tx)
        .await
        .expect("quit");
    assert!(!should_continue);
}

#[tokio::test]
async fn test_pty_bridge_interactive_prompt_interception_and_answer() {
    let pty = PtySession::spawn_in_pty("bash", &["--noprofile".into(), "--norc".into()])
        .expect("spawn bash in pty");
    let mut bridge = TmuxBridge::new(pty, "main");

    let (frame_tx, mut frame_rx) = mpsc::unbounded_channel::<OutgoingFrame>();

    let dir = tempfile::tempdir().expect("create temp dir");
    let script_path = dir.path().join("prompt.sh");
    std::fs::write(
        &script_path,
        "#!/bin/bash\nread -p 'Do you want to proceed? [y/n] ' ans\necho \"RESULT:$ans\"\n",
    )
    .expect("write script");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&script_path).expect("meta").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script_path, perms).expect("chmod");
    }

    // Send command that executes the script
    let cmd = Command::Send {
        lane: "main".into(),
        text: format!("bash {}", script_path.display()),
        previous_response_id: None,
        replay: Vec::new(),
    };

    let spawn_tx = frame_tx.clone();
    let bridge_handle = tokio::spawn(async move {
        bridge.handle_command(cmd, &spawn_tx).await.unwrap();
        bridge
    });

    let mut intercepted_call_id: Option<String> = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline {
        if let Ok(Some(OutgoingFrame::Event {
            event:
                agui::Outgoing::ToolCallStart {
                    tool_call_id,
                    tool_call_name,
                    ..
                },
            ..
        })) = tokio::time::timeout(Duration::from_millis(100), frame_rx.recv()).await
        {
            if tool_call_name == "ask" {
                intercepted_call_id = Some(tool_call_id);
                break;
            }
        }
    }

    let call_id = intercepted_call_id.expect("must intercept ask tool call from prompt");

    // Reacquire bridge to inject answer
    let mut bridge = bridge_handle.await.expect("bridge join");

    // Send ToolResult with approval: "Yes"
    let answer_cmd = Command::ToolResult {
        lane: "main".into(),
        call_id,
        output: "Yes".into(),
        error: false,
    };

    let keep_running = bridge
        .handle_command(answer_cmd, &frame_tx)
        .await
        .expect("handle tool result");
    assert!(keep_running);

    let mut output_text = String::new();
    let mut saw_run_finished = false;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while tokio::time::Instant::now() < deadline {
        if let Ok(Some(OutgoingFrame::Event { event, .. })) =
            tokio::time::timeout(Duration::from_millis(100), frame_rx.recv()).await
        {
            match event {
                agui::Outgoing::TextMessageContent { delta, .. } => {
                    output_text.push_str(&delta);
                }
                agui::Outgoing::RunFinished { .. } => {
                    saw_run_finished = true;
                    break;
                }
                _ => {}
            }
        }
    }

    assert!(
        output_text.contains("RESULT:y"),
        "expected script to receive 'y' keystroke, got: {output_text}"
    );
    assert!(saw_run_finished, "expected RunFinished after prompt answer");
}
