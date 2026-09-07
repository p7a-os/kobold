//! Shared measurement fixtures.
//!
//! In the library rather than in each measuring binary because both the
//! benchmark (`src/bin/bench.rs`) and the performance gate (`tests/perf.rs`)
//! have to use *the same bytes*, and an integration test can only reach
//! `pub` items. Nothing in the running client calls any of this; it is here
//! for the one property a duplicated copy cannot have.
//!
//! That property is not hypothetical. The gate asserts a byte-per-frame count
//! and a scroll-hint ratio, and while it was being written the two copies of
//! `reply()` differed only in the wording of the closing sentence -- which
//! moved the measured ratio from 6.34x to 5.7x, a 10% swing that reads
//! exactly like a performance regression. A comment asking the next person to
//! keep two copies in sync would have deferred that failure, not prevented
//! it.

/// A long markdown reply: prose that wraps, a list, inline emphasis, a table
/// and a fenced code block. Deliberately the mix a real answer has, because
/// the fence and the table are the two constructs whose cost is not per line.
///
/// Editing this invalidates every magnitude the gate asserts. That is allowed
/// -- but re-measure and re-pin them in the same commit, rather than widening
/// a threshold until it passes.
pub fn reply() -> String {
    let mut s = String::new();
    s.push_str("Congestion control is how TCP decides **how fast** to send.\n\n");
    s.push_str("## The window\n\n");
    for i in 0..12 {
        s.push_str(&format!(
            "The sender keeps a congestion window, `cwnd`, and grows it while acknowledgements \
             keep arriving. Round {i} of the explanation adds another sentence so the paragraph \
             has to wrap more than once at a realistic terminal width.\n\n"
        ));
    }
    s.push_str("- Slow start doubles `cwnd` every round trip\n");
    s.push_str("- Congestion avoidance adds one segment per round trip\n");
    s.push_str("- A loss halves it, or drops to one on a timeout\n\n");
    s.push_str("| phase | growth | trigger |\n|---|---|---|\n");
    s.push_str("| slow start | exponential | connection open |\n");
    s.push_str("| avoidance | linear | cwnd past ssthresh |\n");
    s.push_str("| recovery | halved | duplicate acks |\n\n");
    s.push_str("```rust\n");
    for i in 0..30 {
        s.push_str(&format!(
            "fn round_{i}(cwnd: usize, acked: usize) -> usize {{\n    // grow while the path is clear\n    cwnd + acked / cwnd.max(1)\n}}\n"
        ));
    }
    s.push_str("```\n\n");
    s.push_str("That is the whole algorithm, minus the parts every stack disagrees about.\n");
    s
}
