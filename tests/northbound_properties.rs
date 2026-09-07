//! Property-based tests for Northbound AG-UI protocol framing and robustness.

use kobold_proto::codec;
use kobold_proto::northbound::{ClientFrame, ClientServerFrame, LaneStatus, ServerFrame};
use proptest::prelude::*;

proptest! {
    /// Property: Any arbitrary prompt string (including UTF-8, emojis, newlines, quotes)
    /// produces exactly one line on the wire and round-trips without data corruption.
    #[test]
    fn prop_client_prompt_one_line_and_round_trip(
        lane in "[a-zA-Z0-9_-]{1,32}",
        text in "\\PC*"
    ) {
        let frame = ClientFrame::Prompt {
            lane: lane.clone(),
            text: text.clone(),
        };

        let encoded = codec::encode(&frame).expect("encode prompt");

        // Invariant: Wire message must be exactly one newline-terminated line
        prop_assert!(encoded.ends_with('\n'), "encoded message must end with newline");
        let newline_count = encoded.chars().filter(|c| *c == '\n').count();
        prop_assert_eq!(newline_count, 1, "embedded newlines must be escaped");

        // Invariant: Round trip must preserve contents exactly
        let decoded: ClientFrame = codec::decode(&encoded).expect("decode prompt");
        prop_assert_eq!(decoded, frame);
    }

    /// Property: Arbitrary interrupt submissions round trip intact over the wire.
    #[test]
    fn prop_client_submit_interrupt_round_trip(
        lane in "[a-zA-Z0-9_-]{1,16}",
        call_id in "[a-zA-Z0-9_-]{1,16}",
        answers in prop::collection::vec("\\PC*", 0..10)
    ) {
        let frame = ClientFrame::SubmitInterrupt {
            lane,
            call_id,
            answers,
        };

        let encoded = codec::encode(&frame).expect("encode interrupt");
        prop_assert_eq!(encoded.chars().filter(|c| *c == '\n').count(), 1);

        let decoded: ClientFrame = codec::decode(&encoded).expect("decode interrupt");
        prop_assert_eq!(decoded, frame);
    }

    /// Property: Server notices and errors round trip to ClientServerFrame intact.
    #[test]
    fn prop_server_notice_and_error_round_trip(
        notice_text in "\\PC*",
        error_text in "\\PC*"
    ) {
        // Notice
        let notice_frame = ServerFrame::Notice { text: notice_text.clone() };
        let encoded_notice = codec::encode(&notice_frame).expect("encode notice");
        prop_assert_eq!(encoded_notice.chars().filter(|c| *c == '\n').count(), 1);
        let decoded_notice: ClientServerFrame = codec::decode(&encoded_notice).expect("decode notice");
        prop_assert_eq!(decoded_notice, ClientServerFrame::Notice { text: notice_text });

        // Error
        let error_frame = ServerFrame::Error { message: error_text.clone() };
        let encoded_error = codec::encode(&error_frame).expect("encode error");
        prop_assert_eq!(encoded_error.chars().filter(|c| *c == '\n').count(), 1);
        let decoded_error: ClientServerFrame = codec::decode(&encoded_error).expect("decode error");
        prop_assert_eq!(decoded_error, ClientServerFrame::Error { message: error_text });
    }

    /// Property: Arbitrary status changes round trip to ClientServerFrame intact.
    #[test]
    fn prop_server_status_change_round_trip(
        lane in "[a-zA-Z0-9_-]{1,16}",
        status_code in 0u8..4
    ) {
        let status = match status_code {
            0 => LaneStatus::Connecting,
            1 => LaneStatus::Ready,
            2 => LaneStatus::Waiting,
            _ => LaneStatus::Gone,
        };

        let server_frame = ServerFrame::StatusChange { lane: lane.clone(), status };
        let encoded = codec::encode(&server_frame).expect("encode status");
        prop_assert_eq!(encoded.chars().filter(|c| *c == '\n').count(), 1);

        let decoded: ClientServerFrame = codec::decode(&encoded).expect("decode status");
        prop_assert_eq!(decoded, ClientServerFrame::StatusChange { lane, status });
    }

    /// Property: Arbitrary corrupt or hostile wire lines never panic the decoder.
    #[test]
    fn prop_decoder_never_panics_on_arbitrary_input(
        fuzz_line in "\\PC*"
    ) {
        let _ = codec::decode::<ClientFrame>(&fuzz_line);
        let _ = codec::decode::<ClientServerFrame>(&fuzz_line);
    }
}
