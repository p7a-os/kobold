//! Streaming-tolerant markdown rendering for the transcript.
//!
//! Deltas arrive mid-token, so this must never choke on half-written markup:
//! an unclosed `**` or an open fence renders as ordinary text and fixes itself
//! on the next delta. There is no AST and no lookahead past the current line.
//!
//! Styling stays deliberately quiet -- dim for structure, one accent for code,
//! modifiers for emphasis -- so the prose stays the loudest thing on screen.

use ratatui::buffer::CellWidth;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;

type Seg = (String, Style);

/// Display width in terminal cells, measured per grapheme cluster so it agrees
/// exactly with what the painter will consume.
///
/// Counting `char`s instead is wrong for anything outside ASCII: a CJK ideograph
/// is one char and two cells, and an emoji with a modifier is several chars and
/// two cells. Wrapping on the char count therefore breaks a line early and then
/// still overflows the margin, and the painter clips the tail -- text the reader
/// never sees, with nothing on screen to say it was there.
pub fn width_of(text: &str) -> usize {
    text.graphemes(true).map(|g| g.cell_width() as usize).sum()
}

/// Byte offset at which `text` reaches `width` cells, never splitting a
/// grapheme cluster and never returning a prefix wider than asked for.
///
/// Always advances by at least one cluster, so a single grapheme too wide for
/// the whole line spills rather than wedging the caller in a loop that splits
/// nothing off.
pub fn split_at_width(text: &str, width: usize) -> usize {
    let mut used = 0usize;
    for (i, g) in text.grapheme_indices(true) {
        let w = g.cell_width() as usize;
        if used + w > width {
            return if i == 0 {
                text.grapheme_indices(true)
                    .nth(1)
                    .map_or(text.len(), |(n, _)| n)
            } else {
                i
            };
        }
        used += w;
    }
    text.len()
}

fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}

fn code() -> Style {
    Style::default().fg(Color::Cyan)
}

/// Fenced blocks read as a tinted slab rather than a ruled-off region.
/// Terminals have no opacity, so "slightly lighter" is a fixed 256-colour step
/// off black. That assumes a dark terminal; `code_bg` is the caller's resolved
/// override -- `settings::Settings::code_bg`, which already folds
/// `KOBOLD_CODE_BG` into the file setting, so this stays the one path rather
/// than a second environment read here racing the settings one.
fn code_block(code_bg: u8) -> Style {
    Style::default().bg(Color::Indexed(code_bg))
}

/// Where a later append can pick rendering back up.
///
/// Streaming means the same entry is rendered once per delta, and re-parsing
/// all of it every time makes one reply cost O(n^2) in its own length. Only the
/// block still being written can change: a line that has been terminated by a
/// newline, with no fence or table left open across it, will render identically
/// forever. So the renderer reports the first offset it has *not* frozen, and
/// the next append starts there.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Resume {
    /// Byte offset into the source of the first line not yet frozen.
    pub at: usize,
    /// How many lines were emitted before `at`.
    pub lines: usize,
    /// Whether the last frozen line has any content. A fence or a table opens
    /// with a blank line above it unless one is already there, and after a
    /// resume the lines above are not in hand to look at.
    pub trailing_text: bool,
    /// Set when the frozen prefix ends inside a fence that has not closed yet.
    /// A long fence is the worst case there is -- "write me that file" streams
    /// hundreds of lines into one block -- so its completed lines are frozen
    /// like any others, which means carrying enough state to tokenise the rest.
    open_fence: Option<OpenFence>,
}

#[derive(Clone, Debug, PartialEq)]
struct OpenFence {
    /// The fence's language hint, needed to keep tokenising the body.
    hint: String,
    /// What the last frozen line of the body left open.
    carry: crate::syntax::Carry,
}

/// Render `text` into exactly `width`-wide lines. Exact, because the caller
/// scrolls by line count and any estimate desynchronises the viewport.
pub fn render(text: &str, width: usize, code_bg: u8) -> Vec<Line<'static>> {
    render_from(text, width, Resume::default(), code_bg).0
}

/// Render `text[from.at..]`, and report where the *next* append can resume.
///
/// The returned lines replace everything from `from.lines` on. A `from` of
/// `Resume::default()` renders the whole thing, which is what `render` is.
pub fn render_from(
    text: &str,
    width: usize,
    from: Resume,
    code_bg: u8,
) -> (Vec<Line<'static>>, Resume) {
    let width = width.max(8);
    // A resume point past the end means the caller's cache does not describe
    // this text. Falling back to a full render is always correct -- and
    // therefore invisible, which is why it is counted: the output is right
    // either way, so nothing else would ever report that the fast path was
    // silently abandoned.
    let stale = from.at > text.len();
    if stale {
        crate::layout::md_stale();
    }
    let from = if stale { Resume::default() } else { from };
    if from.at == 0 {
        crate::layout::md_full();
    } else {
        crate::layout::md_resumed();
    }
    let (start, base_lines, seed) = (from.at, from.lines, from.trailing_text);
    let mut out = Vec::new();
    // A fence emits its rows as they arrive rather than buffering the body, so
    // a completed line inside one can be frozen like any other. What crosses a
    // line -- the hint, and any block comment or string still open -- is what
    // gets carried instead.
    let mut fence: Option<(String, crate::syntax::Carry)> =
        from.open_fence.clone().map(|f| (f.hint, f.carry));
    // Tables are still buffered: column widths cannot be known until the last
    // row has arrived, so no part of one is final until it ends.
    let mut table: Vec<String> = Vec::new();
    // Freezing nothing new is normal -- a delta that does not complete a line
    // leaves the resume point exactly where it was. It has to carry the whole
    // incoming state when that happens, open fence included: rebuilding it from
    // the defaults loses the fence and renders the rest of the block as prose.
    let mut resume = from;
    let mut offset = start;

    for raw in text[start..].split('\n') {
        // Start of the line after this one. Past the end of the text when this
        // is the final line, which is exactly the line that may still grow.
        let next = offset + raw.len() + 1;

        'line: {
            let trimmed = raw.trim_start();

            if let Some(hint) = trimmed.strip_prefix("```") {
                match fence.take() {
                    Some(_) => slab_close(&mut out, width, code_bg),
                    None => {
                        slab_open(&mut out, width, seed, code_bg);
                        fence = Some((hint.to_owned(), crate::syntax::Carry::Code));
                    }
                }
                break 'line;
            }

            if let Some((hint, carry)) = fence.as_mut() {
                *carry = slab_line(&mut out, hint, raw, *carry, width, code_bg);
                break 'line;
            }

            if is_row(trimmed) {
                table.push(trimmed.to_owned());
                break 'line;
            }
            if !table.is_empty() {
                emit_table(&mut out, &std::mem::take(&mut table), width, seed);
            }

            let (prefix, body, base) = block(trimmed);
            let indent = " ".repeat(raw.len() - trimmed.len());
            let head: Seg = (format!("{indent}{prefix}"), dim());
            let segs = inline(body, base);

            let avail = width.saturating_sub(head.0.chars().count()).max(4);
            for (i, wrapped) in wrap(segs, avail).into_iter().enumerate() {
                let mut spans = vec![Span::styled(
                    if i == 0 {
                        head.0.clone()
                    } else {
                        " ".repeat(head.0.chars().count())
                    },
                    head.1,
                )];
                spans.extend(wrapped.into_iter().map(|(t, s)| Span::styled(t, s)));
                out.push(Line::from(spans));
            }
        }

        // Everything up to `next` is settled when this line was terminated by a
        // newline and no table is mid-flight. The final line is never frozen:
        // another delta appends to it rather than starting a new one.
        if next <= text.len() && table.is_empty() {
            resume = Resume {
                at: next,
                lines: base_lines + out.len(),
                trailing_text: needs_gap(&out, seed),
                open_fence: fence.as_ref().map(|(hint, carry)| OpenFence {
                    hint: hint.clone(),
                    carry: *carry,
                }),
            };
        }
        offset = next;
    }
    // A table still arriving renders with the rows it has; widths settle as
    // more arrive.
    if !table.is_empty() {
        emit_table(&mut out, &table, width, seed);
    }
    // An unclosed fence is a block still being streamed: close the slab so it
    // reads as one, and reopen it on the next delta.
    if fence.is_some() {
        slab_close(&mut out, width, code_bg);
    }
    (out, resume)
}

/// Whether a block starting here wants a blank line above it. `seed` is the
/// answer when nothing has been emitted yet, which happens only at the top of a
/// resumed render, where the line above belongs to the frozen prefix.
fn needs_gap(out: &[Line<'static>], seed: bool) -> bool {
    out.last().map_or(seed, |l| l.width() != 0)
}

const PAD: usize = 2;

/// The tinted blank row that opens and closes a slab.
fn slab_blank(width: usize, code_bg: u8) -> Line<'static> {
    Line::from(Span::styled(" ".repeat(width), code_block(code_bg)))
}

/// Open a fenced block: the gap above it, then its first tinted blank row.
fn slab_open(out: &mut Vec<Line<'static>>, width: usize, seed: bool, code_bg: u8) {
    if needs_gap(out, seed) {
        out.push(Line::default());
    }
    out.push(slab_blank(width, code_bg));
}

/// One source line of a fenced block, wrapped to as many tinted rows as it
/// needs. Tokenising takes the state the line above left open and reports its
/// own, so a fence still being streamed only ever lexes its newest line.
fn slab_line(
    out: &mut Vec<Line<'static>>,
    hint: &str,
    line: &str,
    carry: crate::syntax::Carry,
    width: usize,
    code_bg: u8,
) -> crate::syntax::Carry {
    let bg = code_block(code_bg);
    let inner = width.saturating_sub(PAD * 2).max(4);
    let (tokens, next) = crate::syntax::highlight_line(hint, line, carry);
    for row in fit(tokens, inner) {
        let used: usize = row.iter().map(|(t, _)| width_of(t)).sum();
        let mut spans = vec![Span::styled(" ".repeat(PAD), bg)];
        spans.extend(
            row.into_iter()
                .map(|(t, tok)| Span::styled(t, bg.fg(tok.color()))),
        );
        spans.push(Span::styled(" ".repeat(width - PAD - used.min(inner)), bg));
        out.push(Line::from(spans));
    }
    next
}

/// Close a fenced block: its last tinted row, and the gap below it.
fn slab_close(out: &mut Vec<Line<'static>>, width: usize, code_bg: u8) {
    out.push(slab_blank(width, code_bg));
    out.push(Line::default());
}

/// Lay tokens into rows of at most `width`. Code is hard-split, never word
/// wrapped: breaking an identifier across a space would misrepresent it.
fn fit(
    tokens: Vec<(String, crate::syntax::Tok)>,
    width: usize,
) -> Vec<Vec<(String, crate::syntax::Tok)>> {
    let mut rows = vec![Vec::new()];
    let mut used = 0usize;
    for (text, tok) in tokens {
        let mut text = text.as_str();
        while used + width_of(text) > width {
            // Room left on this row, or a fresh row when it is already full.
            let room = width.saturating_sub(used);
            let at = if room == 0 {
                0
            } else {
                split_at_width(text, room)
            };
            if at > 0 {
                rows.last_mut()
                    .expect("non-empty")
                    .push((text[..at].to_owned(), tok));
                text = &text[at..];
            }
            rows.push(Vec::new());
            used = 0;
        }
        if !text.is_empty() {
            used += width_of(text);
            rows.last_mut()
                .expect("non-empty")
                .push((text.to_owned(), tok));
        }
    }
    rows
}

#[derive(Clone, Copy, PartialEq)]
enum Align {
    Left,
    Center,
    Right,
}

/// A pipe-delimited row. Kept loose so a table still being streamed is
/// recognised from its first line rather than only once it is complete.
fn is_row(line: &str) -> bool {
    line.starts_with('|') && line[1..].contains('|')
}

fn cells(line: &str) -> Vec<String> {
    let t = line.trim().trim_start_matches('|').trim_end_matches('|');
    t.split('|').map(|c| c.trim().to_owned()).collect()
}

fn is_sep(row: &[String]) -> bool {
    !row.is_empty()
        && row.iter().all(|c| {
            let c = c.trim();
            c.len() >= 3 && c.trim_matches(':').chars().all(|ch| ch == '-')
        })
}

fn align_of(spec: &str) -> Align {
    let s = spec.trim();
    match (s.starts_with(':'), s.ends_with(':')) {
        (true, true) => Align::Center,
        (false, true) => Align::Right,
        _ => Align::Left,
    }
}

/// Rendered without a box: a bold header, one dim rule, and space-separated
/// columns. Rules on every edge would fight the borderless look of everything
/// else on screen.
fn emit_table(out: &mut Vec<Line<'static>>, raw: &[String], width: usize, seed: bool) {
    const GAP: usize = 2;
    let dimmed = dim();

    let mut rows: Vec<Vec<String>> = raw.iter().map(|r| cells(r)).collect();
    let (header, aligns, body) = if rows.len() >= 2 && is_sep(&rows[1]) {
        let head = rows.remove(0);
        let spec = rows.remove(0);
        (
            Some(head),
            spec.iter().map(|s| align_of(s)).collect::<Vec<_>>(),
            rows,
        )
    } else {
        (None, Vec::new(), rows)
    };

    let ncols = header
        .iter()
        .map(|h| h.len())
        .chain(body.iter().map(|r| r.len()))
        .max()
        .unwrap_or(0);
    if ncols == 0 {
        return;
    }
    let align = |i: usize| aligns.get(i).copied().unwrap_or(Align::Left);

    // Natural widths, then shave the widest column until the row fits. Shaving
    // the widest keeps narrow columns intact instead of squeezing everything.
    let mut w: Vec<usize> = (0..ncols)
        .map(|i| {
            header
                .iter()
                .chain(body.iter())
                .filter_map(|r| r.get(i))
                .map(|c| visible_len(c))
                .max()
                .unwrap_or(1)
                .max(1)
        })
        .collect();
    let gaps = GAP * ncols.saturating_sub(1);
    while w.iter().sum::<usize>() + gaps > width {
        let Some(widest) = (0..ncols).max_by_key(|&i| w[i]) else {
            break;
        };
        if w[widest] <= 4 {
            break;
        }
        w[widest] -= 1;
    }
    let total = (w.iter().sum::<usize>() + gaps).min(width);

    if needs_gap(out, seed) {
        out.push(Line::default());
    }

    let emit_row = |out: &mut Vec<Line<'static>>, row: &[String], bold: bool| {
        let base = if bold {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        // A cell that had to be narrowed wraps rather than truncating: losing
        // the tail of a cell would silently lose data.
        let wrapped: Vec<Vec<Vec<Seg>>> = (0..ncols)
            .map(|i| {
                let text = row.get(i).map(String::as_str).unwrap_or("");
                wrap(inline(text, base), w[i])
            })
            .collect();
        let height = wrapped.iter().map(|c| c.len()).max().unwrap_or(1);

        for line_idx in 0..height {
            let mut spans: Vec<Span<'static>> = Vec::new();
            for col in 0..ncols {
                if col > 0 {
                    spans.push(Span::raw(" ".repeat(GAP)));
                }
                let empty = Vec::new();
                let segs = wrapped[col].get(line_idx).unwrap_or(&empty);
                let used: usize = segs.iter().map(|(t, _)| width_of(t)).sum();
                let slack = w[col].saturating_sub(used);
                let (before, after) = match align(col) {
                    Align::Left => (0, slack),
                    Align::Right => (slack, 0),
                    Align::Center => (slack / 2, slack - slack / 2),
                };
                if before > 0 {
                    spans.push(Span::raw(" ".repeat(before)));
                }
                spans.extend(segs.iter().map(|(t, st)| Span::styled(t.clone(), *st)));
                if after > 0 {
                    spans.push(Span::raw(" ".repeat(after)));
                }
            }
            out.push(Line::from(spans));
        }
    };

    if let Some(head) = &header {
        emit_row(out, head, true);
        out.push(Line::from(Span::styled("─".repeat(total), dimmed)));
    }
    // A rule between every body row, not just under the header: once a cell
    // wraps onto a second line there is otherwise no way to see where one row
    // ends and the next begins. Dimmer than the header rule so the hierarchy
    // still reads.
    let between = Style::default().fg(Color::Indexed(237));
    for (i, row) in body.iter().enumerate() {
        if i > 0 {
            out.push(Line::from(Span::styled("─".repeat(total), between)));
        }
        emit_row(out, row, false);
    }
    out.push(Line::default());
}

/// Character count with inline markers removed, so `**Name**` measures as
/// `Name` and columns do not end up padded for syntax the reader never sees.
fn visible_len(cell: &str) -> usize {
    inline(cell, Style::default())
        .iter()
        .map(|(t, _)| width_of(t))
        .sum()
}

/// Classify a line: returns the gutter marker, the remaining text, and the
/// base style the text inherits.
fn block(line: &str) -> (String, &str, Style) {
    if let Some(rest) = line.strip_prefix("### ") {
        return (
            String::new(),
            rest,
            Style::default().add_modifier(Modifier::BOLD),
        );
    }
    if let Some(rest) = line.strip_prefix("## ") {
        return (
            String::new(),
            rest,
            Style::default().add_modifier(Modifier::BOLD),
        );
    }
    if let Some(rest) = line.strip_prefix("# ") {
        return (
            String::new(),
            rest,
            Style::default().add_modifier(Modifier::BOLD),
        );
    }
    if let Some(rest) = line.strip_prefix("> ") {
        return ("│ ".to_owned(), rest, dim());
    }
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(marker) {
            return ("• ".to_owned(), rest, Style::default());
        }
    }
    // Ordered list: keep the author's number, it carries meaning.
    if let Some(dot) = line.find(". ") {
        if dot > 0 && dot <= 3 && line[..dot].chars().all(|c| c.is_ascii_digit()) {
            return (
                format!("{}. ", &line[..dot]),
                &line[dot + 2..],
                Style::default(),
            );
        }
    }
    (String::new(), line, Style::default())
}

/// `**bold**`, `*italic*`, `` `code` ``, `[text](url)`. Unclosed markers are
/// emitted literally, which is what makes mid-stream text readable.
fn inline(text: &str, base: Style) -> Vec<Seg> {
    let mut out: Vec<Seg> = Vec::new();
    let b = text.as_bytes();
    let mut i = 0;
    let mut plain = String::new();

    let flush = |plain: &mut String, out: &mut Vec<Seg>| {
        if !plain.is_empty() {
            out.push((std::mem::take(plain), base));
        }
    };

    while i < b.len() {
        let rest = &text[i..];

        if let Some(inner) = closed(rest, "**") {
            flush(&mut plain, &mut out);
            out.push((inner.to_owned(), base.add_modifier(Modifier::BOLD)));
            i += inner.len() + 4;
            continue;
        }
        if let Some(inner) = closed(rest, "`") {
            flush(&mut plain, &mut out);
            out.push((inner.to_owned(), code()));
            i += inner.len() + 2;
            continue;
        }
        if rest.starts_with('*') || rest.starts_with('_') {
            let marker = &rest[..1];
            if let Some(inner) = closed(rest, marker) {
                if !inner.is_empty() {
                    flush(&mut plain, &mut out);
                    out.push((inner.to_owned(), base.add_modifier(Modifier::ITALIC)));
                    i += inner.len() + 2;
                    continue;
                }
            }
        }
        if rest.starts_with('[') {
            if let Some((label, url, len)) = link(rest) {
                flush(&mut plain, &mut out);
                out.push((label.to_owned(), base.add_modifier(Modifier::UNDERLINED)));
                // The URL is kept, dimmed: dropping it would hide information.
                out.push((format!(" ({url})"), dim()));
                i += len;
                continue;
            }
        }

        let ch = rest.chars().next().expect("non-empty");
        plain.push(ch);
        i += ch.len_utf8();
    }
    flush(&mut plain, &mut out);
    if out.is_empty() {
        out.push((String::new(), base));
    }
    out
}

/// Text between a leading `marker` and its next occurrence, if it closes on
/// this line. `None` means "still being typed" -- treat it as plain text.
fn closed<'a>(rest: &'a str, marker: &str) -> Option<&'a str> {
    let after = rest.strip_prefix(marker)?;
    let end = after.find(marker)?;
    Some(&after[..end])
}

fn link(rest: &str) -> Option<(&str, &str, usize)> {
    let close = rest.find("](")?;
    let end = rest[close..].find(')')? + close;
    Some((&rest[1..close], &rest[close + 2..end], end + 1))
}

/// Greedy wrap that carries each word's style with it.
fn wrap(segs: Vec<Seg>, width: usize) -> Vec<Vec<Seg>> {
    let mut lines: Vec<Vec<Seg>> = Vec::new();
    let mut cur: Vec<Seg> = Vec::new();
    let mut used = 0usize;

    for (text, style) in segs {
        for (wi, word) in text.split(' ').enumerate() {
            if word.is_empty() {
                if wi > 0 && used > 0 && used < width {
                    push(&mut cur, " ", style);
                    used += 1;
                }
                continue;
            }
            let mut word = word;
            // A word wider than the line is split rather than allowed to
            // overflow, so a long path or URL cannot break the layout.
            while width_of(word) > width {
                if used > 0 {
                    lines.push(std::mem::take(&mut cur));
                    used = 0;
                }
                let at = split_at_width(word, width);
                lines.push(vec![(word[..at].to_owned(), style)]);
                word = &word[at..];
            }
            let sep = usize::from(wi > 0 && used > 0);
            if used + sep + width_of(word) > width {
                lines.push(std::mem::take(&mut cur));
                used = 0;
            } else if sep == 1 {
                push(&mut cur, " ", style);
                used += 1;
            }
            push(&mut cur, word, style);
            used += width_of(word);
        }
    }
    lines.push(cur);
    lines
}

/// Append to the previous segment when the style matches, so a wrapped line is
/// a handful of spans rather than one per word.
fn push(cur: &mut Vec<Seg>, text: &str, style: Style) {
    match cur.last_mut() {
        Some((prev, s)) if *s == style => prev.push_str(text),
        _ => cur.push((text.to_owned(), style)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every construct whose rendering depends on more than the current line:
    /// a fence, a table, and the blank-line handling between blocks. Prose
    /// alone would not exercise the resume point at all.
    const DOC: &str = "\
Congestion control decides **how fast** to send, and this opening paragraph is \
long enough that it has to wrap at any sensible terminal width.

## The window

- Slow start doubles `cwnd` every round trip
- Congestion avoidance adds one segment
- A loss halves it

| phase | growth | trigger |
|---|---|---|
| slow start | exponential | open |
| avoidance | linear | past ssthresh |

Then some prose after the table, to check the gap handling on the way out.

```rust
fn grow(cwnd: usize) -> usize {
    // a comment, a \"string\", and a number 42
    cwnd + 1
}
```

A closing line.
";

    /// Feed `text` in `step`-byte deltas the way the transcript does, checking
    /// after every one that the incrementally-built lines are exactly what a
    /// full render of the same prefix produces.
    ///
    /// This is the property the whole optimisation rests on: if it holds, the
    /// cache is invisible, and if it does not, the screen shows something that
    /// never existed in the source.
    fn check_streaming(text: &str, width: usize, step: usize) {
        let mut lines: Vec<Line<'static>> = Vec::new();
        let mut resume = Resume::default();
        let mut prev_at = 0usize;
        let mut at = 0usize;

        while at < text.len() {
            at = (at + step).min(text.len());
            while !text.is_char_boundary(at) {
                at += 1;
            }
            let prefix = &text[..at];

            assert!(
                resume.at <= prefix.len(),
                "resume ran past the text at {at}: {resume:?}"
            );
            assert!(
                resume.at >= prev_at,
                "resume went backwards at {at}: {resume:?}"
            );
            prev_at = resume.at;
            assert!(
                resume.lines <= lines.len(),
                "resume claims {} frozen lines, only {} exist",
                resume.lines,
                lines.len()
            );

            lines.truncate(resume.lines);
            let (tail, next) = render_from(prefix, width, resume, 235);
            lines.extend(tail);
            resume = next;

            let whole = render(prefix, width, 235);
            assert_eq!(
                lines.len(),
                whole.len(),
                "line count diverged at byte {at} (step {step}, width {width})\n\
                 incremental:\n{}\nwhole:\n{}",
                dump(&lines),
                dump(&whole)
            );
            for (i, (got, want)) in lines.iter().zip(whole.iter()).enumerate() {
                assert_eq!(
                    got, want,
                    "line {i} diverged at byte {at} (step {step}, width {width})\n\
                     incremental: {:?}\nwhole:       {:?}",
                    got, want
                );
            }
        }
    }

    fn dump(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .enumerate()
            .map(|(i, l)| format!("  {i:>3}: {:?}", l.to_string()))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_resumed_render_matches_a_whole_one_at_every_prefix() {
        // Several step sizes, because a delta boundary landing mid-fence, on a
        // newline, or mid-table row are all different resume states. One byte
        // at a time is the worst case and the one streaming actually produces.
        for step in [1, 3, 17, 64] {
            for width in [40, 72] {
                check_streaming(DOC, width, step);
            }
        }
    }

    #[test]
    fn a_table_is_never_frozen_into_but_a_fence_is() {
        // A table's column widths depend on every row it has, so none of one is
        // final until it ends.
        let table = "intro\n\n| a | b |\n|---|---|\n| c | d |\n";
        let (_, resume) = render_from(table, 60, Resume::default(), 235);
        assert!(
            !table[..resume.at].contains('|'),
            "froze into an open table: {:?}",
            &table[..resume.at]
        );

        // A fence is the opposite, and deliberately so: its lines are
        // independent once the lexer state is carried across them, which is the
        // whole reason a long code block stopped costing O(n^2).
        let fence = "intro\n\n```rust\nfn a() {}\nfn b() {}\n";
        let (_, resume) = render_from(fence, 60, Resume::default(), 235);
        assert!(
            resume.at > fence.find("fn a").expect("body line"),
            "a fence must freeze the body lines it has finished, froze only {:?}",
            &fence[..resume.at]
        );
        assert_eq!(
            resume.open_fence.as_ref().map(|f| f.hint.as_str()),
            Some("rust"),
            "a resume inside a fence must carry the hint, or the tail loses its colours"
        );
    }

    #[test]
    fn the_caller_s_code_bg_reaches_the_rendered_slab() {
        // The whole point of threading `code_bg` down from
        // `settings::Settings` instead of reading `KOBOLD_CODE_BG` here
        // directly: this is the assertion whose absence let the settings-file
        // key do nothing while only the environment variable worked.
        let text = "```\nfn a() {}\n```\n";
        for bg in [1u8, 99, 235] {
            let lines = render(text, 40, bg);
            let found = lines
                .iter()
                .flat_map(|l| l.spans.iter())
                .any(|s| s.style.bg == Some(Color::Indexed(bg)));
            assert!(
                found,
                "no span in the rendered fence carried background {bg}"
            );
        }
    }

    #[test]
    fn a_fenced_block_s_open_content_and_close_rows_each_carry_the_bg() {
        // The test above only checks that the colour turns up *somewhere*,
        // which a stub for any one of `slab_open`/`slab_line`/`slab_close`
        // can still satisfy through whichever of the other two still runs --
        // `slab_blank` backs both the open and the close row, so gutting it
        // would still leave the content row's own tint behind. This pins the
        // shape instead: which row is which, and that each is the thing it
        // claims to be rather than an empty stand-in for it.
        let bg = 99u8;
        let lines = render("```\nfn a() {}\n```\n", 40, bg);
        assert!(
            lines.len() >= 4,
            "expected an open row, a content row, a close row and the gap after it; got {} lines",
            lines.len()
        );

        let blank_tinted_row = |l: &Line<'static>| -> bool {
            l.spans.len() == 1
                && l.spans[0].content.chars().all(|c| c == ' ')
                && l.spans[0].content.chars().count() == 40
                && l.spans[0].style.bg == Some(Color::Indexed(bg))
        };
        assert!(
            blank_tinted_row(&lines[0]),
            "row 0 should be slab_open's full-width tinted row"
        );
        assert!(
            lines[1]
                .spans
                .iter()
                .any(|s| s.content.contains("fn a()") && s.style.bg == Some(Color::Indexed(bg))),
            "row 1 should be slab_line's content, tinted"
        );
        assert!(
            blank_tinted_row(&lines[2]),
            "row 2 should be slab_close's full-width tinted row"
        );
        assert!(
            lines[3].spans.is_empty(),
            "row 3 should be slab_close's blank gap row"
        );
    }

    #[test]
    fn lexer_state_survives_a_freeze() {
        // A block comment, a multi-line string and an unterminated one all run
        // past a line ending, so each is a chance for the carried state to be
        // dropped and the rest of the block to revert to code colouring.
        for text in [
            "```rust\n/* one\ntwo\nthree */\ndone\n```\n",
            "```python\nx = \"\"\"one\ntwo\n\"\"\"\ny = 1\n```\n",
            "```rust\nlet s = \"never closed\nnext line\nmore\n```\n",
        ] {
            check_streaming(text, 40, 1);
        }
    }

    #[test]
    fn the_last_line_is_never_frozen() {
        // It has no terminating newline, so the next delta appends to it rather
        // than starting a new one. Freezing it would strand the rest of the word.
        for text in ["hello", "one\ntwo", "a\n\nb\n\nc"] {
            let (_, resume) = render_from(text, 60, Resume::default(), 235);
            let tail_start = text.rfind('\n').map_or(0, |i| i + 1);
            assert!(
                resume.at <= tail_start,
                "froze the final line of {text:?}: resume {} > {tail_start}",
                resume.at
            );
        }
    }

    #[test]
    fn a_stale_resume_point_falls_back_instead_of_panicking() {
        // The cache key should prevent this, but an out-of-range resume must
        // degrade to a full render rather than slicing outside the string.
        let bogus = Resume {
            at: 9_999,
            lines: 40,
            trailing_text: true,
            ..Resume::default()
        };
        let (lines, resume) = render_from("short text\n", 60, bogus, 235);
        assert_eq!(lines, render("short text\n", 60, 235));
        assert_eq!(
            resume.lines, 1,
            "resumed line count should restart from zero"
        );
    }

    #[test]
    fn a_resume_point_exactly_at_the_end_is_resumed_from_rather_than_discarded() {
        // The boundary of the staleness check. `at == len` describes the text
        // exactly -- everything in it is frozen -- so it must resume and emit
        // the empty tail, not decide the cache is wrong and lay the whole
        // entry out again. Off by one here turns the cheapest possible render
        // into the most expensive one, silently and with correct output.
        let text = "one\ntwo\n";
        let at_end = Resume {
            at: text.len(),
            lines: 2,
            trailing_text: true,
            ..Resume::default()
        };
        let (tail, _) = render_from(text, 60, at_end, 235);
        let whole = render(text, 60, 235);
        assert!(
            tail.len() < whole.len(),
            "resuming at the end should emit the tail alone, got {} lines against {} for a \
             whole render",
            tail.len(),
            whole.len()
        );
    }
}
