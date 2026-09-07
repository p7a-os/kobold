//! Terminal escape sequences beyond progress reporting.
//!
//! Everything here is one-way: the application emits, the terminal acts. There
//! is no capability handshake for any of it, so support is decided by terminal
//! identity -- the same approach cargo takes for progress bars, and for the
//! same reason: an unsupported terminal prints the sequence as visible junk.

use std::io::Write;

fn env_has(key: &str, needle: &str) -> bool {
    std::env::var(key)
        .map(|v| v.to_ascii_lowercase().contains(needle))
        .unwrap_or(false)
}

fn emit(seq: &str) {
    let mut out = std::io::stdout();
    let _ = out.write_all(seq.as_bytes());
    let _ = out.flush();
}

/// OSC 52: put text on the system clipboard.
///
/// Works through SSH, which is the whole point -- the terminal on the near end
/// does the copying, so a remote process can reach the local clipboard.
pub fn copy(text: &str) {
    if !clipboard_supported() {
        return;
    }
    emit(&format!("\x1b]52;c;{}\x07", base64(text.as_bytes())));
}

pub fn clipboard_supported() -> bool {
    env_has("TERM_PROGRAM", "ghostty")
        || env_has("TERM", "ghostty")
        || env_has("TERM_PROGRAM", "wezterm")
        || env_has("TERM_PROGRAM", "iterm")
        || env_has("TERM", "kitty")
        || env_has("TERM", "alacritty")
        || std::env::var_os("TMUX").is_some()
}

/// OSC 8: wrap text so the terminal makes it clickable.
///
/// There is no way to *ask* a terminal whether it supports this -- no query,
/// no terminfo capability -- so this is an allowlist. Getting it wrong in the
/// permissive direction leaves escape bytes on screen.
pub fn hyperlinks_supported() -> bool {
    if std::env::var_os("KOBOLD_NO_HYPERLINKS").is_some() {
        return false;
    }
    env_has("TERM_PROGRAM", "ghostty")
        || env_has("TERM", "ghostty")
        || env_has("TERM_PROGRAM", "wezterm")
        || env_has("TERM_PROGRAM", "iterm")
        || env_has("TERM", "kitty")
        || env_has("TERM", "alacritty")
        // GNOME Terminal and other VTE terminals, from 0.50.
        || std::env::var("VTE_VERSION").ok().and_then(|v| v.parse::<u32>().ok()).is_some_and(|v| v >= 5000)
}

/// The text with the link markers around it, for embedding in rendered output.
pub fn link(url: &str, label: &str) -> String {
    format!("\x1b]8;;{url}\x1b\\{label}\x1b]8;;\x1b\\")
}

/// OSC 0 sets both the window and icon title; OSC 2 sets the window title only.
/// Control characters are stripped: the sequence ends at a BEL, so text
/// containing one would truncate the title and leak the rest onto the screen.
pub fn title(text: &str) {
    let clean: String = text.chars().filter(|c| !c.is_control()).take(120).collect();
    emit(&format!("\x1b]2;{clean}\x07"));
}

/// Undoes `title`. An empty `Pt` is the documented way to clear an OSC-2
/// title -- xterm's own control-sequence reference does not spell out what a
/// terminal does with it beyond "change window title to `Pt`", but every
/// terminal this crate targets (xterm, iTerm2, kitty, Alacritty, VTE, Ghostty)
/// takes an empty title to mean "stop overriding" and falls back to its own
/// default, which is what leaving a stale title behind after exit calls for.
/// Same reasoning `restore` already applies to the keyboard-enhancement flags
/// at push time: a sequence emitted on the way in has to be undone on the way
/// out, or it outlives the process.
pub fn reset_title() {
    emit("\x1b]2;\x07");
}

/// Standard base64, no line breaks. Hand-rolled to avoid a dependency for
/// forty lines of table lookup.
fn base64(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        out.push(T[(n >> 18 & 63) as usize] as char);
        out.push(T[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6 & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        // Multi-byte input must be treated as bytes, not characters.
        assert_eq!(base64("é".as_bytes()), "w6k=");
    }

    #[test]
    fn link_wraps_the_label_only() {
        assert_eq!(
            link("https://x.dev", "docs"),
            "\x1b]8;;https://x.dev\x1b\\docs\x1b]8;;\x1b\\"
        );
    }

    #[test]
    fn title_cannot_be_truncated_by_its_own_content() {
        // A BEL in the text would end the sequence early and print the rest.
        let clean: String = "hi\x07there\n"
            .chars()
            .filter(|c| !c.is_control())
            .collect();
        assert_eq!(clean, "hithere");
    }
}
