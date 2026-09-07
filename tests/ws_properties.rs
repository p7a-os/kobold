use proptest::prelude::*;

use kobold_core::ws::validate_token;
use kobold_proto::codec;
use kobold_proto::northbound::ClientFrame;

proptest! {
    #[test]
    fn prop_token_validation_reflexivity_and_sensitivity(
        token in "[0-9a-f]{64}",
        char_idx in 0usize..64,
        new_char in "[0-9a-f]",
    ) {
        // Reflexivity: A token always validates against itself
        prop_assert!(validate_token(&token, &token));

        // Sensitivity: Modifying any single character invalidates the token
        let mut corrupted = token.clone();
        let target_char = new_char.chars().next().unwrap();
        let orig_char = corrupted.chars().nth(char_idx).unwrap();

        if target_char != orig_char {
            corrupted.replace_range(char_idx..char_idx + 1, &target_char.to_string());
            prop_assert!(!validate_token(&token, &corrupted));
            prop_assert!(!validate_token(&corrupted, &token));
        }

        // Length mismatch rejection
        let truncated = &token[..63];
        prop_assert!(!validate_token(&token, truncated));
        prop_assert!(!validate_token(truncated, &token));

        let extended = format!("{token}0");
        prop_assert!(!validate_token(&token, &extended));
    }

    #[test]
    fn prop_prompt_frame_roundtrip(lane in "[a-z0-9_-]{1,32}", text in "\\PC*") {
        let frame = ClientFrame::Prompt {
            lane: lane.clone(),
            text: text.clone(),
        };

        let encoded = codec::encode(&frame).expect("encode prompt");
        let decoded: ClientFrame = codec::decode(&encoded).expect("decode prompt");

        prop_assert_eq!(decoded, frame);
    }

    #[test]
    fn prop_interrupt_frame_roundtrip(
        lane in "[a-z0-9_-]{1,32}",
        call_id in "[a-z0-9-]{1,64}",
        answers in prop::collection::vec("\\PC*", 0..10),
    ) {
        let frame = ClientFrame::SubmitInterrupt {
            lane,
            call_id,
            answers,
        };

        let encoded = codec::encode(&frame).expect("encode interrupt");
        let decoded: ClientFrame = codec::decode(&encoded).expect("decode interrupt");

        prop_assert_eq!(decoded, frame);
    }

    #[test]
    fn prop_malformed_json_never_panics(corrupted in "\\PC*") {
        // Any random text fed into codec::decode should return an Err or Ok, never panic
        let _ = codec::decode::<ClientFrame>(&corrupted);
    }
}
