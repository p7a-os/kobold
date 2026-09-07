//! ANSI / VT100 terminal sequence normalization and stripping.
//!
//! Cleans raw terminal byte streams into readable text by stripping
//! VT100 control codes, colors, cursor repositioning escapes, and carriage return jumps.

/// Strips ANSI CSI sequences, OSC sequences, and non-printable control codes.
pub fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Escape sequence start
            match chars.peek() {
                Some('[') => {
                    chars.next(); // consume '['
                                  // CSI sequence: consume parameters and intermediate bytes until final character (0x40..=0x7E)
                    for ch in chars.by_ref() {
                        if ('@'..='~').contains(&ch) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next(); // consume ']'
                                  // OSC sequence: consume until BEL (\x07) or ST (\x1b\)
                    let mut prev = '\0';
                    for ch in chars.by_ref() {
                        if ch == '\x07' || (prev == '\x1b' && ch == '\\') {
                            break;
                        }
                        prev = ch;
                    }
                }
                Some('(') | Some(')') | Some('*') | Some('+') => {
                    chars.next(); // consume set designator
                    chars.next(); // consume character set
                }
                Some(_) => {
                    // Two-character escape sequence (e.g. \x1bM, \x1b7, \x1b8)
                    chars.next();
                }
                None => break,
            }
        } else if c == '\r' {
            // Carriage return: if followed by \n, let \n be processed; otherwise collapse
            if chars.peek() == Some(&'\n') {
                chars.next();
                out.push('\n');
            } else {
                out.push('\n');
            }
        } else if c == '\x08' || c == '\x7f' {
            // Backspace: remove last character from out if in same line
            if out.ends_with(|ch| ch != '\n') {
                out.pop();
            }
        } else if c == '\t' || c == '\n' || (!c.is_control() && c != '\0') {
            out.push(c);
        }
    }

    // Deduplicate multiple consecutive empty lines resulting from CR collapses
    collapse_excessive_newlines(&out)
}

fn collapse_excessive_newlines(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut newline_count = 0;

    for ch in s.chars() {
        if ch == '\n' {
            newline_count += 1;
            if newline_count <= 2 {
                result.push(ch);
            }
        } else {
            newline_count = 0;
            result.push(ch);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strips_color_and_style_escapes() {
        let raw = "\x1b[32mHello\x1b[0m \x1b[1;34mWorld\x1b[0m";
        assert_eq!(strip_ansi(raw), "Hello World");
    }

    #[test]
    fn test_strips_cursor_movement_and_clear() {
        let raw = "\x1b[2J\x1b[HWelcome to terminal\x1b[1A\x1b[2K";
        assert_eq!(strip_ansi(raw), "Welcome to terminal");
    }

    #[test]
    fn test_handles_carriage_returns_and_newlines() {
        let raw = "Loading...\rDone!\r\nSecond line\n";
        assert_eq!(strip_ansi(raw), "Loading...\nDone!\nSecond line\n");
    }

    #[test]
    fn test_strips_osc_title_sequences() {
        let raw = "\x1b]0;Terminal Title\x07Hello from shell";
        assert_eq!(strip_ansi(raw), "Hello from shell");
    }

    #[test]
    fn test_handles_backspaces() {
        let raw = "abc\x08d";
        assert_eq!(strip_ansi(raw), "abd");
    }
}
