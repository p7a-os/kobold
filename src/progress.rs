//! Terminal progress reporting via OSC 9;4.
//!
//! This is the sequence cargo uses: the application *reports* progress and the
//! terminal draws it in its own chrome (Ghostty puts a bar at the top of the
//! split). It is strictly one-way -- there is no interaction, no gestures, no
//! events back. An application can only say "working", "x percent", "failed",
//! or "done".
//!
//! Gated on terminals known to support it, like cargo's own implementation:
//! elsewhere the sequence is printed as visible garbage.

use std::io::Write;

#[derive(Clone, Copy, PartialEq)]
pub enum State {
    /// Remove the bar.
    Clear,
    /// 0-100.
    At(u8),
    Error,
    /// Working, with no idea how far along.
    Indeterminate,
}

/// Whether the terminal will render the sequence rather than print it.
pub fn supported() -> bool {
    let is = |k: &str, v: &str| {
        std::env::var(k)
            .map(|got| got.to_ascii_lowercase().contains(v))
            .unwrap_or(false)
    };
    is("TERM_PROGRAM", "ghostty")
        || is("TERM", "ghostty")
        || is("TERM_PROGRAM", "wezterm")
        || std::env::var_os("WT_SESSION").is_some()
        || std::env::var_os("ConEmuANSI").is_some()
}

/// Report progress. A no-op where it would be printed literally.
///
/// Written to stderr, not stdout, and only when stderr is a terminal.
///
/// stdout is the data channel: `-p` prints the model's reply there for a
/// caller to pipe somewhere, and an escape sequence written into it is
/// corruption of that caller's data rather than decoration. It was, until this
/// was fixed -- `kobold -p ... > file` embedded two OSC sequences in the file,
/// on any terminal that supports the bar.
///
/// stderr reaches the same terminal when there is one, so nothing is lost, and
/// it keeps the bar out of the synchronized frame the TUI is writing to stdout
/// as well.
pub fn set(state: State) {
    if !supported() || !std::io::IsTerminal::is_terminal(&std::io::stderr()) {
        return;
    }
    let seq = match state {
        State::Clear => "\x1b]9;4;0;\x07".to_owned(),
        State::At(pct) => format!("\x1b]9;4;1;{}\x07", pct.min(100)),
        State::Error => "\x1b]9;4;2;\x07".to_owned(),
        State::Indeterminate => "\x1b]9;4;3;\x07".to_owned(),
    };
    let mut out = std::io::stderr();
    let _ = out.write_all(seq.as_bytes());
    let _ = out.flush();
}

/// Clears on drop, including while unwinding. A bar left behind outlives the
/// process and there is no way for the user to dismiss it.
pub struct Guard;

impl Drop for Guard {
    fn drop(&mut self) {
        set(State::Clear);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequences_match_the_conemu_spec() {
        // Asserted on the bytes rather than on behaviour: nothing here can be
        // observed from a test, and a typo would only show up as garbage in
        // someone's terminal.
        let render = |s: State| match s {
            State::Clear => "\x1b]9;4;0;\x07".to_owned(),
            State::At(p) => format!("\x1b]9;4;1;{}\x07", p.min(100)),
            State::Error => "\x1b]9;4;2;\x07".to_owned(),
            State::Indeterminate => "\x1b]9;4;3;\x07".to_owned(),
        };
        assert_eq!(render(State::At(42)), "\x1b]9;4;1;42\x07");
        assert_eq!(
            render(State::At(255)),
            "\x1b]9;4;1;100\x07",
            "percent must be clamped"
        );
        assert_eq!(render(State::Indeterminate), "\x1b]9;4;3;\x07");
        assert_eq!(render(State::Clear), "\x1b]9;4;0;\x07");
    }
}
