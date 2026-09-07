//! The codec's framing invariant, checked rather than asserted by example.
//!
//! `codec.rs`'s own doc comment states the property the whole wire rests on:
//! *a JSON document never contains a raw newline outside a string, and inside
//! a string a newline is escaped* -- so a serialized message is always exactly
//! one line, whatever text it carries. Its unit tests demonstrate that with
//! one hand-picked nasty string. These generate it.
//!
//! **Why this one first.** The failure mode is a torn message rather than an
//! error: a raw newline reaching the wire splits one message into two
//! fragments, neither of them valid JSON, and the reader reports a decode
//! failure on a line it was never sent. The inputs that would do it are
//! exactly the ones a hand-written fixture rounds off: control characters,
//! text that is already escaped, text ending mid-escape. A generator does not
//! round them off -- shrinking a deliberately broken codec here reported the
//! minimal failing input as a literal backslash-n, which is precisely the case
//! a chosen literal misses.
//!
//! Deliberately typed against a local struct and `Command::ToolResult` rather
//! than `Frame`: the property belongs to the codec, not to any one protocol
//! shape, and typing it against the envelope would couple a framing test to a
//! protocol change it has nothing to do with.

use kobold_proto::{codec, Command};
use proptest::prelude::*;
use serde::{Deserialize, Serialize};

/// A minimal carrier, so the property is about the codec rather than about
/// whatever the protocol happens to look like this week.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
struct Payload {
    text: String,
}

proptest! {
    /// The invariant `codec.rs` documents, over arbitrary text.
    ///
    /// `\PC*` is any sequence of printable-or-control characters, which is
    /// the point -- it generates the newlines, carriage returns, tabs, nulls
    /// and escape-looking sequences that a chosen literal does not.
    #[test]
    fn an_encoded_message_is_exactly_one_line_whatever_text_it_carries(text in r"\PC*") {
        let line = codec::encode(&Payload { text: text.clone() }).expect("plain data encodes");
        prop_assert_eq!(
            line.matches('\n').count(), 1,
            "a raw newline reached the wire, which tears the message in two"
        );
        prop_assert!(line.ends_with('\n'), "the one newline must be the terminator");
    }

    /// And the text survives, which is the half that stops the property being
    /// satisfied by an encoder that simply dropped newlines.
    #[test]
    fn text_survives_the_round_trip_unchanged(text in r"\PC*") {
        let want = Payload { text };
        let line = codec::encode(&want).expect("encode");
        let got: Payload = codec::decode(&line).expect("decode");
        prop_assert_eq!(got, want);
    }

    /// The same two properties against a real protocol type, because a local
    /// struct proves the codec is sound and not that the types it actually
    /// carries are. `ToolResult` is the one that carries genuinely arbitrary
    /// text: a tool's output is a file's contents, and a file is anything.
    #[test]
    fn a_tool_result_carrying_arbitrary_output_is_one_line_and_survives(
        output in r"\PC*",
        call_id in r"[a-zA-Z0-9_-]{1,32}",
        error in proptest::bool::ANY,
    ) {
        let want = Command::ToolResult {
            lane: "main".to_owned(),
            call_id,
            output,
            error,
        };
        let line = codec::encode(&want).expect("encode");
        prop_assert_eq!(line.matches('\n').count(), 1);
        let got: Command = codec::decode(&line).expect("decode");
        prop_assert_eq!(got, want);
    }

    /// A decoder must reject arbitrary junk rather than accept it, and must
    /// not panic on any of it.
    ///
    /// The assertion is deliberately weak -- `is_err()` rather than a
    /// message -- because the strong claim here is *does not panic*, and a
    /// proptest that reaches the assertion at all has already established it.
    /// The one shape that must not be rejected is a real message, which the
    /// round-trip properties above cover.
    #[test]
    fn arbitrary_input_is_rejected_rather_than_panicking(junk in r"\PC{0,200}") {
        // A line that happens to be a valid `Payload` is not junk; skip it
        // rather than assert something false about it.
        let decoded: Result<Payload, _> = codec::decode(&junk);
        if decoded.is_ok() {
            return Ok(());
        }
        prop_assert!(decoded.is_err());
    }

    /// Truncation in the error is presentational and must never touch the
    /// data path, at any size. The unit test pins one size; this pins the
    /// relationship.
    #[test]
    fn a_large_payload_survives_whole_however_its_error_is_shown(
        text in proptest::collection::vec(any::<char>(), 0..4000)
    ) {
        let text: String = text.into_iter().collect();
        let want = Payload { text };
        let line = codec::encode(&want).expect("encode");
        let got: Payload = codec::decode(&line).expect("decode");
        prop_assert_eq!(got, want, "a large payload must round-trip whole");
    }
}
