//! Contract tests for Southbound PTY & tmux adapter wire format.

use kobold_proto::agui::{self, Base};
use kobold_proto::codec;
use kobold_proto::{Command, Model, OutgoingFrame, Startup, Transport};

#[test]
fn test_tmux_adapter_startup_wire_contract() {
    let startup = Startup {
        egress: None,
        api_key: "dummy-key".into(),
        model: Model {
            name: "tmux-pty".into(),
            effort: "low".into(),
            server_tools: Vec::new(),
            tools: Vec::new(),
        },
    };

    let encoded = codec::encode(&startup).expect("encode startup");
    assert!(encoded.ends_with('\n'), "wire frame must end with newline");
    let decoded: Startup = codec::decode(&encoded).expect("decode startup");
    assert_eq!(decoded, startup);
}

#[test]
fn test_tmux_adapter_outgoing_frame_contract() {
    // 1. Transport Connected
    let transport = OutgoingFrame::Transport(Transport::Connected);
    let enc_t = codec::encode(&transport).expect("encode transport");
    assert!(enc_t.contains("Connected"));

    // 2. RunStarted event
    let started = OutgoingFrame::Event {
        lane: "main".into(),
        event: agui::Outgoing::RunStarted {
            base: Base::default(),
            run_id: "r-test-1".into(),
            thread_id: "main".into(),
            parent_run_id: None,
        },
    };
    let enc_start = codec::encode(&started).expect("encode start");
    assert!(enc_start.contains("\"type\":\"RUN_STARTED\""));
    assert!(enc_start.contains("\"runId\":\"r-test-1\""));

    // 3. TextMessageContent event
    let delta = OutgoingFrame::Event {
        lane: "main".into(),
        event: agui::Outgoing::TextMessageContent {
            base: Base::default(),
            message_id: "m-test-1".into(),
            delta: "Terminal output chunk".into(),
        },
    };
    let enc_delta = codec::encode(&delta).expect("encode delta");
    assert!(enc_delta.contains("\"type\":\"TEXT_MESSAGE_CONTENT\""));
    assert!(enc_delta.contains("Terminal output chunk"));

    // 4. Synthetic Ask ToolCall
    let ask_start = OutgoingFrame::Event {
        lane: "main".into(),
        event: agui::Outgoing::ToolCallStart {
            base: Base::default(),
            tool_call_id: "ask-123".into(),
            tool_call_name: "ask".into(),
            parent_message_id: None,
        },
    };
    let enc_ask = codec::encode(&ask_start).expect("encode ask");
    assert!(enc_ask.contains("\"type\":\"TOOL_CALL_START\""));
    assert!(enc_ask.contains("\"toolCallName\":\"ask\""));

    // 5. RunFinished event
    let finished = OutgoingFrame::Event {
        lane: "main".into(),
        event: agui::Outgoing::RunFinished {
            base: Base::default(),
            run_id: "r-test-1".into(),
            thread_id: "main".into(),
            usage: None,
        },
    };
    let enc_fin = codec::encode(&finished).expect("encode finished");
    assert!(enc_fin.contains("\"type\":\"RUN_FINISHED\""));
}

#[test]
fn test_tmux_adapter_incoming_commands_contract() {
    let send_cmd = Command::Send {
        lane: "main".into(),
        text: "echo 'hello'".into(),
        previous_response_id: None,
        replay: Vec::new(),
    };
    let enc_send = codec::encode(&send_cmd).expect("encode send");
    let dec_send: Command = codec::decode(&enc_send).expect("decode send");
    assert_eq!(dec_send, send_cmd);

    let tool_res = Command::ToolResult {
        lane: "main".into(),
        call_id: "ask-123".into(),
        output: "Yes".into(),
        error: false,
    };
    let enc_res = codec::encode(&tool_res).expect("encode res");
    let dec_res: Command = codec::decode(&enc_res).expect("decode res");
    assert_eq!(dec_res, tool_res);
}
