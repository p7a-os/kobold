//! How often the transcript had to be laid out.
//!
//! `term::Stats` counts what reached the terminal; this counts the work done
//! before that, and the two are not substitutes. Memoisation is invisible to
//! every counter in `term`: deleting it outright turns each keystroke into
//! O(session) work while producing a byte-identical frame, so bytes, cells,
//! scrolls and repaints all sit perfectly still. Measured, not assumed --
//! streaming one reply goes from 1 full layout to 1248 with every `term`
//! counter unchanged to the byte. These are the numbers that move.
//!
//! The shape deliberately matches `term::Stats` -- process-global atomics,
//! `Relaxed`, one snapshot accessor, and callers taking a before/after delta
//! -- because a second style of instrument in the same codebase is one more
//! thing to learn for no benefit. There is deliberately no reset: a reset is
//! a second way to get a delta wrong, and subtracting two snapshots already
//! works.

use std::sync::atomic::{AtomicU64, Ordering};

static HITS: AtomicU64 = AtomicU64::new(0);
static RESUMED: AtomicU64 = AtomicU64::new(0);
static FULL: AtomicU64 = AtomicU64::new(0);
static MD_FULL: AtomicU64 = AtomicU64::new(0);
static MD_RESUMED: AtomicU64 = AtomicU64::new(0);
static MD_STALE: AtomicU64 = AtomicU64::new(0);
static SLOTS: AtomicU64 = AtomicU64::new(0);
static ENTRIES: AtomicU64 = AtomicU64::new(0);
static PASSES: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy)]
pub struct Stats {
    /// Entries whose cached lines were still valid: no work at all.
    pub hits: u64,
    /// Entries extended from where the last layout stopped. Streaming `n`
    /// deltas into one entry should report one `full` and `n - 1` `resumed`.
    pub resumed: u64,
    /// Entries laid out from scratch: a first render, a resize, or text that
    /// did not simply grow.
    ///
    /// The regression detector. `hits` and `resumed` are what make a failure
    /// legible -- when this rises, they say which fast path stopped firing.
    pub full: u64,
    /// Markdown renders that started at the top of the text.
    pub md_full: u64,
    /// Markdown renders that started from a resume point.
    pub md_resumed: u64,
    /// Resume points discarded because they did not describe their text.
    ///
    /// Diagnostic rather than load-bearing: a stale resume already forces the
    /// fall-through that `full` counts, so this says *why* rather than
    /// *that*. Counted within `md_full`, since a whole render is what it
    /// degrades to.
    pub md_stale: u64,
    /// Row slots built by `render_transcript`'s first pass, summed over every
    /// pass in the window.
    ///
    /// The first pass places every row of every entry before the second pass
    /// writes only the ones the viewport shows, so this is the number that
    /// says whether placement is windowed. Divided by `passes` it is
    /// slots-per-frame, and slots-per-frame rising with transcript length is
    /// the windowing property failing -- stated as a count rather than as a
    /// wall-clock growth ratio.
    pub slots: u64,
    /// Transcript entries visited by the same pass.
    ///
    /// Second counter for the same reason there are three layout counters
    /// rather than two: if `slots` grows, this says whether the cost is per
    /// entry or per line, which are different faults with different fixes.
    pub entries: u64,
    /// How many times the first pass ran, so the two above can be read per
    /// pass rather than as totals.
    ///
    /// Also the counter that tells "the pass is cheap" from "the pass did not
    /// run": zero slots with a non-zero `passes` is a pass that placed
    /// nothing, and zero of both is a pass that never happened.
    pub passes: u64,
}

impl Stats {
    /// Share of entry layouts that avoided a full pass. Falls when something
    /// starts invalidating the cache it did not need to.
    pub fn reuse(&self) -> f64 {
        let total = self.hits + self.resumed + self.full;
        if total == 0 {
            return 0.0;
        }
        (self.hits + self.resumed) as f64 / total as f64
    }
}

impl std::fmt::Display for Stats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} full layouts, {} resumed, {} cached ({:.1}% reused), md {} full / {} resumed, {} stale resumes",
            self.full,
            self.resumed,
            self.hits,
            self.reuse() * 100.0,
            self.md_full,
            self.md_resumed,
            self.md_stale,
        )?;
        write!(
            f,
            ", {} placement passes, {} slots ({:.1}/pass over {} entries)",
            self.passes,
            self.slots,
            self.slots as f64 / self.passes.max(1) as f64,
            self.entries,
        )
    }
}

/// A snapshot of what this process has laid out. Named to match
/// `term::stats()`, and used the same way: take two and subtract.
pub fn stats() -> Stats {
    Stats {
        hits: HITS.load(Ordering::Relaxed),
        resumed: RESUMED.load(Ordering::Relaxed),
        full: FULL.load(Ordering::Relaxed),
        md_full: MD_FULL.load(Ordering::Relaxed),
        md_resumed: MD_RESUMED.load(Ordering::Relaxed),
        md_stale: MD_STALE.load(Ordering::Relaxed),
        slots: SLOTS.load(Ordering::Relaxed),
        entries: ENTRIES.load(Ordering::Relaxed),
        passes: PASSES.load(Ordering::Relaxed),
    }
}

/// Record one run of `render_transcript`'s first pass.
///
/// Three atomics per pass, not per line: the totals are already in hand when
/// the pass ends, so there is nothing to count incrementally. Counting inside
/// the per-line loop instead would repeat the mistake this module's `Tally`
/// exists to document -- an atomic per item measured at ~2.7% of a frame.
pub(crate) fn pass(slots: u64, entries: u64) {
    SLOTS.fetch_add(slots, Ordering::Relaxed);
    ENTRIES.fetch_add(entries, Ordering::Relaxed);
    PASSES.fetch_add(1, Ordering::Relaxed);
}

/// What one entry's `refresh` had to do, tallied per frame rather than
/// reported per entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Cached lines still valid: no work.
    Hit,
    /// Extended from where the last layout stopped.
    Resumed,
    /// Laid out from scratch.
    Full,
}

/// Per-frame tally, flushed once instead of touching an atomic per entry.
///
/// Measured, not assumed: incrementing an atomic inside the per-entry loop
/// cost ~0.9us on a 35us frame at 400 entries -- about 2.7%, consistently, in
/// 8 of 8 interleaved runs. That is small but it is not nothing, and an
/// instrument that slows the thing it measures is a bad instrument. Batching
/// makes it three atomics per frame regardless of transcript length, which is
/// unmeasurable against the same baseline.
#[derive(Debug, Clone, Copy, Default)]
pub struct Tally {
    hits: u64,
    resumed: u64,
    full: u64,
}

impl Tally {
    pub fn add(&mut self, outcome: Outcome) {
        match outcome {
            Outcome::Hit => self.hits += 1,
            Outcome::Resumed => self.resumed += 1,
            Outcome::Full => self.full += 1,
        }
    }

    /// Fold a frame's tally into the process counters. Skips the atomics
    /// entirely for a frame that laid nothing out, which is most of them.
    pub fn flush(self) {
        if self.hits != 0 {
            HITS.fetch_add(self.hits, Ordering::Relaxed);
        }
        if self.resumed != 0 {
            RESUMED.fetch_add(self.resumed, Ordering::Relaxed);
        }
        if self.full != 0 {
            FULL.fetch_add(self.full, Ordering::Relaxed);
        }
    }
}

pub(crate) fn md_full() {
    MD_FULL.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn md_resumed() {
    MD_RESUMED.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn md_stale() {
    MD_STALE.fetch_add(1, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(hits: u64, resumed: u64, full: u64) -> Stats {
        Stats {
            hits,
            resumed,
            full,
            md_full: 0,
            md_resumed: 0,
            md_stale: 0,
            slots: 0,
            entries: 0,
            passes: 0,
        }
    }

    #[test]
    fn reuse_is_the_share_of_layouts_that_avoided_a_full_pass() {
        // Both fast paths count as reuse: a cache hit did no work at all, and
        // a resume did work proportional to the delta rather than the entry.
        assert_eq!(stats(0, 0, 4).reuse(), 0.0, "every layout was full");
        assert_eq!(stats(3, 1, 0).reuse(), 1.0, "no layout was full");
        assert_eq!(stats(1, 1, 2).reuse(), 0.5, "half and half");
        assert_eq!(stats(6, 2, 2).reuse(), 0.8);
    }

    #[test]
    fn reuse_of_nothing_is_zero_rather_than_a_division_by_zero() {
        // A window in which nothing was laid out at all -- an idle frame --
        // must report a number, not NaN.
        let r = stats(0, 0, 0).reuse();
        assert_eq!(r, 0.0);
        assert!(r.is_finite(), "reuse must never be NaN or infinite");
    }

    #[test]
    fn display_names_every_counter_it_carries() {
        // This line is what `KOBOLD_STATS` prints, so a counter silently
        // dropped from it is a counter nobody reads.
        let s = Stats {
            hits: 7,
            resumed: 3,
            full: 2,
            md_full: 5,
            md_resumed: 4,
            md_stale: 1,
            slots: 900,
            entries: 12,
            passes: 3,
        };
        let text = s.to_string();
        for expect in [
            "2 full layouts",
            "3 resumed",
            "7 cached",
            "83.3% reused",
            "5 full / 4 resumed",
            "1 stale",
            "3 placement passes",
            "900 slots",
            "300.0/pass",
            "12 entries",
        ] {
            assert!(text.contains(expect), "{expect:?} missing from {text:?}");
        }
    }

    #[test]
    fn a_tally_counts_each_outcome_separately_and_flushes_once() {
        let mut t = Tally::default();
        for o in [
            Outcome::Hit,
            Outcome::Hit,
            Outcome::Resumed,
            Outcome::Full,
            Outcome::Hit,
        ] {
            t.add(o);
        }
        assert_eq!((t.hits, t.resumed, t.full), (3, 1, 1));

        // Flushing is additive into the process counters, which is what makes
        // a before/after delta the right way to read them.
        let before = super::stats();
        t.flush();
        let after = super::stats();
        assert_eq!(after.hits - before.hits, 3);
        assert_eq!(after.resumed - before.resumed, 1);
        assert_eq!(after.full - before.full, 1);
    }
}
