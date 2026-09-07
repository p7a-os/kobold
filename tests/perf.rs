//! The performance gate: deterministic counters, asserted.
//!
//! `bench.rs` reports percentiles and growth ratios, which are properties of
//! the box as much as of the code -- useful for exploration, useless as a
//! gate. Everything here is a count. Same input, same numbers, on a loaded
//! machine or an idle one.
//!
//! What this exists to catch, stated concretely because it is not obvious:
//! deleting the transcript memoisation outright leaves every `term` counter
//! byte-for-byte identical -- same bytes, same cells, same elision, same
//! scrolls -- while turning each keystroke into O(session) work. Measured,
//! not hypothesised: streaming one reply goes from 1 full layout to 1248.
//! The `layout::` counters are the only instrument that sees it.
//!
//! What this does NOT catch, recorded so nobody mistakes a green run for
//! more than it is: a pure-CPU regression *inside* one layout pass. If
//! `layout_entry` became twice as slow producing identical output, `full`
//! stays 1 and nothing here moves. These count how often layout runs, not
//! what one run costs. UNKNOWN whether a realistic change has that shape --
//! no one has constructed one, and no one has ruled it out.
//!
//! One `#[test]`, all scenarios sequential: the counters are process-global
//! with no reset (deliberately -- a reset is a second way to get a delta
//! wrong), so parallel scenarios would corrupt each other's arithmetic.

use std::cell::Cell as StdCell;
use std::io::{self, Write};
use std::rc::Rc;

use kobold::app::{App, Who};
use kobold::fixture::reply;
use kobold::term::Screen;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;

/// A terminal that measures instead of displaying. Needed because `BYTES` is
/// only incremented inside `term::SyncOut::write`, and `Screen::with_backend`
/// does not wrap its writer in one -- so the counter alone would report zero.
#[derive(Clone, Default)]
struct Sink(Rc<StdCell<usize>>);

impl Write for Sink {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.0.set(self.0.get() + b.len());
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Both counter sets over one measured window, plus the bytes the sink saw.
struct Delta {
    hits: u64,
    resumed: u64,
    full: u64,
    md_full: u64,
    md_resumed: u64,
    md_stale: u64,
    slots: u64,
    passes: u64,
    frames: u64,
    bytes: usize,
    written: u64,
    onscreen: u64,
    scrolls: u64,
    repaints: u64,
    entries: u64,
}

impl Delta {
    fn elided(&self) -> f64 {
        1.0 - (self.written as f64 / self.onscreen.max(1) as f64)
    }
    fn bytes_per_frame(&self) -> usize {
        self.bytes / self.frames.max(1) as usize
    }
}

/// Snapshot, run, snapshot, subtract -- the same shape `bench.rs::wire`
/// already uses. The closure is handed the sink so it can draw through it.
fn measure(sink: &Sink, body: impl FnOnce()) -> Delta {
    let t0 = kobold::term::stats();
    let l0 = kobold::layout::stats();
    let b0 = sink.0.get();
    body();
    let t1 = kobold::term::stats();
    let l1 = kobold::layout::stats();
    Delta {
        hits: l1.hits - l0.hits,
        resumed: l1.resumed - l0.resumed,
        full: l1.full - l0.full,
        md_full: l1.md_full - l0.md_full,
        md_resumed: l1.md_resumed - l0.md_resumed,
        md_stale: l1.md_stale - l0.md_stale,
        slots: l1.slots - l0.slots,
        passes: l1.passes - l0.passes,
        frames: t1.frames - t0.frames,
        bytes: sink.0.get() - b0,
        written: t1.written - t0.written,
        onscreen: t1.onscreen - t0.onscreen,
        scrolls: t1.scrolls - t0.scrolls,
        repaints: t1.repaints - t0.repaints,
        entries: l1.entries - l0.entries,
    }
}

fn screen(sink: &Sink, area: Rect) -> Screen<CrosstermBackend<Sink>> {
    Screen::with_backend(CrosstermBackend::new(sink.clone()), area)
}

#[test]
fn the_render_path_stays_within_its_measured_cost() {
    // Cleared rather than inherited: every one of these changes what gets
    // emitted, so a developer's terminal would otherwise decide whether the
    // gate passes. `KOBOLD_CODE_BG` would move bytes, the rest gate escape
    // sequences.
    for key in [
        "KOBOLD_CODE_BG",
        "TERM",
        "TERM_PROGRAM",
        "TMUX",
        "VTE_VERSION",
    ] {
        std::env::remove_var(key);
    }
    // Hardcoded, not taken from the environment: the numbers below are only
    // meaningful at one geometry.
    let area = Rect::new(0, 0, 100, 30);
    let body = reply();

    stream(area, &body);
    scroll(area, &body);
    keystroke(area, &body);
    peak(area, &body);
    stale_resume();
}

/// Peak, for a TUI, is not concurrent users. It is every pane open at once
/// with a long session behind each.
///
/// **Eight panes is the documented ceiling**: panes split the body by
/// `Ratio(1, n)` and the renderer's own floor is `.max(8)` columns, so the
/// interface is unusable well before the pane count becomes interesting.
/// Measuring there measures the worst case anyone can actually reach.
///
/// The property is the same one the windowing exists for, asserted where it
/// is under the most pressure: **a keystroke costs what the viewport costs,
/// not what the session holds.** Eight panes multiply the visible rows, not
/// the transcript -- so per-frame work must scale with panes, and must not
/// scale with turns.
fn peak(area: Rect, body: &str) {
    let run = |panes: usize, turns: usize| -> Delta {
        let sink = Sink::default();
        let mut screen = screen(&sink, area);
        let mut app = App::new("main", "b0");
        for i in 0..turns {
            app.push(Who::User, format!("question {i}"));
            app.push(Who::Model, body.to_owned());
        }
        // `fork` clones the pane up to the selection, so each new pane
        // carries the same session rather than an empty one -- which is the
        // point: peak is every pane loaded, not one loaded and seven blank.
        for _ in 1..panes {
            app.pane_mut().selected = Some(app.pane().transcript.len() - 1);
            assert!(app.fork(), "fork must succeed to build the peak case");
        }
        assert_eq!(
            app.panes.len(),
            panes,
            "the fixture must actually open {panes} panes"
        );

        // Outside the window: this frame lays every pane out for the first
        // time, and it is exactly the cost the keystrokes after it must not
        // repeat.
        screen.draw(|buf, a| app.render(buf, a)).expect("draw");
        measure(&sink, || {
            for c in "hello".chars() {
                app.pane_mut().input.push(c);
                screen.draw(|buf, a| app.render(buf, a)).expect("draw");
            }
        })
    };

    let small = run(8, 4);
    let large = run(8, 80);

    // The assertion that matters, and the one a fixed-size scenario cannot
    // make: fifty times the session, and a keystroke must cost the same.
    assert_eq!(
        small.bytes_per_frame(),
        large.bytes_per_frame(),
        "a keystroke at peak cost {} bytes with 4 turns and {} with 80 -- \
         per-frame work is scaling with the session rather than the viewport",
        small.bytes_per_frame(),
        large.bytes_per_frame()
    );

    // And nothing is re-laid-out. At eight panes a single full layout per
    // keystroke would be eight, which is how this would fail quietly: still
    // bounded, still constant in turns, and eight times the work.
    assert_eq!(large.full, 0, "a keystroke re-laid out an entry at peak");
    assert_eq!(
        large.resumed, 0,
        "nothing is streaming; there is nothing to resume"
    );
    assert_eq!(
        large.repaints, 0,
        "a keystroke forced a full repaint at peak"
    );

    // Scrolls are suppressed above one pane -- a full-width band would drag
    // the neighbour with it -- so the hint must not fire here. Asserted
    // because its absence is what makes the byte counts above comparable.
    assert_eq!(
        large.scrolls, 0,
        "the scroll hint fired with more than one pane open"
    );

    // **The assertion that actually catches un-windowing**, and the one the
    // byte counts above cannot make. Rows placed per frame must be bounded by
    // the viewport, not by the transcript.
    //
    // Learned by checking rather than assuming: widening `drawn` past the
    // viewport leaves every byte count identical, because the extra rows fall
    // outside the buffer and are clipped. The work is done and thrown away --
    // invisible to bytes, cells, scrolls and repaints alike, which is exactly
    // how the 1,293x placement defect hid for so long.
    let per_frame = |d: &Delta| d.slots / d.frames.max(1);
    assert_eq!(
        per_frame(&small),
        per_frame(&large),
        "rows placed per frame went from {} to {} as the session grew -- \
         placement is following the transcript rather than the viewport",
        per_frame(&small),
        per_frame(&large)
    );
    assert!(
        per_frame(&large) <= 8 * (area.height as u64 + 4),
        "{} rows placed per frame across 8 panes of a {}-row screen is not a \
         viewport bound",
        per_frame(&large),
        area.height
    );

    // Entries visited still scales with the session: that is the known
    // O(entries) ceiling the row-placement work left in place deliberately.
    // Pinned so a future change that removes it is noticed, and so nobody
    // reads the byte constancy above as meaning nothing scales at all.
    assert!(
        large.entries > small.entries,
        "entries visited stopped scaling with the session, which contradicts \
         the recorded O(entries) ceiling -- if that was fixed on purpose, this \
         assertion is what should tell you to update the record"
    );
}

/// A resume point that has stopped describing its text.
///
/// Here rather than in `md`'s own tests because the counters are
/// process-global and the lib's tests run in parallel, which would make a
/// before/after delta meaningless. The `md_stale` counter is the only
/// evidence this ever happened -- the fallback produces a correct whole
/// render, so output, bytes and cells all look exactly right while the work
/// silently went from resuming a few lines to re-rendering everything.
fn stale_resume() {
    // A real resume point, taken from a long text and then applied to a
    // short one -- which is exactly how this happens: the cached offset stops
    // describing the entry it belongs to.
    let long = "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\n";
    let short = "one\n";
    let (_, stale) = kobold::md::render_from(long, 60, Default::default(), 235);
    assert!(
        stale.at > short.len(),
        "the fixture must actually produce a stale point"
    );

    let before = kobold::layout::stats();
    let (lines, _) = kobold::md::render_from(short, 60, stale, 235);
    let after = kobold::layout::stats();

    assert_eq!(
        after.md_stale - before.md_stale,
        1,
        "the discarded resume point must be counted"
    );
    assert_eq!(
        after.md_full - before.md_full,
        1,
        "and the whole render it degraded to counted too"
    );
    assert_eq!(
        after.md_resumed - before.md_resumed,
        0,
        "it did not resume anything"
    );
    assert_eq!(
        lines,
        kobold::md::render(short, 60, 235),
        "the fallback still renders correctly"
    );
}

/// A reply arriving word by word. The shape that proves memoisation works:
/// the entry is laid out once and extended thereafter.
fn stream(area: Rect, body: &str) {
    let sink = Sink::default();
    let mut screen = screen(&sink, area);
    let mut app = App::new("main", "b0");
    app.push(Who::Model, "");
    // The opening frame is outside the window: it is necessarily a full
    // repaint, and including it would make `repaints == 0` unassertable in
    // steady state.
    screen.draw(|buf, a| app.render(buf, a)).expect("draw");

    let deltas: Vec<&str> = body.split_inclusive(' ').collect();
    let d = measure(&sink, || {
        for part in &deltas {
            app.pane_mut()
                .transcript
                .last_mut()
                .expect("pushed")
                .text
                .push_str(part);
            screen.draw(|buf, a| app.render(buf, a)).expect("draw");
        }
    });

    assert_eq!(
        d.frames,
        deltas.len() as u64,
        "one frame per delta; a different count means the harness stopped measuring what it says"
    );
    // The invariant the whole memoisation exists for. Without it this is
    // `deltas.len()` -- 1248 at the time of writing -- and no other counter
    // in this file moves at all.
    //
    // Zero rather than one because the opening frame, which is where the
    // entry's single full layout happens, is deliberately outside the
    // measured window. Same property either way: the entry is laid out once
    // and extended thereafter.
    assert_eq!(
        d.full, 0,
        "the streaming entry is laid out by the opening frame, then only extended"
    );
    assert_eq!(
        d.resumed,
        deltas.len() as u64,
        "every delta must extend the previous layout rather than redo it"
    );
    assert_eq!(
        d.repaints, 0,
        "no full repaint once the opening frame is behind us"
    );
    // The markdown renderer's own view of the same property. `md_stale`
    // matters most: a resume point that stops describing its text degrades to
    // a whole render with correct output and no other symptom, so this is the
    // only place that failure is visible at all.
    // 10 rather than 0, and the number is exact: the fixture's opening line
    // is ten space-separated tokens, so the first ten deltas arrive before
    // any newline exists and there is nothing yet to freeze. `Entry::refresh`
    // still takes its own fast path for them -- they are cheap, the text is
    // barely a line -- but markdown genuinely restarts from the top, and the
    // counter says so rather than rounding it to "resumed".
    assert_eq!(
        d.md_full, 10,
        "only the deltas arriving before the first newline restart"
    );
    assert_eq!(
        d.md_resumed,
        deltas.len() as u64 - 10,
        "every delta after the first frozen line resumes the markdown render"
    );
    assert_eq!(
        d.md_stale, 0,
        "no resume point should stop describing its own text"
    );
    assert!(
        d.elided() >= 0.99,
        "streaming touches one growing entry, so nearly every cell should be untouched; got {:.3}",
        d.elided()
    );
    // Magnitude rather than invariant: this is the crossterm encoder's output
    // for these frames, and a crossterm upgrade may legitimately change it.
    // Re-measure and re-pin when that happens; do not widen it.
    assert_eq!(
        d.bytes_per_frame(),
        68,
        "bytes per streamed frame, pinned to the crossterm version in Cargo.lock"
    );
}

/// A transcript sliding up as turns are appended, which is what the scroll
/// hint exists for.
fn scroll(area: Rect, body: &str) {
    // Run twice: once letting the renderer emit its scroll hint, once with it
    // suppressed so every frame is a repaint. The ratio is the hint's whole
    // justification, so it is asserted rather than described.
    let run = |hint: bool| -> Delta {
        let sink = Sink::default();
        let mut screen = screen(&sink, area);
        let mut app = App::new("main", "b0");
        for i in 0..20 {
            app.push(Who::User, format!("question {i}"));
            app.push(Who::Model, body.to_owned());
        }
        screen.draw(|buf, a| app.render(buf, a)).expect("draw");

        measure(&sink, || {
            for i in 0..40 {
                app.push(Who::User, format!("another question {i}"));
                screen
                    .draw(|buf, a| {
                        let mut painted = app.render(buf, a);
                        if !hint {
                            painted.scroll = None;
                        }
                        painted
                    })
                    .expect("draw");
            }
        })
    };

    let slid = run(true);
    let full = run(false);

    // 40 appends inside the window, each a new entry laid out exactly once
    // ever -- not once per frame. Equality is the point: `>` would mean an
    // entry already on screen was being laid out again.
    assert_eq!(
        slid.full, 40,
        "each appended entry is laid out exactly once"
    );
    assert_eq!(
        slid.resumed, 0,
        "a user message arrives whole; nothing to resume"
    );
    // A shortfall means the hint stopped being emitted and those frames
    // silently became repaints -- correct output, far more bytes.
    assert_eq!(
        slid.scrolls, 40,
        "every append should slide the transcript rather than repaint it"
    );
    assert!(
        slid.elided() >= 0.95,
        "sliding should leave most cells untouched; got {:.3}",
        slid.elided()
    );
    let ratio = full.bytes_per_frame() as f64 / slid.bytes_per_frame().max(1) as f64;
    assert!(
        ratio >= 6.0,
        "the scroll hint should cost several times fewer bytes than a repaint; got {ratio:.1}x \
         ({} B/frame slid against {} B/frame repainted)",
        slid.bytes_per_frame(),
        full.bytes_per_frame()
    );
}

/// Typing with a long conversation on screen: the latency-critical path,
/// where re-laying the transcript is what a user feels as lag.
fn keystroke(area: Rect, body: &str) {
    let run = |turns: usize| -> Delta {
        let sink = Sink::default();
        let mut screen = screen(&sink, area);
        let mut app = App::new("main", "b0");
        for i in 0..turns {
            app.push(Who::User, format!("question {i}"));
            app.push(Who::Model, body.to_owned());
        }
        // Outside the window on purpose: this frame lays out every entry for
        // the first time, and it is exactly the cost the keystrokes after it
        // must not repeat.
        screen.draw(|buf, a| app.render(buf, a)).expect("draw");

        measure(&sink, || {
            for c in "the quick brown fox jumps over the lazy dog".chars() {
                app.pane_mut().insert(c);
                screen.draw(|buf, a| app.render(buf, a)).expect("draw");
            }
        })
    };

    let small = run(30);

    // The assertion that matters most: a keystroke re-lays out nothing. The
    // transcript did not change, only the prompt below it.
    assert_eq!(
        small.full, 0,
        "a keystroke must not lay out any transcript entry"
    );
    assert_eq!(small.resumed, 0, "nor resume one: no entry's text grew");
    assert_eq!(small.scrolls, 0, "typing does not move the transcript");
    assert_eq!(small.repaints, 0, "nor repaint it");
    // 60 entries checked on each of 43 frames, every one found still valid.
    // Asserted so that `full == 0` cannot be satisfied the wrong way: if the
    // refresh loop stopped visiting entries at all, `full` would also be
    // zero, and this is what tells the two apart.
    assert_eq!(
        small.hits,
        60 * 43,
        "every entry should be visited and found cached each frame"
    );

    // The windowing property, pinned directly rather than through a magic
    // number: a frame's cost is set by the viewport, so a transcript nearly
    // seven times longer must cost the same per frame. This is worth more
    // than any absolute figure here -- it is the property `frame-vs-history`
    // explores in wall-clock, asserted deterministically instead.
    let large = run(200);
    assert_eq!(large.full, 0, "still no re-layout with a long transcript");
    let (a, b) = (
        small.bytes_per_frame() as f64,
        large.bytes_per_frame() as f64,
    );
    let drift = (a - b).abs() / a.max(1.0);
    assert!(
        drift < 0.05,
        "per-frame cost must not track transcript length: {a:.0} B/frame at 30 turns against \
         {b:.0} at 200, {:.1}% apart",
        drift * 100.0
    );

    // What the bytes above do not say on their own, and what the slot
    // counter was built to see. `render_transcript` places rows before it
    // writes them, and that placement used to cover the whole transcript --
    // 38,799 rows placed to write thirty, at two hundred turns, with these
    // same byte counts flat throughout. Now it places what it writes.
    //
    // Stated as invariance rather than as a bound. A threshold alone is weak:
    // it passes for any transcript short enough. The same number at three
    // very different sizes is the property itself, and nothing satisfies that
    // by accident.
    let tiny = run(1);
    let per = |d: &Delta| d.slots as f64 / d.passes as f64;
    assert!(
        tiny.passes > 0 && small.passes > 0 && large.passes > 0,
        "the placement pass must actually run -- zero rows placed because it stopped \
         running reads exactly like zero because it got clever"
    );
    for (turns, d) in [(1, &tiny), (30, &small), (200, &large)] {
        assert!(
            per(d) <= 30.0,
            "at {turns} turns the pass placed {:.0} rows for a 30-row viewport",
            per(d)
        );
    }
    assert_eq!(
        per(&tiny) as u64,
        per(&large) as u64,
        "rows placed per frame must not move between 1 turn and 200: {:.0} against {:.0}",
        per(&tiny),
        per(&large)
    );
    assert_eq!(
        per(&small) as u64,
        per(&large) as u64,
        "nor between 30 and 200"
    );

    // Output identity, the before/after witness. Measured at 34 bytes a
    // frame at every size against the code this replaced -- 1468 bytes over
    // 43 frames, identical before and after -- and it must stay there: the
    // whole claim is "same output, less work", and a single byte of drift
    // would mean something visible changed.
    for (turns, d) in [(1, &tiny), (30, &small), (200, &large)] {
        assert_eq!(
            d.bytes_per_frame(),
            34,
            "bytes per frame moved at {turns} turns -- the frames are supposed to be identical"
        );
    }

    // The known ceiling, asserted rather than left to be rediscovered. The
    // fold still visits every entry to sum its lines, because any entry's
    // length can change between frames, so this stays O(entries) by design.
    // 400 visits against the 38,799 rows it replaced is ~97x, and closing the
    // rest would mean a running total invalidated per entry -- a second
    // source of truth for something currently derived. Wrong trade today;
    // recorded so the limit is known rather than surprising.
    assert_eq!(
        large.entries / large.passes,
        400,
        "one visit per entry per frame, which is the ceiling this fix does not remove"
    );
    assert_eq!(small.entries / small.passes, 60);
}
