//! One line of editable text: a string and a caret, and the operations both
//! places that take typing need.
//!
//! Extracted because there are two such places and there were nearly two
//! implementations. The prompt had the full set -- word motion, the readline
//! kills, delete-forward -- and the panel's free-text field had `type` and
//! `backspace`, so a question asking for a sentence could not be given one
//! with a space in it, let alone corrected. Sharing the logic is what makes
//! the two behave the same rather than merely look similar.
//!
//! Free functions over `(&mut String, &mut usize)` rather than a struct the
//! callers embed: `Pane` keeps its `input` and `cursor` fields, which
//! forty-nine call sites and a good deal of rendering already reach for
//! directly. The behaviour is what wanted sharing, not the layout.
//!
//! The caret counts characters, never bytes, so it cannot land inside a
//! multi-byte one.

/// Pull a caret back to the end of the text if it is somehow past it.
///
/// Nothing here should produce one, but every operation indexes with the
/// caret, and `String::remove` at the end of a string panics rather than
/// returning nothing. A stray caret should cost a keystroke that does nothing,
/// not the session.
fn clamp(text: &str, cursor: &mut usize) {
    // `min` rather than a branch. `if *cursor > end` and `if *cursor >= end`
    // behave identically -- assigning `end` when it is already `end` changes
    // nothing -- so the comparison was a coin-flip no test could ever pin.
    // Removing the branch removes the question.
    *cursor = (*cursor).min(len_chars(text));
}

/// Byte offset of character index `at`, or the end.
fn byte_at(text: &str, at: usize) -> usize {
    text.char_indices()
        .nth(at)
        .map(|(i, _)| i)
        .unwrap_or(text.len())
}

pub fn len_chars(text: &str) -> usize {
    text.chars().count()
}

/// Every operation reports whether it changed anything, so a caller can use it
/// as a dirty flag. A keypress that did nothing -- backspace at the start,
/// left at column zero -- must not cost a repaint.
pub fn insert(text: &mut String, cursor: &mut usize, c: char) -> bool {
    clamp(text, cursor);
    let at = byte_at(text, *cursor);
    text.insert(at, c);
    *cursor += 1;
    true
}

/// Insert a whole run at the caret. Used for paste: with bracketed paste a
/// paste arrives as one event, so it costs one edit and one repaint rather
/// than one of each per character.
pub fn insert_str(text: &mut String, cursor: &mut usize, run: &str) -> bool {
    clamp(text, cursor);
    // A pasted CR, or a CRLF, would otherwise show up as a stray control
    // character in the input.
    let run = run.replace("\r\n", "\n").replace('\r', "\n");
    if run.is_empty() {
        return false;
    }
    let at = byte_at(text, *cursor);
    text.insert_str(at, &run);
    *cursor += run.chars().count();
    true
}

pub fn backspace(text: &mut String, cursor: &mut usize) -> bool {
    clamp(text, cursor);
    if *cursor == 0 {
        return false;
    }
    let at = byte_at(text, *cursor - 1);
    text.remove(at);
    *cursor -= 1;
    true
}

pub fn delete_forward(text: &mut String, cursor: &mut usize) -> bool {
    clamp(text, cursor);
    if *cursor >= len_chars(text) {
        return false;
    }
    let at = byte_at(text, *cursor);
    text.remove(at);
    true
}

/// Whitespace-delimited, the readline convention: skip any run of spaces, then
/// the word itself.
fn word_left(text: &str, cursor: usize) -> usize {
    let chars: Vec<char> = text.chars().collect();
    let mut i = cursor.min(chars.len());
    while i > 0 && chars[i - 1].is_whitespace() {
        i -= 1;
    }
    while i > 0 && !chars[i - 1].is_whitespace() {
        i -= 1;
    }
    i
}

fn word_right(text: &str, cursor: usize) -> usize {
    let chars: Vec<char> = text.chars().collect();
    let mut i = cursor.min(chars.len());
    while i < chars.len() && chars[i].is_whitespace() {
        i += 1;
    }
    while i < chars.len() && !chars[i].is_whitespace() {
        i += 1;
    }
    i
}

pub fn move_left(text: &str, cursor: &mut usize, word: bool) -> bool {
    let to = if word {
        word_left(text, *cursor)
    } else {
        cursor.saturating_sub(1)
    };
    std::mem::replace(cursor, to) != to
}

pub fn move_right(text: &str, cursor: &mut usize, word: bool) -> bool {
    let to = if word {
        word_right(text, *cursor)
    } else {
        (*cursor + 1).min(len_chars(text))
    };
    std::mem::replace(cursor, to) != to
}

pub fn home(cursor: &mut usize) -> bool {
    std::mem::replace(cursor, 0) != 0
}

pub fn end(text: &str, cursor: &mut usize) -> bool {
    let to = len_chars(text);
    std::mem::replace(cursor, to) != to
}

pub fn delete_word_back(text: &mut String, cursor: &mut usize) -> bool {
    clamp(text, cursor);
    let to = word_left(text, *cursor);
    if to == *cursor {
        return false;
    }
    let (a, b) = (byte_at(text, to), byte_at(text, *cursor));
    text.replace_range(a..b, "");
    *cursor = to;
    true
}

pub fn kill_to_start(text: &mut String, cursor: &mut usize) -> bool {
    clamp(text, cursor);
    if *cursor == 0 {
        return false;
    }
    let b = byte_at(text, *cursor);
    text.replace_range(..b, "");
    *cursor = 0;
    true
}

pub fn kill_to_end(text: &mut String, cursor: &usize) -> bool {
    let a = byte_at(text, *cursor);
    if a == text.len() {
        return false;
    }
    text.truncate(a);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `text` with the caret written as `|`, so a case reads as what it looks
    /// like on screen rather than as a string and a number.
    fn at(spec: &str) -> (String, usize) {
        let cursor = spec.chars().take_while(|c| *c != '|').count();
        (spec.replace('|', ""), cursor)
    }

    fn show(text: &str, cursor: usize) -> String {
        let mut out: String = text.chars().take(cursor).collect();
        out.push('|');
        out.extend(text.chars().skip(cursor));
        out
    }

    /// Apply `op` to the state `before` describes and check both the text and
    /// where the caret ended up, plus whether anything was reported as changed.
    fn check(before: &str, op: impl FnOnce(&mut String, &mut usize) -> bool, after: &str) {
        let (mut text, mut cursor) = at(before);
        let changed = op(&mut text, &mut cursor);
        assert_eq!(show(&text, cursor), after, "from {before:?}");
        assert_eq!(
            changed,
            before != after,
            "{before:?} -> {after:?} reported changed={changed}, which would {} a repaint",
            if changed { "cost" } else { "skip" }
        );
    }

    #[test]
    fn typing_lands_at_the_caret_and_moves_it_along() {
        check("ab|cd", |t, k| insert(t, k, 'X'), "abX|cd");
        check("|", |t, k| insert(t, k, 'X'), "X|");
        // Multi-byte, because the caret counts characters and must not land
        // inside one.
        check("é|é", |t, k| insert(t, k, 'ß'), "éß|é");
    }

    #[test]
    fn a_paste_arrives_whole_with_its_line_endings_normalised() {
        check("a|b", |t, k| insert_str(t, k, "XY"), "aXY|b");
        check("|", |t, k| insert_str(t, k, "x\r\ny\rz"), "x\ny\nz|");
        // Nothing pasted is nothing changed.
        check("a|b", |t, k| insert_str(t, k, ""), "a|b");
    }

    #[test]
    fn backspace_and_delete_take_from_either_side_of_the_caret() {
        check("ab|cd", backspace, "a|cd");
        check("ab|cd", delete_forward, "ab|d");
        // At each end the opposite key has nothing to take.
        check("|abc", backspace, "|abc");
        check("abc|", delete_forward, "abc|");
        check("é|é", backspace, "|é");
    }

    #[test]
    fn word_motion_skips_the_spaces_then_the_word() {
        // The readline convention, which is why a run of spaces does not cost
        // a separate keypress to cross.
        check(
            "one two| three",
            |t, k| move_left(t, k, true),
            "one |two three",
        );
        check("one   |two", |t, k| move_left(t, k, true), "|one   two");
        check(
            "one| two three",
            |t, k| move_right(t, k, true),
            "one two| three",
        );
        check("|one   two", |t, k| move_right(t, k, true), "one|   two");
        // And the ends hold.
        check("|one", |t, k| move_left(t, k, true), "|one");
        check("one|", |t, k| move_right(t, k, true), "one|");
    }

    #[test]
    fn character_motion_stops_at_both_ends() {
        check("a|b", |t, k| move_left(t, k, false), "|ab");
        check("a|b", |t, k| move_right(t, k, false), "ab|");
        check("|ab", |t, k| move_left(t, k, false), "|ab");
        check("ab|", |t, k| move_right(t, k, false), "ab|");
    }

    #[test]
    fn home_and_end_go_to_the_ends_and_report_nothing_when_already_there() {
        check("ab|c", |_, k| home(k), "|abc");
        check("|abc", |_, k| home(k), "|abc");
        check("a|bc", |t, k| end(t, k), "abc|");
        check("abc|", |t, k| end(t, k), "abc|");
    }

    #[test]
    fn the_kills_cut_from_the_caret_rather_than_the_ends() {
        check("one two|", delete_word_back, "one |");
        check("one two |", delete_word_back, "one |");
        check("|one", delete_word_back, "|one");

        check("abc|def", kill_to_start, "|def");
        check("|abc", kill_to_start, "|abc");

        check("abc|def", |t, k| kill_to_end(t, k), "abc|");
        check("abc|", |t, k| kill_to_end(t, k), "abc|");
    }

    #[test]
    fn a_caret_past_the_end_is_treated_as_the_end_rather_than_panicking() {
        // Nothing should produce one, but every operation indexes with it, so
        // an out-of-range caret must clamp rather than take the process down.
        let mut text = "abc".to_owned();
        let mut cursor = 99;
        backspace(&mut text, &mut cursor);
        assert_eq!(text, "ab");
        let mut cursor = 99;
        assert!(!delete_forward(&mut text, &mut cursor));
        let mut cursor = 99;
        move_left("abc", &mut cursor, true);
        assert_eq!(cursor, 0);
    }

    #[test]
    fn length_is_counted_in_characters_not_bytes() {
        // The caret is a character index, so this is what every clamp uses. In
        // bytes "ééé" is six, and a caret allowed to reach six would sit
        // outside the string.
        assert_eq!(len_chars("ééé"), 3);
        assert_eq!(len_chars(""), 0);
    }
}
