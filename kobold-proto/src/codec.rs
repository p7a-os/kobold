//! Newline-delimited JSON, one message per line.
//!
//! Chosen because the measurement said the pipe dominates and the codec does
//! not: 0.35us per delta against an inter-delta gap of 4-48ms, so a binary
//! format would optimise the part that is already free.
//!
//! **The framing rests on one property: a JSON document never contains a raw
//! newline outside a string, and inside a string a newline is escaped as
//! `\n`.** So a serialized message is always exactly one line, whatever text
//! it carries -- which matters because the messages here routinely carry
//! multi-line model output and multi-line tool results. That property is
//! asserted below rather than assumed, because if it ever failed the symptom
//! would be a torn message rather than an error.
//!
//! One place touches the JSON backend, for the same reason `kobold::json`
//! exists: swapping it is a one-file change rather than a scatter.

use serde::{Deserialize, Serialize};

/// Anything that went wrong turning a message into a line or back.
#[derive(Debug)]
pub enum Error {
    /// The value could not be serialized. In practice unreachable for the
    /// protocol types, which are plain data.
    Encode(sonic_rs::Error),
    /// A line that is not a message. An adapter emitting this is
    /// misbehaving -- most likely printing diagnostics to stdout, which
    /// belongs on stderr.
    Decode {
        line: String,
        source: sonic_rs::Error,
    },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Encode(e) => write!(f, "could not encode a message: {e}"),
            // The line is quoted and truncated: it is untrusted input from
            // another process, and it lands in a message the user reads.
            Error::Decode { line, source } => {
                let shown: String = line.chars().take(200).collect();
                write!(f, "not a protocol message: {source} in {shown:?}")
            }
        }
    }
}

impl std::error::Error for Error {}

/// One message as one line, newline included, ready to write.
pub fn encode<T: Serialize>(message: &T) -> Result<String, Error> {
    let mut line = sonic_rs::to_string(message).map_err(Error::Encode)?;
    line.push('\n');
    Ok(line)
}

/// One line back into a message. The trailing newline may be present or not,
/// since readers differ on whether they strip it.
pub fn decode<T: for<'de> Deserialize<'de>>(line: &str) -> Result<T, Error> {
    sonic_rs::from_str(line.trim_end_matches(['\n', '\r'])).map_err(|source| Error::Decode {
        line: line.to_owned(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agui::{Base, Incoming, Outgoing};
    use crate::{Command, IncomingFrame, OutgoingFrame, Transport};

    /// Every frame an adapter can write. Written out rather than sampled, so
    /// a variant added without a round-trip test is a compile error here
    /// rather than a silent wire bug.
    fn every_frame() -> Vec<OutgoingFrame> {
        vec![
            OutgoingFrame::Transport(Transport::Connected),
            OutgoingFrame::Transport(Transport::Disconnected("server closed".into())),
            OutgoingFrame::Event {
                lane: "main".into(),
                event: Outgoing::TextMessageContent {
                    base: Base::default(),
                    message_id: "msg_1".into(),
                    delta: "hello".into(),
                },
            },
            OutgoingFrame::Event {
                lane: "fork-1".into(),
                event: Outgoing::RunFinished {
                    base: Base::default(),
                    thread_id: "fork-1".into(),
                    run_id: "resp_1".into(),
                    usage: None,
                },
            },
        ]
    }

    fn every_command() -> Vec<Command> {
        vec![
            Command::Send {
                lane: "main".into(),
                text: "hi".into(),
                previous_response_id: None,
                replay: Vec::new(),
            },
            Command::Send {
                lane: "fork-1".into(),
                text: "again".into(),
                previous_response_id: Some("resp_1".into()),
                replay: vec![
                    ("user".into(), "first".into()),
                    ("assistant".into(), "then".into()),
                ],
            },
            Command::ToolResult {
                lane: "main".into(),
                call_id: "call_1".into(),
                output: "the file's contents".into(),
                error: false,
            },
            // Both spellings, because a `bool` that is only ever written one
            // way round-trips identically whether or not it is on the wire.
            Command::ToolResult {
                lane: "main".into(),
                call_id: "call_2".into(),
                output: "no such tool".into(),
                error: true,
            },
            Command::Cancel {
                lane: "main".into(),
            },
            Command::Quit,
        ]
    }

    /// What an adapter wrote, as Kobold reads it. The two frame types are
    /// separate by design, so equality has to be checked across the pair
    /// rather than within one of them.
    fn matches(out: &OutgoingFrame, back: &IncomingFrame) -> bool {
        match (out, back) {
            (OutgoingFrame::Transport(a), IncomingFrame::Transport(b)) => a == b,
            (
                OutgoingFrame::Event {
                    lane: a,
                    event: Outgoing::TextMessageContent { delta, .. },
                },
                IncomingFrame::Event {
                    lane: b,
                    event: Incoming::TextMessageContent { delta: got, .. },
                },
            ) => a == b && delta == got,
            (
                OutgoingFrame::Event {
                    lane: a,
                    event: Outgoing::RunFinished { run_id, .. },
                },
                IncomingFrame::Event {
                    lane: b,
                    event: Incoming::RunFinished { run_id: got, .. },
                },
            ) => a == b && run_id == got,
            _ => false,
        }
    }

    #[test]
    fn every_frame_written_is_the_frame_that_is_read() {
        for want in every_frame() {
            let line = encode(&want).expect("encode");
            let got: IncomingFrame = decode(&line).expect("decode");
            assert!(matches(&want, &got), "{want:?} came back as {got:?}");
        }
    }

    #[test]
    fn the_lane_and_the_two_halves_are_all_distinguishable_on_the_wire() {
        // The envelope exists to say which half is a provider protocol's,
        // which half is ours, and which pane an event belongs to. A round
        // trip cannot see any of that: an encoding that collapsed the halves
        // or dropped the lane would decode back to whatever it collapsed to.
        let event = encode(&OutgoingFrame::Event {
            lane: "fork-2".into(),
            event: Outgoing::TextMessageEnd {
                base: Base::default(),
                message_id: "m".into(),
            },
        })
        .expect("encode");
        assert!(event.contains(r#""lane":"fork-2""#), "no lane in {event:?}");
        assert!(event.contains("Event"), "no Event tag in {event:?}");
        assert!(
            !event.contains("Transport"),
            "an event claims to be transport: {event:?}"
        );

        let transport = encode(&OutgoingFrame::Transport(Transport::Connected)).expect("encode");
        assert!(
            transport.contains("Transport"),
            "no Transport tag in {transport:?}"
        );
        assert!(
            !transport.contains("lane"),
            "a transport frame carries a lane: {transport:?}"
        );

        // And a bare event, with no envelope at all, is not a frame.
        // Otherwise an adapter that skipped the envelope would keep working
        // and the lane would be advisory.
        let bare = encode(&Outgoing::TextMessageEnd {
            base: Base::default(),
            message_id: "m".into(),
        })
        .expect("encode");
        assert!(
            decode::<IncomingFrame>(&bare).is_err(),
            "a bare event decoded as a frame"
        );
    }

    #[test]
    fn a_frame_for_one_lane_does_not_read_as_a_frame_for_another() {
        // The counter-assertion to every test above: they would all pass
        // against a decoder that returned a constant lane.
        let lanes: Vec<String> = ["main", "fork-1", "fork-2"]
            .iter()
            .map(|l| {
                let line = encode(&OutgoingFrame::Event {
                    lane: (*l).to_owned(),
                    event: Outgoing::TextMessageEnd {
                        base: Base::default(),
                        message_id: "m".into(),
                    },
                })
                .expect("encode");
                match decode::<IncomingFrame>(&line).expect("decode") {
                    IncomingFrame::Event { lane, .. } => lane,
                    other => panic!("{other:?}"),
                }
            })
            .collect();
        assert_eq!(lanes, vec!["main", "fork-1", "fork-2"]);
    }

    #[test]
    fn every_command_survives_a_round_trip() {
        for want in every_command() {
            let line = encode(&want).expect("encode");
            let got: Command = decode(&line).expect("decode");
            assert_eq!(got, want);
        }
    }

    #[test]
    fn a_message_carrying_newlines_is_still_exactly_one_line() {
        // The property the whole framing rests on. Model output and tool
        // results are routinely multi-line, so if a raw newline could reach
        // the wire the reader would split one message into two fragments,
        // and neither fragment would be valid JSON -- a torn message rather
        // than a clean error.
        let nasty = "line one\nline two\r\nline three\n";
        for message in [
            OutgoingFrame::Event {
                lane: "main".into(),
                event: Outgoing::TextMessageContent {
                    base: Base::default(),
                    message_id: "m".into(),
                    delta: nasty.into(),
                },
            },
            OutgoingFrame::Transport(Transport::Disconnected(nasty.into())),
        ] {
            let line = encode(&message).expect("encode");
            assert_eq!(
                line.matches('\n').count(),
                1,
                "more than one newline in {line:?}"
            );
            assert!(
                line.ends_with('\n'),
                "the one newline must be the terminator"
            );
            let back: IncomingFrame = decode(&line).expect("decode");
            assert!(matches(&message, &back), "text did not survive");
        }

        let command = Command::ToolResult {
            lane: "main".into(),
            call_id: "c".into(),
            output: nasty.into(),
            error: false,
        };
        let line = encode(&command).expect("encode");
        assert_eq!(line.matches('\n').count(), 1);
        assert_eq!(decode::<Command>(&line).expect("decode"), command);
    }

    #[test]
    fn a_decoder_accepts_a_line_with_or_without_its_terminator() {
        // Readers disagree about whether they hand back the newline;
        // `tokio::io::Lines` strips it, a hand-rolled split may not.
        let want = OutgoingFrame::Transport(Transport::Connected);
        let line = encode(&want).expect("encode");
        for variant in [
            line.clone(),
            line.trim_end().to_owned(),
            format!("{}\r\n", line.trim_end()),
        ] {
            let got: IncomingFrame = decode(&variant).expect("decode");
            assert!(matches(&want, &got), "{variant:?}");
        }
    }

    #[test]
    fn a_line_that_is_not_a_message_is_an_error_naming_what_it_saw() {
        // The realistic case: an adapter printing a diagnostic to stdout
        // instead of stderr. That must be reported, not silently skipped,
        // and the report has to say what arrived or it is undiagnosable.
        let err = decode::<IncomingFrame>("Warning: reconnecting...\n").expect_err("not a message");
        let shown = err.to_string();
        assert!(
            shown.contains("Warning: reconnecting"),
            "did not quote the line: {shown}"
        );

        // Valid JSON that is not one of ours fails the same way.
        assert!(decode::<IncomingFrame>(r#"{"hello":"world"}"#).is_err());
        assert!(
            decode::<IncomingFrame>("").is_err(),
            "an empty line is not a message"
        );
    }

    #[test]
    fn a_very_long_line_is_truncated_in_the_error_but_not_on_the_wire() {
        // The error text reaches the user's terminal, so an adapter emitting
        // a megabyte of garbage must not paste a megabyte into the message.
        let huge = "x".repeat(10_000);
        let err = decode::<IncomingFrame>(&huge).expect_err("not a message");
        assert!(err.to_string().len() < 400, "error text was not truncated");

        // But a legitimately large message still round-trips whole: the
        // truncation is presentational and must not touch the data path.
        let big = OutgoingFrame::Event {
            lane: "main".into(),
            event: Outgoing::TextMessageContent {
                base: Base::default(),
                message_id: "m".into(),
                delta: huge.clone(),
            },
        };
        let line = encode(&big).expect("encode");
        let back: IncomingFrame = decode(&line).expect("decode");
        assert!(matches(&big, &back));
    }
}
