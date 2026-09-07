//! Contract tests verifying Northbound AG-UI framing and wire protocol conformance.

use kobold_proto::agui;
use kobold_proto::codec;
use kobold_proto::northbound::{
    AskRecord, ClientFrame, ClientServerFrame, LaneStatus, MessageRole, ServerFrame,
    TranscriptRecord,
};

#[test]
fn contract_client_frame_exact_keys() {
    let frame = ClientFrame::Prompt {
        lane: "main".into(),
        text: "hello world\nmultiline".into(),
    };
    let line = codec::encode(&frame).expect("encode");

    // Must be exactly one line
    assert_eq!(line.chars().filter(|c| *c == '\n').count(), 1);
    assert!(line.ends_with('\n'));

    // JSON structure contains Prompt, lane, text
    assert!(line.contains("\"Prompt\""));
    assert!(line.contains("\"lane\":\"main\""));
    assert!(line.contains("\"text\":\"hello world\\nmultiline\""));

    // Decodes back identically
    let decoded: ClientFrame = codec::decode(&line).expect("decode");
    assert_eq!(decoded, frame);
}

#[test]
fn contract_server_frame_to_client_server_frame() {
    let server_frame = ServerFrame::Snapshot {
        lane: "dev".into(),
        branch: "b1".into(),
        messages: vec![
            TranscriptRecord {
                role: MessageRole::User,
                text: "question".into(),
                response_id: None,
            },
            TranscriptRecord {
                role: MessageRole::Model,
                text: "answer".into(),
                response_id: Some("resp-99".into()),
            },
        ],
        active_interrupt: Some(AskRecord {
            call_id: "c-1".into(),
            question: "Confirm?".into(),
            options: vec!["yes".into(), "no".into()],
            multi_select: false,
        }),
        status: LaneStatus::Ready,
    };

    let line = codec::encode(&server_frame).expect("encode server frame");
    assert!(line.ends_with('\n'));
    assert_eq!(line.chars().filter(|c| *c == '\n').count(), 1);

    // Frontends deserialize into ClientServerFrame
    let client_frame: ClientServerFrame = codec::decode(&line).expect("decode client server frame");
    match client_frame {
        ClientServerFrame::Snapshot {
            lane,
            branch,
            messages,
            active_interrupt,
            status,
        } => {
            assert_eq!(lane, "dev");
            assert_eq!(branch, "b1");
            assert_eq!(status, LaneStatus::Ready);
            assert_eq!(messages.len(), 2);
            assert_eq!(messages[0].role, MessageRole::User);
            assert_eq!(messages[0].text, "question");
            assert_eq!(messages[1].role, MessageRole::Model);
            assert_eq!(messages[1].text, "answer");
            assert_eq!(messages[1].response_id.as_deref(), Some("resp-99"));

            let ask = active_interrupt.expect("active interrupt");
            assert_eq!(ask.call_id, "c-1");
            assert_eq!(ask.question, "Confirm?");
            assert_eq!(ask.options, vec!["yes", "no"]);
            assert!(!ask.multi_select);
        }
        other => panic!("expected Snapshot, got {:?}", other),
    }
}

#[test]
fn contract_streaming_event_round_trip() {
    let server_event = ServerFrame::Event {
        lane: "main".into(),
        event: agui::Outgoing::TextMessageContent {
            base: agui::Base::default(),
            message_id: "msg-42".into(),
            delta: "streaming token \"with quotes\" and \nnewlines".into(),
        },
    };

    let line = codec::encode(&server_event).expect("encode event");
    assert_eq!(line.chars().filter(|c| *c == '\n').count(), 1);

    let client_frame: ClientServerFrame = codec::decode(&line).expect("decode event");
    match client_frame {
        ClientServerFrame::Event { lane, event } => {
            assert_eq!(lane, "main");
            match event {
                agui::Incoming::TextMessageContent {
                    delta, message_id, ..
                } => {
                    assert_eq!(delta, "streaming token \"with quotes\" and \nnewlines");
                    assert_eq!(message_id, "msg-42");
                }
                other => panic!("expected TextMessageContent, got {:?}", other),
            }
        }
        other => panic!("expected Event, got {:?}", other),
    }
}
