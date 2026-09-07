//! Contract tests for the Northbound WebSocket protocol and Web Companion UI.

use kobold_core::ws::WEB_COMPANION_HTML;
use kobold_proto::agui;
use kobold_proto::codec;
use kobold_proto::northbound::{
    AskRecord, ClientFrame, ClientServerFrame, LaneStatus, MessageRole, ServerFrame,
    TranscriptRecord,
};

#[test]
fn test_ws_client_frames_wire_contract() {
    // 1. Prompt
    let prompt = ClientFrame::Prompt {
        lane: "main".into(),
        text: "echo 'hello web'".into(),
    };
    let json = codec::encode(&prompt).expect("encode prompt");
    assert!(json.contains("\"Prompt\""));
    assert!(json.contains("\"lane\":\"main\""));
    assert!(json.contains("\"text\":\"echo 'hello web'\""));
    let decoded: ClientFrame = codec::decode(&json).expect("decode prompt");
    assert_eq!(decoded, prompt);

    // 2. SubmitInterrupt
    let submit = ClientFrame::SubmitInterrupt {
        lane: "main".into(),
        call_id: "ask-0123".into(),
        answers: vec!["Yes".into()],
    };
    let json = codec::encode(&submit).expect("encode submit");
    assert!(json.contains("\"SubmitInterrupt\""));
    assert!(json.contains("\"call_id\":\"ask-0123\""));
    assert!(json.contains("\"answers\":[\"Yes\"]"));
    let decoded: ClientFrame = codec::decode(&json).expect("decode submit");
    assert_eq!(decoded, submit);

    // 3. CancelInterrupt
    let cancel = ClientFrame::CancelInterrupt {
        lane: "main".into(),
        call_id: "ask-0123".into(),
    };
    let json = codec::encode(&cancel).expect("encode cancel");
    assert!(json.contains("\"CancelInterrupt\""));
    let decoded: ClientFrame = codec::decode(&json).expect("decode cancel");
    assert_eq!(decoded, cancel);

    // 4. Fork
    let fork = ClientFrame::Fork {
        new_lane: "feature-1".into(),
        parent_branch: "branch-1".into(),
        parent_at: 42,
    };
    let json = codec::encode(&fork).expect("encode fork");
    assert!(json.contains("\"Fork\""));
    let decoded: ClientFrame = codec::decode(&json).expect("decode fork");
    assert_eq!(decoded, fork);

    // 5. Detach
    let detach = ClientFrame::Detach;
    let json = codec::encode(&detach).expect("encode detach");
    assert!(json.contains("\"Detach\""));
    let decoded: ClientFrame = codec::decode(&json).expect("decode detach");
    assert_eq!(decoded, detach);
}

#[test]
fn test_ws_server_frames_wire_contract() {
    // 1. Snapshot
    let snapshot = ServerFrame::Snapshot {
        lane: "main".into(),
        branch: "main".into(),
        messages: vec![TranscriptRecord {
            role: MessageRole::User,
            text: "hello".into(),
            response_id: None,
        }],
        active_interrupt: Some(AskRecord {
            call_id: "ask-1".into(),
            question: "Proceed?".into(),
            options: vec!["Yes".into(), "No".into()],
            multi_select: false,
        }),
        status: LaneStatus::Ready,
    };
    let json = codec::encode(&snapshot).expect("encode snapshot");
    assert!(json.contains("\"Snapshot\""));
    assert!(json.contains("\"lane\":\"main\""));
    assert!(json.contains("\"Ready\""));
    let decoded: ClientServerFrame = codec::decode(&json).expect("decode snapshot");
    assert!(matches!(decoded, ClientServerFrame::Snapshot { .. }));

    // 2. Event
    let event = ServerFrame::Event {
        lane: "main".into(),
        event: agui::Outgoing::TextMessageContent {
            base: agui::Base::default(),
            message_id: "msg-2".into(),
            delta: "chunk".into(),
        },
    };
    let json = codec::encode(&event).expect("encode event");
    assert!(json.contains("\"Event\""));
    assert!(json.contains("\"TEXT_MESSAGE_CONTENT\""));
    let decoded: ClientServerFrame = codec::decode(&json).expect("decode event");
    assert!(matches!(decoded, ClientServerFrame::Event { .. }));

    // 3. StatusChange
    let status_change = ServerFrame::StatusChange {
        lane: "main".into(),
        status: LaneStatus::Ready,
    };
    let json = codec::encode(&status_change).expect("encode status change");
    assert!(json.contains("\"StatusChange\""));
    assert!(json.contains("\"Ready\""));
    let decoded: ClientServerFrame = codec::decode(&json).expect("decode status change");
    assert!(matches!(decoded, ClientServerFrame::StatusChange { .. }));

    // 4. Notice
    let notice = ServerFrame::Notice {
        text: "Connected in read-only mode".into(),
    };
    let json = codec::encode(&notice).expect("encode notice");
    assert!(json.contains("\"Notice\""));
    assert!(json.contains("read-only mode"));
    let decoded: ClientServerFrame = codec::decode(&json).expect("decode notice");
    assert!(matches!(decoded, ClientServerFrame::Notice { .. }));

    // 5. Error
    let err = ServerFrame::Error {
        message: "Tool failed".into(),
    };
    let json = codec::encode(&err).expect("encode error");
    assert!(json.contains("\"Error\""));
    let decoded: ClientServerFrame = codec::decode(&json).expect("decode error");
    assert!(matches!(decoded, ClientServerFrame::Error { .. }));
}

#[test]
fn test_web_companion_ui_dom_contract() {
    // Verify core UI components exist in HTML bundle
    assert!(WEB_COMPANION_HTML.contains("id=\"prompt-form\""));
    assert!(WEB_COMPANION_HTML.contains("id=\"prompt-input\""));
    assert!(WEB_COMPANION_HTML.contains("id=\"transcript-box\""));
    assert!(WEB_COMPANION_HTML.contains("id=\"interrupt-card\""));
    assert!(WEB_COMPANION_HTML.contains("id=\"interrupt-yes-btn\""));
    assert!(WEB_COMPANION_HTML.contains("id=\"interrupt-no-btn\""));
    assert!(WEB_COMPANION_HTML.contains("id=\"lanes-bar\""));
    assert!(WEB_COMPANION_HTML.contains("id=\"status-dot\""));

    // Verify WebSocket handling functions exist
    assert!(WEB_COMPANION_HTML.contains("function connectWS()"));
    assert!(WEB_COMPANION_HTML.contains("function handleServerFrame(frame)"));
    assert!(WEB_COMPANION_HTML.contains("function renderSnapshot(snap)"));
    assert!(WEB_COMPANION_HTML.contains("function renderEvent(ev)"));
    assert!(WEB_COMPANION_HTML.contains("function submitInterruptAnswer(output)"));
}
