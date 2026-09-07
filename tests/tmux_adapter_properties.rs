//! Property-based tests for ANSI parsing and prompt detection invariants.

use kobold_adapter_tmux::ansi::strip_ansi;
use kobold_adapter_tmux::prompts::{detect_prompt, translate_answer_to_keystrokes};
use proptest::prelude::*;

proptest! {
    #[test]
    fn prop_strip_ansi_never_panics_and_removes_csi(
        input in "\\PC*"
    ) {
        let stripped = strip_ansi(&input);
        // Stripped output must never contain CSI opening sequence
        prop_assert!(!stripped.contains("\x1b["), "output still contains CSI escape code: {stripped:?}");
        // Output must be valid UTF-8
        prop_assert!(std::str::from_utf8(stripped.as_bytes()).is_ok());
    }

    #[test]
    fn prop_detect_prompt_invariants(
        prefix in "[a-zA-Z0-9 ]{0,20}",
        tag in prop_oneof![
            Just("[y/n]"),
            Just("(y/n)"),
            Just("[y/N]"),
            Just("[yes/no]"),
            Just("Are you sure?"),
            Just("Allow command?"),
            Just("Press Enter to continue"),
        ],
        suffix in "[ !?]{0,5}",
    ) {
        let prompt_text = format!("{prefix} {tag}{suffix}");
        let detected = detect_prompt(&prompt_text);
        prop_assert!(detected.is_some(), "prompt should be detected: {prompt_text}");
        let p = detected.unwrap();
        prop_assert!(!p.options.is_empty(), "prompt must provide options");
    }

    #[test]
    fn prop_translate_answer_always_ends_in_newline(
        answer in "[a-zA-Z0-9_-]{1,30}"
    ) {
        let keystrokes = translate_answer_to_keystrokes(&answer);
        prop_assert!(keystrokes.ends_with('\n'), "keystrokes must end in newline for terminal submission");
    }
}
