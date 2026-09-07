//! Frame-cost measurement, not part of the client.
//!
//! Every scenario here is a shape the real client hits: a reply streaming in
//! word by word, a keystroke echoed while a long conversation is on screen, a
//! transcript sliding up by one row. The numbers that matter are per frame, so
//! that is what this reports -- totals hide the tail, and the tail is what a
//! user feels as a stutter.
//!
//!   bench            -> every scenario
//!   bench stream     -> one of them by name
//!
//! Wall-clock timings move with the machine. The stable numbers, and the ones
//! worth putting in a commit message, are `bytes/frame` and the growth ratio
//! between the first and last frames of a streaming run: those are properties
//! of the algorithm rather than of the box it ran on.

use std::cell::Cell as StdCell;
use std::io::{self, Write};
use std::rc::Rc;
use std::time::{Duration, Instant};

use kobold::app::{App, Who};
use kobold::fixture::reply;
use kobold::term::Screen;
use ratatui::backend::CrosstermBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

/// A terminal that measures instead of displaying: the byte counter is what
/// the wire cost of a frame actually is.
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

/// Per-frame timings. Reported as a distribution because the interesting
/// failure -- one frame in fifty costing ten times the rest -- is invisible in
/// a mean.
struct Stats {
    samples: Vec<Duration>,
}

impl Stats {
    fn new() -> Self {
        Stats {
            samples: Vec::new(),
        }
    }

    fn push(&mut self, d: Duration) {
        self.samples.push(d);
    }

    fn pct(&self, sorted: &[Duration], p: f64) -> Duration {
        if sorted.is_empty() {
            return Duration::ZERO;
        }
        let i = ((sorted.len() - 1) as f64 * p).round() as usize;
        sorted[i]
    }

    /// Mean of the first and last tenth, and their ratio. A renderer whose cost
    /// is flat in the length of what it is rendering scores about 1.0 here; one
    /// that redoes its work every frame grows without bound.
    fn growth(&self) -> (Duration, Duration, f64) {
        let n = self.samples.len();
        if n < 20 {
            return (Duration::ZERO, Duration::ZERO, 1.0);
        }
        let k = n / 10;
        let mean = |s: &[Duration]| s.iter().sum::<Duration>() / s.len() as u32;
        let first = mean(&self.samples[..k]);
        let last = mean(&self.samples[n - k..]);
        let ratio = last.as_secs_f64() / first.as_secs_f64().max(f64::EPSILON);
        (first, last, ratio)
    }

    fn report(&self, name: &str, extra: &str) {
        let mut sorted = self.samples.clone();
        sorted.sort();
        let total: Duration = self.samples.iter().sum();
        let mean = total / self.samples.len().max(1) as u32;
        let (first, last, ratio) = self.growth();
        println!(
            "{name:<22} n={:<5} mean {:>9.1?}  p50 {:>9.1?}  p99 {:>9.1?}  max {:>9.1?}  total {:>8.1?}",
            self.samples.len(),
            mean,
            self.pct(&sorted, 0.50),
            self.pct(&sorted, 0.99),
            self.pct(&sorted, 1.0),
            total,
        );
        if self.samples.len() >= 20 {
            println!(
                "{:<22} first-10% {:>9.1?}  last-10% {:>9.1?}  growth {:>5.2}x{}",
                "", first, last, ratio, extra
            );
        } else if !extra.is_empty() {
            println!("{:<22}{}", "", extra);
        }
    }
}

/// Render one frame into a detached buffer. Isolates layout and painting from
/// the diff and the wire, which is what most of these scenarios want to time.
fn paint(app: &mut App, area: Rect) {
    let mut buf = Buffer::empty(area);
    app.render(&mut buf, area);
}

/// Stream a reply in one delta per word, painting after each -- the real hot
/// path, and the one whose cost used to grow with the length of the reply.
fn stream(area: Rect, text: &str, name: &str) {
    let mut app = App::new("main", "b0");
    app.push(Who::Model, "");
    let mut stats = Stats::new();
    for part in text.split_inclusive(' ') {
        app.pane_mut()
            .transcript
            .last_mut()
            .expect("pushed")
            .text
            .push_str(part);
        let t = Instant::now();
        paint(&mut app, area);
        stats.push(t.elapsed());
    }
    stats.report(name, "");
}

/// Type into the prompt with a long conversation already on screen. This is the
/// latency-critical path: every millisecond here is one the user feels between
/// pressing a key and seeing it.
fn keystroke(area: Rect) {
    let mut app = App::new("main", "b0");
    let body = reply();
    for i in 0..30 {
        app.push(Who::User, format!("question {i}"));
        app.push(Who::Model, body.clone());
    }
    paint(&mut app, area);

    let mut stats = Stats::new();
    for c in "the quick brown fox jumps over the lazy dog".chars() {
        app.pane_mut().insert(c);
        let t = Instant::now();
        paint(&mut app, area);
        stats.push(t.elapsed());
    }
    stats.report("keystroke-echo", "");
}

/// Frame cost against conversation length. Flat is the whole point: a session
/// that has been running for an hour must not paint slower than a fresh one.
fn flat(area: Rect) {
    let body = reply();
    print!("{:<22}", "frame-vs-history");
    for n in [1usize, 10, 50, 200] {
        let mut app = App::new("main", "b0");
        for i in 0..n {
            app.push(Who::User, format!("question {i}"));
            app.push(Who::Model, body.clone());
        }
        paint(&mut app, area);
        // Dirty one cell the way a spinner tick does, so this measures a
        // steady-state repaint rather than the first layout.
        let mut best = Duration::MAX;
        for _ in 0..20 {
            app.spinner = app.spinner.wrapping_add(1);
            let t = Instant::now();
            paint(&mut app, area);
            best = best.min(t.elapsed());
        }
        print!("  {n:>4} turns {best:>8.1?}");
    }
    println!();
}

/// A backend that does nothing, so a frame can be timed with the escape
/// encoder and the write taken out and only the diff left in.
struct NullBackend;

impl ratatui::backend::Backend for NullBackend {
    type Error = io::Error;
    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a ratatui::buffer::Cell)>,
    {
        // Consumed, not discarded: walking the diff is the cost being measured.
        for cell in content {
            std::hint::black_box(cell);
        }
        Ok(())
    }
    fn hide_cursor(&mut self) -> io::Result<()> {
        Ok(())
    }
    fn show_cursor(&mut self) -> io::Result<()> {
        Ok(())
    }
    fn get_cursor_position(&mut self) -> io::Result<ratatui::layout::Position> {
        Ok(ratatui::layout::Position::new(0, 0))
    }
    fn set_cursor_position<P: Into<ratatui::layout::Position>>(&mut self, _: P) -> io::Result<()> {
        Ok(())
    }
    fn clear(&mut self) -> io::Result<()> {
        Ok(())
    }
    fn clear_region(&mut self, _: ratatui::backend::ClearType) -> io::Result<()> {
        Ok(())
    }
    fn size(&self) -> io::Result<ratatui::layout::Size> {
        Ok(ratatui::layout::Size::new(0, 0))
    }
    fn window_size(&mut self) -> io::Result<ratatui::backend::WindowSize> {
        Ok(ratatui::backend::WindowSize {
            columns_rows: ratatui::layout::Size::new(0, 0),
            pixels: ratatui::layout::Size::new(0, 0),
        })
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
    fn scroll_region_up(&mut self, _: core::ops::Range<u16>, _: u16) -> io::Result<()> {
        Ok(())
    }
    fn scroll_region_down(&mut self, _: core::ops::Range<u16>, _: u16) -> io::Result<()> {
        Ok(())
    }
}

/// Where a frame's time actually goes: laying the transcript out, walking the
/// diff, and turning the changed cells into escapes.
///
/// This decides whether a smaller `Cell` would be worth having. The case for
/// packing one rests on the diff being a large share of the frame; if laying
/// out is what costs, a cheaper cell buys nothing.
fn split(area: Rect) {
    let body = reply();
    let build = || {
        let mut app = App::new("main", "b0");
        for i in 0..20 {
            app.push(Who::User, format!("question {i}"));
            app.push(Who::Model, body.clone());
        }
        app
    };
    let best = |mut f: Box<dyn FnMut()>| {
        let mut best = Duration::MAX;
        for _ in 0..40 {
            let t = Instant::now();
            f();
            best = best.min(t.elapsed());
        }
        best
    };

    // Layout only, into a detached buffer.
    let mut app = build();
    let mut buf = Buffer::empty(area);
    app.render(&mut buf, area);
    let layout = best(Box::new(move || {
        app.spinner = app.spinner.wrapping_add(1);
        app.render(&mut buf, area);
    }));

    // Layout plus the diff, with nothing written.
    let mut app = build();
    let mut screen = Screen::with_backend(NullBackend, area);
    screen.draw(|b, a| app.render(b, a)).expect("draw");
    let diffed = best(Box::new(move || {
        app.spinner = app.spinner.wrapping_add(1);
        screen.draw(|b, a| app.render(b, a)).expect("draw");
    }));

    // And with the escapes actually encoded.
    let mut app = build();
    let sink = Sink::default();
    let mut screen = Screen::with_backend(CrosstermBackend::new(sink), area);
    screen.draw(|b, a| app.render(b, a)).expect("draw");
    let encoded = best(Box::new(move || {
        app.spinner = app.spinner.wrapping_add(1);
        screen.draw(|b, a| app.render(b, a)).expect("draw");
    }));

    let diff = diffed.saturating_sub(layout);
    let encode = encoded.saturating_sub(diffed);
    let pct = |d: Duration| 100.0 * d.as_secs_f64() / encoded.as_secs_f64().max(f64::EPSILON);
    println!(
        "frame-split {}x{:<10} layout {:>8.1?} ({:>4.1}%)  diff {:>8.1?} ({:>4.1}%)  encode+write {:>8.1?} ({:>4.1}%)  total {:>8.1?}",
        area.width,
        area.height,
        layout,
        pct(layout),
        diff,
        pct(diff),
        encode,
        pct(encode),
        encoded,
    );
}

/// Bytes on the wire for a transcript sliding up by one row, with and without
/// the scroll hint. The ratio is the property under test; it does not move with
/// the machine.
fn wire(area: Rect) {
    let body = reply();
    let run = |hint: bool| -> (usize, Duration) {
        let sink = Sink::default();
        let mut screen = Screen::with_backend(CrosstermBackend::new(sink.clone()), area);
        let mut app = App::new("main", "b0");
        for i in 0..20 {
            app.push(Who::User, format!("question {i}"));
            app.push(Who::Model, body.clone());
        }
        screen.draw(|buf, a| app.render(buf, a)).expect("draw");

        let before = sink.0.get();
        let mut total = Duration::ZERO;
        for i in 0..40 {
            app.push(Who::User, format!("another question {i}"));
            let t = Instant::now();
            screen
                .draw(|buf, a| {
                    let mut painted = app.render(buf, a);
                    if !hint {
                        painted.scroll = None;
                    }
                    painted
                })
                .expect("draw");
            total += t.elapsed();
        }
        ((sink.0.get() - before) / 40, total / 40)
    };

    let before = kobold::term::stats();
    let (slid_bytes, slid_time) = run(true);
    let after = kobold::term::stats();
    let (full_bytes, full_time) = run(false);
    println!(
        "{:<22} scroll-hint {slid_bytes:>6} B/frame {slid_time:>9.1?}   repaint {full_bytes:>6} B/frame {full_time:>9.1?}   {:.1}x fewer bytes",
        "wire-cost",
        full_bytes as f64 / slid_bytes.max(1) as f64,
    );
    // Cells the diff did not have to send, over the scroll-hint run. This is
    // the number that says damage tracking is still working; it drops as soon
    // as something starts dirtying rows it did not need to.
    let elided = 1.0
        - (after.written - before.written) as f64
            / (after.onscreen - before.onscreen).max(1) as f64;
    println!(
        "{:<22} {:.1}% of cells elided, {} scrolls, {} full repaints over {} frames",
        "",
        elided * 100.0,
        after.scrolls - before.scrolls,
        after.repaints - before.repaints,
        after.frames - before.frames,
    );
}

/// What the two backends put on the wire for identical frames.
///
/// Termina is said to pack SGR more tightly than crossterm, which is the only
/// reason kobold would move: it already drives the backend through its own
/// frame loop, diff and scroll regions, so nothing above the escape encoder
/// would change. This measures the claim instead of taking it, because
/// switching costs a rewrite of terminal setup, teardown and the event loop.
#[cfg(feature = "termina-ab")]
mod termina_ab {
    use super::*;
    use ratatui_termina::TerminaBackend;
    use std::cell::RefCell;
    use std::time::Duration;
    use termina::{Event, Terminal as TerminaTerminal, WindowSize};

    type Tape = Rc<RefCell<Vec<u8>>>;

    /// Records the stream once armed, so the opening full repaint can be left
    /// out and only steady-state frames compared.
    #[derive(Clone, Default)]
    struct Recorder {
        tape: Tape,
        armed: Rc<StdCell<bool>>,
    }

    impl Recorder {
        fn arm(&self) {
            self.armed.set(true);
        }
        fn bytes(&self) -> Vec<u8> {
            self.tape.borrow().clone()
        }
    }

    impl Write for Recorder {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            if self.armed.get() {
                self.tape.borrow_mut().extend_from_slice(b);
            }
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// The same recorder wearing termina's terminal trait. `event_reader` is
    /// unreachable: the backend never calls it, which is what makes this
    /// possible at all -- termina's real `EventReader` cannot be built by an
    /// outside implementor.
    struct TerminaRecorder {
        inner: Recorder,
        size: WindowSize,
    }

    impl Write for TerminaRecorder {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.inner.write(b)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl TerminaTerminal for TerminaRecorder {
        fn enter_raw_mode(&mut self) -> io::Result<()> {
            Ok(())
        }
        fn enter_cooked_mode(&mut self) -> io::Result<()> {
            Ok(())
        }
        fn get_dimensions(&self) -> io::Result<WindowSize> {
            Ok(self.size)
        }
        fn event_reader(&self) -> termina::EventReader {
            unimplemented!("the backend never reads events")
        }
        fn poll<F: Fn(&Event) -> bool>(&self, _: F, _: Option<Duration>) -> io::Result<bool> {
            Ok(false)
        }
        fn read<F: Fn(&Event) -> bool>(&self, _: F) -> io::Result<Event> {
            Err(io::Error::new(io::ErrorKind::WouldBlock, "no events"))
        }
        fn set_panic_hook(
            &mut self,
            _: impl Fn(&mut termina::PlatformHandle) + Send + Sync + 'static,
        ) {
        }
    }

    /// Split a stream into the categories that decide its size.
    fn categorise(d: &[u8]) -> [(&'static str, usize); 4] {
        let (mut sgr, mut cursor, mut other, mut text) = (0, 0, 0, 0);
        let mut i = 0;
        while i < d.len() {
            if d[i] == 0x1b && i + 1 < d.len() && d[i + 1] == b'[' {
                let mut j = i + 2;
                while j < d.len() && !d[j].is_ascii_alphabetic() {
                    j += 1;
                }
                let n = (j + 1).min(d.len()) - i;
                match d.get(j) {
                    Some(b'm') => sgr += n,
                    Some(b'H') | Some(b'G') | Some(b'd') | Some(b'A') | Some(b'B') | Some(b'C')
                    | Some(b'D') => cursor += n,
                    _ => other += n,
                }
                i += n;
                continue;
            }
            text += 1;
            i += 1;
        }
        [
            ("sgr", sgr),
            ("cursor", cursor),
            ("other", other),
            ("text", text),
        ]
    }

    const FRAMES: usize = 40;

    /// Twenty turns on screen, then one appended per frame so the transcript
    /// slides -- the shape the client spends its time in.
    fn drive<B: ratatui::backend::Backend>(screen: &mut Screen<B>, rec: &Recorder) -> Duration
    where
        B::Error: std::fmt::Debug,
    {
        let body = reply();
        let mut app = App::new("main", "b0");
        for i in 0..20 {
            app.push(Who::User, format!("question {i}"));
            app.push(Who::Model, body.clone());
        }
        screen.draw(|b, a| app.render(b, a)).expect("first frame");
        rec.arm();
        // Best of several passes: this is a few microseconds of encoding, so a
        // mean would mostly report what else the machine was doing.
        let mut best = Duration::MAX;
        for _ in 0..20 {
            let t = Instant::now();
            for i in 0..FRAMES {
                app.push(Who::User, format!("another question {i}"));
                screen.draw(|b, a| app.render(b, a)).expect("frame");
            }
            best = best.min(t.elapsed() / FRAMES as u32);
        }
        best
    }

    pub fn run(area: Rect) {
        let ct = Recorder::default();
        let mut screen = Screen::with_backend(CrosstermBackend::new(ct.clone()), area);
        let ct_time = drive(&mut screen, &ct);
        let ct = ct.bytes();

        let tm_rec = Recorder::default();
        let backend = TerminaBackend::new(TerminaRecorder {
            inner: tm_rec.clone(),
            size: WindowSize {
                cols: area.width,
                rows: area.height,
                pixel_width: None,
                pixel_height: None,
            },
        });
        let mut screen = Screen::with_backend(backend, area);
        let tm_time = drive(&mut screen, &tm_rec);
        let tm = tm_rec.bytes();

        // 20 passes were timed but only one recorded, so the tape holds that
        // many copies of each frame.
        let (ct_n, tm_n) = (ct.len() / (FRAMES * 20), tm.len() / (FRAMES * 20));
        println!(
            "{:<22} crossterm {:>4} B/frame {:>8.2?}/frame   termina {:>4} B/frame {:>8.2?}/frame   {:+.0}% bytes, {:+.0}% time",
            "termina-a-b",
            ct_n,
            ct_time,
            tm_n,
            tm_time,
            100.0 * (tm_n as f64 - ct_n as f64) / ct_n as f64,
            100.0 * (tm_time.as_secs_f64() - ct_time.as_secs_f64()) / ct_time.as_secs_f64(),
        );
        for (name, bytes) in [("crossterm", &ct), ("termina", &tm)] {
            let parts = categorise(bytes)
                .iter()
                .map(|(k, v)| format!("{k} {:>3}", v / (FRAMES * 20)))
                .collect::<Vec<_>>()
                .join("  ");
            println!("{:<22} {name:<10} {parts}  B/frame", "");
        }
        if std::env::var_os("BENCH_DUMP").is_some() {
            for (name, bytes) in [("crossterm", &ct), ("termina", &tm)] {
                let one = &bytes[..bytes.len() / (FRAMES * 20)];
                eprintln!(
                    "\n--- {name}, roughly one frame ---\n{:?}",
                    String::from_utf8_lossy(one)
                );
            }
        }
    }
}

fn main() {
    let area = Rect::new(0, 0, 100, 30);
    let only = std::env::args().nth(1);
    let want = |name: &str| only.as_deref().is_none_or(|o| name.starts_with(o));

    println!("terminal {}x{}\n", area.width, area.height);

    if want("stream") {
        let body = reply();
        // Prose alone, then the same reply with its code fence: the fence is
        // the construct whose re-highlighting cost is not per line.
        let prose = body.split("```").next().expect("has a prefix").to_owned();
        stream(area, &prose, "stream-prose");
        stream(area, &body, "stream-full");

        // A fence is one block, so it is live until its closing marker lands.
        // This is the worst shape a reply can take: the answer to "write me
        // that file", where nearly every delta extends the same open block.
        let mut big = String::from("Here is the file.\n\n```rust\n");
        for i in 0..200 {
            big.push_str(&format!(
                "fn step_{i}(state: &mut State) -> Result<(), Error> {{\n    state.advance({i});\n    Ok(())\n}}\n"
            ));
        }
        big.push_str("```\n\nThat is the whole file.\n");
        stream(area, &big, "stream-800-line-fence");
    }
    if want("keystroke") {
        keystroke(area);
    }
    if want("frame") {
        flat(area);
    }
    if want("split") {
        // Several sizes: the diff scales with the screen while laying out
        // scales with the transcript, so their ratio is not one number.
        for a in [
            Rect::new(0, 0, 80, 24),
            area,
            Rect::new(0, 0, 200, 50),
            Rect::new(0, 0, 400, 100),
        ] {
            split(a);
        }
    }
    if want("wire") {
        wire(area);
    }
    #[cfg(feature = "termina-ab")]
    if want("termina") {
        termina_ab::run(area);
    }
}
