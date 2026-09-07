//! Terminal ownership: raw mode, the alternate screen, and one synchronized
//! update per frame.
//!
//! `ratatui::init` is not used here because it gives no way to wrap a draw in
//! a synchronized update, which is the whole point of this module.

use std::io::{self, Stdout, Write};
use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use ratatui::backend::Backend;
#[cfg(not(feature = "termina-out"))]
use ratatui::backend::CrosstermBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::app::Painted;

/// Begin / end synchronized update, DEC private mode 2026.
///
/// The terminal queues everything between the two and paints it in one go,
/// instead of showing our intermediate cursor moves and half-painted rows --
/// which is what "flicker" and "tearing" in a TUI actually are.
///
/// Deliberately not gated on a capability query. A terminal that does not know
/// the mode ignores it, which is the documented fallback, so querying would
/// cost a startup round-trip to learn nothing actionable. Ghostty, Kitty,
/// WezTerm, iTerm2, foot and Alacritty >= 0.13 all implement it; tmux >= 3.4
/// passes it through, though it wants `terminal-features ',xterm*:sync'` to
/// forward it to the outer terminal rather than handling it itself.
const BEGIN_SYNC: &[u8] = b"\x1b[?2026h";
const END_SYNC: &[u8] = b"\x1b[?2026l";

/// Set while a draw is in progress, so the intermediate flushes ratatui does
/// when it repositions the cursor do not chop one frame into three.
///
/// A process has one terminal, so this is a static rather than plumbing a
/// handle through the backend -- `CrosstermBackend::writer_mut` is an unstable
/// API and not worth depending on for this.
static HOLD: AtomicBool = AtomicBool::new(false);

/// Whether a synchronized frame is currently open, so `restore` can close one
/// we died inside without emitting a stray reset when we did not.
static IN_FRAME: AtomicBool = AtomicBool::new(false);

/// Whether the Kitty keyboard flags were pushed, so `restore` pops exactly what
/// it pushed. Popping a stack we never pushed to would discard whatever the
/// program that launched us had set.
static KEYS_PUSHED: AtomicBool = AtomicBool::new(false);

/// Ask for the Kitty keyboard protocol's escape-code disambiguation, which is
/// the only way a terminal can report Shift+Enter as distinct from Enter.
///
/// Paired with `restore`, including on the panic path -- leaving the flags
/// pushed changes how every later program in that terminal sees its keyboard.
pub fn enable_key_disambiguation() -> bool {
    if !crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false) {
        return false;
    }
    let pushed = crossterm::execute!(
        io::stdout(),
        crossterm::event::PushKeyboardEnhancementFlags(
            crossterm::event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
        )
    )
    .is_ok();
    KEYS_PUSHED.store(pushed, Ordering::Relaxed);
    pushed
}

/// What frames have cost so far. Counting emissions against elisions is the
/// cheapest useful instrument a terminal renderer has: it says directly whether
/// an optimisation moved bytes, and it is the only way to see what a real
/// session costs rather than what a benchmark does.
///
/// Statics rather than fields because a process has one terminal, matching how
/// the synchronized-frame flags are kept.
static FRAMES: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);
static CELLS_WRITTEN: AtomicU64 = AtomicU64::new(0);
static CELLS_ONSCREEN: AtomicU64 = AtomicU64::new(0);
static SCROLLS: AtomicU64 = AtomicU64::new(0);
static REPAINTS: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy)]
pub struct Stats {
    pub frames: u64,
    pub bytes: u64,
    /// Cells the diff actually sent.
    pub written: u64,
    /// Cells on screen across those frames, so `onscreen - written` is what
    /// damage tracking saved.
    pub onscreen: u64,
    /// Frames that asked the terminal to slide a row range.
    pub scrolls: u64,
    /// Frames that gave up and repainted everything: first frame, and resizes.
    pub repaints: u64,
}

impl Stats {
    /// Share of cells the diff did not have to send. The number to watch: it
    /// falls when something starts dirtying the screen needlessly.
    pub fn elided(&self) -> f64 {
        if self.onscreen == 0 {
            return 0.0;
        }
        1.0 - (self.written as f64 / self.onscreen as f64)
    }
}

impl std::fmt::Display for Stats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let per = |n: u64| n as f64 / self.frames.max(1) as f64;
        write!(
            f,
            "{} frames, {} bytes ({:.0} B/frame), {} cells written of {} ({:.1}% elided), {} scrolls, {} full repaints",
            self.frames,
            self.bytes,
            per(self.bytes),
            self.written,
            self.onscreen,
            self.elided() * 100.0,
            self.scrolls,
            self.repaints,
        )
    }
}

/// A snapshot of what this process has painted.
pub fn stats() -> Stats {
    Stats {
        frames: FRAMES.load(Ordering::Relaxed),
        bytes: BYTES.load(Ordering::Relaxed),
        written: CELLS_WRITTEN.load(Ordering::Relaxed),
        onscreen: CELLS_ONSCREEN.load(Ordering::Relaxed),
        scrolls: SCROLLS.load(Ordering::Relaxed),
        repaints: REPAINTS.load(Ordering::Relaxed),
    }
}

/// Opens the synchronized frame on the first byte written and closes it on the
/// flush that ends the draw. The markers travel in the same stream as the
/// frame, so wrapping costs two writes of eight bytes and no extra syscall.
pub struct SyncOut<W: Write> {
    inner: W,
}

impl<W: Write> Write for SyncOut<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if !IN_FRAME.swap(true, Ordering::Relaxed) {
            self.inner.write_all(BEGIN_SYNC)?;
            BYTES.fetch_add(BEGIN_SYNC.len() as u64, Ordering::Relaxed);
        }
        BYTES.fetch_add(buf.len() as u64, Ordering::Relaxed);
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        if !HOLD.load(Ordering::Relaxed) && IN_FRAME.swap(false, Ordering::Relaxed) {
            self.inner.write_all(END_SYNC)?;
        }
        self.inner.flush()
    }
}

/// Termina's escape encoder, driven over the terminal crossterm still owns.
///
/// Termina writes noticeably tighter escapes than crossterm: the sixteen basic
/// colours go out as `SGR 90` rather than `SGR 38;5;8`, and one implicit `SGR`
/// resets a cell's attributes where crossterm sends `39`, `49`, `59` and then a
/// full reset anyway. Measured over sliding-transcript frames at 100x30, that is
/// 93 bytes per frame against 63, with frame time unchanged.
///
/// Adopting it does not mean adopting termina's terminal. `Screen` only ever
/// asks the backend to draw, clear, flush, move the cursor, report a size and
/// slide a region -- never `get_cursor_position`, the one method that has to
/// read a reply. So the parts of termina's `Terminal` that concern input stay
/// unreachable, and raw mode, the alternate screen, bracketed paste, the
/// keyboard protocol and the event stream all remain crossterm's, which is
/// where kobold's key handling already is.
#[cfg(feature = "termina-out")]
pub struct TerminaOut<W: Write = SyncOut<Stdout>> {
    inner: W,
}

#[cfg(feature = "termina-out")]
impl<W: Write> TerminaOut<W> {
    pub fn new(inner: W) -> Self {
        Self { inner }
    }
}

#[cfg(feature = "termina-out")]
impl<W: Write> Write for TerminaOut<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(feature = "termina-out")]
impl<W: Write> termina::Terminal for TerminaOut<W> {
    /// Crossterm owns the mode; entering it twice would capture the state it
    /// already changed and restore the wrong thing.
    fn enter_raw_mode(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn enter_cooked_mode(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn get_dimensions(&self) -> io::Result<termina::WindowSize> {
        let (cols, rows) = crossterm::terminal::size()?;
        Ok(termina::WindowSize {
            cols,
            rows,
            pixel_width: None,
            pixel_height: None,
        })
    }

    /// Unreachable by construction: the backend only reads events to answer
    /// `get_cursor_position`, which `Screen` never calls. Termina's real
    /// `EventReader` cannot be built from outside the crate, so there is
    /// nothing to return even in principle -- but the two methods that could
    /// plausibly be reached report "no events" rather than panicking.
    fn event_reader(&self) -> termina::EventReader {
        unreachable!("kobold reads keys through crossterm, never through the backend")
    }

    fn poll<F: Fn(&termina::Event) -> bool>(
        &self,
        _filter: F,
        _timeout: Option<std::time::Duration>,
    ) -> io::Result<bool> {
        Ok(false)
    }

    fn read<F: Fn(&termina::Event) -> bool>(&self, _filter: F) -> io::Result<termina::Event> {
        Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "input does not come through the backend",
        ))
    }

    /// `restore` is installed by main as an ordinary panic hook, and covers
    /// everything this one would.
    fn set_panic_hook(
        &mut self,
        _f: impl Fn(&mut termina::PlatformHandle) + Send + Sync + 'static,
    ) {
    }
}

/// The backend `Screen::init` builds. Swapping it changes only how cells become
/// escape sequences; everything above is written against `Backend`.
#[cfg(not(feature = "termina-out"))]
pub type DefaultBackend = CrosstermBackend<SyncOut<Stdout>>;
#[cfg(feature = "termina-out")]
pub type DefaultBackend = ratatui_termina::TerminaBackend<TerminaOut>;

/// The screen: a backend, the pair of buffers, and the diff between them.
///
/// This stands in for `ratatui::Terminal`, for one reason. The transcript is
/// pinned to the bottom, so when it grows every visible row shifts up and the
/// diff degenerates into a full repaint -- 15.9 KB on a 400x100 terminal, and
/// around two thirds of every byte kobold writes. A terminal can slide a row
/// range itself for a few bytes, but using that means updating the record of
/// what is on screen, and `Terminal` keeps that buffer private.
pub struct Screen<B: Backend = DefaultBackend> {
    backend: B,
    /// What the terminal is showing.
    shown: Buffer,
    /// What it should show next.
    next: Buffer,
    /// Set when `shown` may not describe the screen -- before the first frame,
    /// and after a resize -- so the next frame repaints in full instead of
    /// trusting a scroll against a stale record.
    resync: bool,
    cursor_hidden: bool,
    /// Set when there is no tty to ask for the size, which is how the scroll
    /// path gets driven against a simulated terminal in the tests.
    fixed: Option<Rect>,
}

impl Screen<DefaultBackend> {
    pub fn init() -> io::Result<Self> {
        crossterm::terminal::enable_raw_mode()?;
        crossterm::execute!(
            io::stdout(),
            crossterm::terminal::EnterAlternateScreen,
            // Makes a paste arrive as one event instead of one key per character.
            crossterm::event::EnableBracketedPaste,
        )?;
        let out = SyncOut {
            inner: io::stdout(),
        };
        #[cfg(not(feature = "termina-out"))]
        let backend = CrosstermBackend::new(out);
        #[cfg(feature = "termina-out")]
        let backend = ratatui_termina::TerminaBackend::new(TerminaOut::new(out));
        let area = Rect::new(0, 0, 0, 0);
        Ok(Self {
            backend,
            shown: Buffer::empty(area),
            next: Buffer::empty(area),
            resync: true,
            cursor_hidden: false,
            fixed: None,
        })
    }
}

impl<B: Backend> Screen<B> {
    /// A screen over any backend at a fixed size.
    ///
    /// Exists so the scroll path can be driven against `TestBackend`, which
    /// implements the same scroll-region semantics independently: the mirror
    /// this type keeps of the screen can then be checked against a model of
    /// the screen rather than against itself.
    pub fn with_backend(backend: B, area: Rect) -> Self {
        Self {
            backend,
            shown: Buffer::empty(area),
            next: Buffer::empty(area),
            resync: true,
            cursor_hidden: false,
            fixed: Some(area),
        }
    }

    pub fn backend(&self) -> &B {
        &self.backend
    }

    /// Paint one frame as a single atomic terminal update.
    pub fn draw(
        &mut self,
        render: impl FnOnce(&mut Buffer, Rect) -> Painted,
    ) -> Result<(), B::Error> {
        let area = match self.fixed {
            Some(area) => area,
            None => {
                let size = self.backend.size()?;
                Rect::new(0, 0, size.width, size.height)
            }
        };
        if self.next.area != area {
            self.shown.resize(area);
            self.next.resize(area);
            self.resync = true;
        }
        self.next.reset();
        let painted = render(&mut self.next, area);

        HOLD.store(true, Ordering::Relaxed);
        let painted = self.paint(painted);
        HOLD.store(false, Ordering::Relaxed);
        // Closes the synchronized frame, and runs even when painting failed:
        // one left open makes the terminal stop displaying anything at all.
        let closed = self.backend.flush();
        painted.and(closed)
    }

    fn paint(&mut self, painted: Painted) -> Result<(), B::Error> {
        if self.resync {
            // `shown` is not to be trusted, so make it something the diff is
            // guaranteed to overwrite completely, and match that on screen.
            self.backend.clear()?;
            self.shown.reset();
            self.resync = false;
            REPAINTS.fetch_add(1, Ordering::Relaxed);
        } else if let Some((region, lines)) = painted.scroll {
            self.slide(region, lines)?;
        }

        let Self {
            backend,
            shown,
            next,
            ..
        } = self;
        let written = std::cell::Cell::new(0u64);
        backend.draw(
            shown
                .diff_iter(next)
                .inspect(|_| written.set(written.get() + 1)),
        )?;
        FRAMES.fetch_add(1, Ordering::Relaxed);
        CELLS_WRITTEN.fetch_add(written.get(), Ordering::Relaxed);
        CELLS_ONSCREEN.fetch_add(next.content.len() as u64, Ordering::Relaxed);

        match painted.cursor {
            Some(at) => {
                if self.cursor_hidden {
                    self.backend.show_cursor()?;
                    self.cursor_hidden = false;
                }
                // Unconditional: scrolling a region leaves the cursor
                // somewhere undefined, and the diff moves it around anyway.
                self.backend.set_cursor_position(at)?;
            }
            None => {
                if !self.cursor_hidden {
                    self.backend.hide_cursor()?;
                    self.cursor_hidden = true;
                }
            }
        }

        core::mem::swap(&mut self.shown, &mut self.next);
        Ok(())
    }

    /// Ask the terminal to slide a range of rows, and mirror the move in
    /// `shown` so the diff that follows only paints the rows the slide
    /// exposed.
    ///
    /// The mirror has to match the escape exactly. It does by construction:
    /// rows move, and the rows the move vacates become empty cells, which is
    /// what `Cell::reset` produces and what the terminal writes there.
    ///
    /// A wrong `lines` is a bytes problem, not a correctness one -- the diff
    /// still runs afterwards and still reconciles the screen. Only a mismatch
    /// between this and what the escape actually did could corrupt anything.
    fn slide(&mut self, region: Range<u16>, lines: i32) -> Result<(), B::Error> {
        let height = region.end.saturating_sub(region.start);
        let n = u16::try_from(lines.unsigned_abs())
            .unwrap_or(u16::MAX)
            .min(height);
        if n == 0 || height == 0 {
            return Ok(());
        }
        // Every buffer here is anchored at the origin, so a row's cells start
        // at `y * width`.
        debug_assert_eq!((self.shown.area.x, self.shown.area.y), (0, 0));
        let w = self.shown.area.width as usize;
        let (top, bottom) = (region.start as usize * w, region.end as usize * w);
        let moved = n as usize * w;
        SCROLLS.fetch_add(1, Ordering::Relaxed);
        let band = &mut self.shown.content[top..bottom];
        let vacated = band.len() - moved;
        if lines > 0 {
            // Content moves up; the bottom `n` rows are vacated.
            band.rotate_left(moved);
            for cell in &mut band[vacated..] {
                cell.reset();
            }
            self.backend.scroll_region_up(region, n)
        } else {
            band.rotate_right(moved);
            for cell in &mut band[..moved] {
                cell.reset();
            }
            self.backend.scroll_region_down(region, n)
        }
    }
}

impl<B: Backend> Drop for Screen<B> {
    fn drop(&mut self) {
        if self.fixed.is_none() {
            restore();
        }
    }
}

/// Undo everything `Screen::init` did. Safe to call more than once, and from a
/// panic hook, so a crash cannot leave the terminal in raw mode on the
/// alternate screen with its display held.
pub fn restore() {
    let mut out = io::stdout();
    // Close a frame we died inside, but do not emit a stray reset otherwise:
    // the begin/end pairs should balance for anything parsing the stream, tmux
    // included.
    if IN_FRAME.swap(false, Ordering::Relaxed) {
        let _ = out.write_all(END_SYNC);
    }
    // Before leaving the alternate screen, and on the panic path too: flags left
    // pushed outlive the process and change how the next program sees its keys.
    if KEYS_PUSHED.swap(false, Ordering::Relaxed) {
        let _ = crossterm::execute!(out, crossterm::event::PopKeyboardEnhancementFlags);
    }
    let _ = crossterm::execute!(
        out,
        crossterm::cursor::Show,
        crossterm::event::DisableBracketedPaste,
        crossterm::terminal::LeaveAlternateScreen,
    );
    let _ = crossterm::terminal::disable_raw_mode();
    let _ = out.flush();
    // Same leak, different sequence: `osc::title` is set once at startup and
    // nothing ever clears it, so the terminal's tab or window keeps showing
    // "kobold -- <model>" long after the process is gone.
    crate::progress::set(crate::progress::State::Clear);
    crate::osc::reset_title();
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::style::Style;

    /// Serialises the tests that paint against the one that inspects a raw
    /// `SyncOut`.
    ///
    /// `HOLD` is process-global, and `Screen::draw` holds it true for the
    /// duration of a paint so that the many writes making up a frame close
    /// their synchronized-update marker once, at the end, rather than after
    /// each. `SyncOut::flush` therefore only emits the closing marker when
    /// `HOLD` is false. Cargo runs these tests as parallel threads in one
    /// process, so a `Screen::draw` in flight elsewhere makes the marker
    /// assertion below see an unbalanced stream and fail -- measured at four
    /// failures in forty runs before this lock existed.
    ///
    /// Serialising rather than making `HOLD` per-instance on purpose: the
    /// flag is genuinely global in production -- one terminal, one process,
    /// no concurrent draws -- so this models what really happens, and the
    /// alternative would mean changing the paint path to fix a test-isolation
    /// problem. Every test that calls `Screen::draw` takes this, not just the
    /// two obvious ones; a draw anywhere is what breaks the assertion.
    static PAINTING: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Held for the whole of a test that paints or inspects `SyncOut`.
    /// Ignores poisoning: a panic in one test has already failed that test,
    /// and refusing the lock afterwards would turn one failure into many.
    fn painting() -> std::sync::MutexGuard<'static, ()> {
        PAINTING.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Paint rows `base..base+height` as "row N", which makes a scroll by k
    /// look exactly like the transcript sliding by k.
    fn rows(buf: &mut Buffer, area: Rect, base: usize) -> Painted {
        for y in 0..area.height {
            let text = format!("row {}", base + y as usize);
            for (i, ch) in text.chars().enumerate() {
                if (i as u16) < area.width {
                    buf[(i as u16, y)].set_char(ch).set_style(Style::default());
                }
            }
        }
        Painted::default()
    }

    fn expected(area: Rect, base: usize) -> Buffer {
        let mut b = Buffer::empty(area);
        rows(&mut b, area, base);
        b
    }

    #[test]
    fn a_slid_frame_leaves_the_screen_exactly_where_a_full_repaint_would() {
        let _painting = painting();
        let area = Rect::new(0, 0, 24, 8);
        let mut screen = Screen::with_backend(TestBackend::new(24, 8), area);

        screen.draw(|buf, a| rows(buf, a, 0)).unwrap();
        assert_eq!(screen.backend().buffer(), &expected(area, 0));

        // Slide up by two, and say so. The terminal scrolls; only the two rows
        // the scroll exposed should need painting.
        screen
            .draw(|buf, a| Painted {
                scroll: Some((0..8, 2)),
                ..rows(buf, a, 2)
            })
            .unwrap();
        assert_eq!(screen.backend().buffer(), &expected(area, 2));
        assert_eq!(
            &screen.shown,
            &expected(area, 2),
            "mirror drifted from the screen"
        );

        // And back down again.
        screen
            .draw(|buf, a| Painted {
                scroll: Some((0..8, -2)),
                ..rows(buf, a, 0)
            })
            .unwrap();
        assert_eq!(screen.backend().buffer(), &expected(area, 0));
    }

    #[test]
    fn a_wrong_scroll_hint_costs_bytes_but_never_correctness() {
        let _painting = painting();
        // The hint is only ever an optimisation: the diff runs afterwards and
        // reconciles whatever the scroll got wrong.
        let area = Rect::new(0, 0, 24, 8);
        let mut screen = Screen::with_backend(TestBackend::new(24, 8), area);
        screen.draw(|buf, a| rows(buf, a, 0)).unwrap();

        for (claimed, actual) in [(5i32, 1usize), (-3, 4), (99, 6), (1, 6)] {
            screen
                .draw(|buf, a| Painted {
                    scroll: Some((0..8, claimed)),
                    ..rows(buf, a, actual)
                })
                .unwrap();
            assert_eq!(
                screen.backend().buffer(),
                &expected(area, actual),
                "claimed {claimed}, actually moved to {actual}"
            );
        }
    }

    /// A writer the test can still read after handing it to the backend
    /// (`CrosstermBackend::writer` is an unstable API).
    #[derive(Clone, Default)]
    struct Tap(std::rc::Rc<std::cell::RefCell<Vec<u8>>>);
    impl Write for Tap {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_slide_goes_out_as_a_scroll_region_and_not_a_repaint() {
        let _painting = painting();
        // Pins the wire format, and the size of the win: the whole point is
        // that a scrolling transcript costs a fixed handful of bytes instead
        // of one repaint per visible cell.
        let area = Rect::new(0, 0, 80, 24);
        // Full-width rows, because that is what a transcript looks like and
        // what makes a repaint expensive. Short rows would understate it.
        let wide = |buf: &mut Buffer, a: Rect, base: usize| {
            for y in 0..a.height {
                let text = format!("row {} ", base + y as usize).repeat(20);
                for (i, ch) in text.chars().take(a.width as usize).enumerate() {
                    buf[(i as u16, y)].set_char(ch);
                }
            }
            Painted::default()
        };
        let frame_two = |hint: bool| {
            let tap = Tap::default();
            let mut screen =
                Screen::with_backend(ratatui::backend::CrosstermBackend::new(tap.clone()), area);
            screen.draw(|buf, a| wide(buf, a, 0)).unwrap();
            let before = tap.0.borrow().len();
            screen
                .draw(|buf, a| Painted {
                    scroll: hint.then_some((0..24, 1)),
                    ..wide(buf, a, 1)
                })
                .unwrap();
            let out = tap.0.borrow()[before..].to_vec();
            out
        };

        let repainted = frame_two(false);
        let slid = frame_two(true);

        // Set the scrolling region, scroll it inside that region, reset it.
        let text = String::from_utf8_lossy(&slid).into_owned();
        assert!(
            text.contains("\u{1b}[1;24r"),
            "no scrolling region: {text:?}"
        );
        assert!(text.contains("\u{1b}[1S"), "no scroll up: {text:?}");
        // A slide costs the scroll escape plus the one row it exposed, so it
        // stays flat as the screen grows while a repaint scales with it. The
        // margin here is deliberately loose; the benchmark measures the real
        // ratio, which on a 400x100 terminal is 15,474 bytes against 72.
        assert!(
            slid.len() * 5 < repainted.len(),
            "slide {} bytes vs repaint {} bytes",
            slid.len(),
            repainted.len()
        );
    }

    #[test]
    fn sliding_only_touches_the_named_rows() {
        let _painting = painting();
        // The bar and the prompt sit outside the transcript band and must not
        // move with it.
        let area = Rect::new(0, 0, 24, 8);
        let mut screen = Screen::with_backend(TestBackend::new(24, 8), area);
        let paint = |buf: &mut Buffer, a: Rect, base: usize| {
            for y in 0..6u16 {
                for (i, ch) in format!("row {}", base + y as usize).chars().enumerate() {
                    buf[(i as u16, y)].set_char(ch);
                }
            }
            for (i, ch) in "fixed footer".chars().enumerate() {
                buf[(i as u16, 7)].set_char(ch);
            }
            let _ = a;
        };
        screen
            .draw(|buf, a| {
                paint(buf, a, 0);
                Painted::default()
            })
            .unwrap();
        screen
            .draw(|buf, a| {
                paint(buf, a, 3);
                Painted {
                    scroll: Some((0..6, 3)),
                    ..Painted::default()
                }
            })
            .unwrap();

        let mut want = Buffer::empty(area);
        paint(&mut want, area, 3);
        assert_eq!(screen.backend().buffer(), &want);
    }

    /// Everything `Screen` asks of a backend, driven through termina's encoder
    /// over the shim, so the stubbed halves of `termina::Terminal` are proven
    /// unreachable rather than assumed to be.
    #[cfg(feature = "termina-out")]
    #[test]
    fn the_termina_shim_survives_every_call_a_frame_makes() {
        let _painting = painting();
        let area = Rect::new(0, 0, 24, 8);
        let tap = Tap::default();
        let backend =
            ratatui_termina::TerminaBackend::new(TerminaOut::new(SyncOut { inner: tap.clone() }));
        let mut screen = Screen::with_backend(backend, area);

        // A first frame clears and repaints, a second slides a region and
        // shows the cursor, a third hides it again.
        screen.draw(|buf, a| rows(buf, a, 0)).unwrap();
        screen
            .draw(|buf, a| {
                rows(buf, a, 2);
                Painted {
                    cursor: Some(ratatui::layout::Position::new(3, 4)),
                    scroll: Some((0..8, 2)),
                }
            })
            .unwrap();
        screen.draw(|buf, a| rows(buf, a, 2)).unwrap();

        let out = String::from_utf8_lossy(&tap.0.borrow()).into_owned();
        assert!(out.contains("\u{1b}[1;8r"), "no scrolling region: {out:?}");
        assert!(
            out.contains("\u{1b}[?2026h"),
            "frames must still be synchronized"
        );
        assert!(out.contains("\u{1b}[?2026l"), "and closed again");
    }

    #[test]
    fn elision_is_the_share_of_cells_the_diff_did_not_send() {
        // The counters themselves are process-global, so the real numbers come
        // from the benchmark, which owns its process. What is worth pinning
        // here is the arithmetic, since a sign error would quietly report a
        // renderer as efficient exactly when it had stopped being so.
        let s = |written, onscreen| Stats {
            frames: 1,
            bytes: 0,
            written,
            onscreen,
            scrolls: 0,
            repaints: 0,
        };
        assert_eq!(
            s(0, 100).elided(),
            1.0,
            "an unchanged frame elides everything"
        );
        assert_eq!(s(100, 100).elided(), 0.0, "a full repaint elides nothing");
        assert!((s(25, 100).elided() - 0.75).abs() < f64::EPSILON);
        assert_eq!(s(0, 0).elided(), 0.0, "no frames yet is not 100% efficient");
    }

    #[test]
    fn a_frame_is_one_synchronized_update_however_many_writes_it_takes() {
        let _painting = painting();
        let mut out = SyncOut { inner: Vec::new() };
        out.write_all(b"cells").unwrap();
        out.write_all(b"cursor").unwrap();
        out.flush().unwrap();
        assert_eq!(out.inner, b"\x1b[?2026hcellscursor\x1b[?2026l");

        // A flush with nothing written since must not emit a stray reset: the
        // pairs have to balance for anything parsing the stream, tmux included.
        out.inner.clear();
        out.flush().unwrap();
        assert!(out.inner.is_empty(), "unbalanced end-sync: {:?}", out.inner);
    }

    // ---- Stats -----------------------------------------------------------
    //
    // `KOBOLD_STATS` prints this line to the user's terminal after `restore`,
    // and it is the only place the render counters are ever shown. Nothing
    // asserted the formatting, so three mutants of the arithmetic inside it
    // survived -- a line that reads plausibly and reports the wrong thing.

    #[test]
    fn the_stats_line_reports_per_frame_and_elision_correctly() {
        let s = Stats {
            frames: 4,
            bytes: 400,
            written: 25,
            onscreen: 100,
            scrolls: 3,
            repaints: 1,
        };
        let shown = s.to_string();

        // 400 bytes over 4 frames is 100, not 400. The per-frame divide is
        // the whole reason this line exists rather than printing the totals:
        // "how much does a frame cost" is the question, and the raw byte
        // count answers a different one.
        assert!(
            shown.contains("100 B/frame"),
            "per-frame divide is wrong: {shown}"
        );
        // 25 of 100 written is 75% elided. Reporting 25% -- the fraction
        // written rather than the fraction saved -- is the plausible-and
        // -backwards version, and it would make damage tracking look broken
        // when it is working.
        assert!(
            shown.contains("75.0% elided"),
            "elision is inverted or wrong: {shown}"
        );
        assert!(shown.contains("4 frames"), "{shown}");
        assert!(shown.contains("3 scrolls"), "{shown}");
        assert!(shown.contains("1 full repaints"), "{shown}");
    }

    #[test]
    fn an_empty_screen_reports_nothing_elided_rather_than_dividing_by_zero() {
        // Reachable: `KOBOLD_STATS` on a session that exited before painting.
        // The guard exists so this is 0% rather than NaN, and "NaN% elided"
        // is the kind of output that gets read as a bug in the counters.
        // Written out rather than `Default`: adding a derive to a
        // production type for test ergonomics is the same move as deriving
        // one to give a mutation tool a target, and it is refused here for
        // the same reason.
        let s = Stats {
            frames: 0,
            bytes: 0,
            written: 0,
            onscreen: 0,
            scrolls: 0,
            repaints: 0,
        };
        assert_eq!(s.elided(), 0.0);
        let shown = s.to_string();
        assert!(!shown.contains("NaN"), "divided by zero: {shown}");
        assert!(
            shown.contains("0 B/frame"),
            "frames.max(1) is not guarding: {shown}"
        );
    }

    #[test]
    fn elision_is_the_fraction_saved_not_the_fraction_written() {
        // The direction, on its own, because it is the one that is wrong in
        // a way that looks right. Writing every cell is 0% elided; writing
        // none of them is 100%.
        let all = Stats {
            frames: 1,
            bytes: 0,
            written: 100,
            onscreen: 100,
            scrolls: 0,
            repaints: 0,
        };
        let none = Stats {
            frames: 1,
            bytes: 0,
            written: 0,
            onscreen: 100,
            scrolls: 0,
            repaints: 0,
        };
        assert_eq!(all.elided(), 0.0, "writing every cell elided nothing");
        assert_eq!(none.elided(), 1.0, "writing no cells elided everything");
    }

    #[test]
    fn a_scroll_of_zero_lines_is_not_a_scroll_at_all() {
        let _painting = painting();
        // Reachable: the hint is derived from the offset between frames, and
        // a frame that did not move produces zero. The guard turns that into
        // nothing; without it the terminal is sent a scroll-region escape for
        // no lines, and the counter records a scroll that never happened --
        // which would quietly inflate the one number the perf gate reads to
        // decide whether the scroll optimisation is still alive.
        let area = Rect::new(0, 0, 24, 8);
        let mut screen = Screen::with_backend(TestBackend::new(24, 8), area);
        screen.draw(|buf, a| rows(buf, a, 0)).unwrap();

        let before = stats().scrolls;
        screen
            .draw(|buf, a| Painted {
                scroll: Some((0..8, 0)),
                ..rows(buf, a, 0)
            })
            .unwrap();
        assert_eq!(
            stats().scrolls,
            before,
            "a zero-line hint was counted as a scroll"
        );

        // The partner: a real scroll still counts. A guard that rejected
        // everything would satisfy the assertion above and disable the
        // optimisation entirely.
        screen
            .draw(|buf, a| Painted {
                scroll: Some((0..8, 2)),
                ..rows(buf, a, 2)
            })
            .unwrap();
        assert_eq!(
            stats().scrolls,
            before + 1,
            "a real scroll stopped being counted"
        );
    }

    #[test]
    fn cells_written_counts_the_cells_the_diff_actually_sent() {
        let _painting = painting();
        // The counter the whole efficiency gate rests on: `written` against
        // `onscreen` is what "97.5% elided" means, and a `written` stuck at
        // zero reports perfect elision for a frame that repainted everything.
        // Nothing asserted it against a known number.
        let area = Rect::new(0, 0, 10, 2);
        let mut screen = Screen::with_backend(TestBackend::new(10, 2), area);

        // First paint: every cell differs from the empty mirror.
        let before = stats();
        screen.draw(|buf, a| rows(buf, a, 0)).unwrap();
        let first = stats();
        assert!(
            first.written > before.written,
            "the opening frame wrote no cells, so the counter is not counting"
        );

        // Second paint, identical content: the diff sends nothing.
        let mid = stats();
        screen.draw(|buf, a| rows(buf, a, 0)).unwrap();
        assert_eq!(
            stats().written,
            mid.written,
            "an identical frame still wrote cells, so the diff is not eliding"
        );

        // Third: change exactly one cell, and exactly one cell should go out.
        let mid = stats();
        screen
            .draw(|buf, a| {
                let p = rows(buf, a, 0);
                buf[(0, 0)].set_char('Z');
                p
            })
            .unwrap();
        assert_eq!(
            stats().written - mid.written,
            1,
            "one changed cell should send exactly one cell"
        );
    }
}
