//! Property-based testing for ACP protocol parsing and translation invariants.

use kobold_adapter_acp::proto::{JsonRpcMessage, SessionUpdate, SessionUpdateParams};
use kobold_adapter_acp::AcpBridge;
use kobold_proto::agui;
use kobold_proto::OutgoingFrame;
use proptest::prelude::*;

proptest! {
    #[test]
    fn prop_text_delta_survives_acp_translation(
        delta in "\\PC{1,512}",
    ) {
        let mut bridge = AcpBridge::new("sess-prop");
        let update_msg = JsonRpcMessage::notification(
            "session/update",
            serde_json::json!({
                "sessionId": "sess-prop",
                "update": {
                    "type": "text",
                    "delta": delta.clone()
                }
            }),
        );

        let (_, frames) = bridge.handle_agent_message(update_msg);
        prop_assert_eq!(frames.len(), 1);
        if let OutgoingFrame::Event { event: agui::Outgoing::TextMessageContent { delta: out_delta, .. }, .. } = &frames[0] {
            prop_assert_eq!(out_delta.as_str(), delta.as_str());
        } else {
            prop_assert!(false, "expected TextMessageContent");
        }
    }

    #[test]
    fn prop_arbitrary_jsonrpc_notification_never_panics(
        method in "[a-zA-Z0-9_/-]{1,32}",
        delta in "\\PC{0,256}",
    ) {
        let mut bridge = AcpBridge::new("sess-prop-2");
        let msg = JsonRpcMessage::notification(
            method,
            serde_json::json!({ "raw": delta }),
        );

        // State machine must never panic on arbitrary notification methods
        let _ = bridge.handle_agent_message(msg);
    }

    #[test]
    fn prop_session_update_roundtrip(
        delta in "\\PC{1,256}",
        session_id in "[a-zA-Z0-9_-]{1,32}",
    ) {
        let update = SessionUpdate::Text { delta: delta.clone() };
        let params = SessionUpdateParams {
            session_id: session_id.clone(),
            update,
        };

        let serialized = sonic_rs::to_vec(&params).expect("serialize");
        let deserialized: SessionUpdateParams = sonic_rs::from_slice(&serialized).expect("deserialize");
        prop_assert_eq!(deserialized.session_id, session_id);
        prop_assert_eq!(deserialized.update, SessionUpdate::Text { delta });
    }
}
