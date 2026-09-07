//! Streaming text-to-speech: chunking now, engine behind a trait.
//!
//! The latency-critical piece is not the model, it is deciding *when* a partial
//! sentence is worth speaking. Waiting for a full sentence adds hundreds of
//! milliseconds; flushing on every comma produces flat, choppy prosody. So the
//! chunker flushes on punctuation with a different length threshold per class,
//! and the engine is whatever can consume 25-60 character clauses.

use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::process::Command;
use tokio::sync::mpsc;

/// A clause that is worth speaking on its own.
pub type Chunk = String;

/// Hard ceiling. Past this we break at a word boundary rather than let a
/// run-on sentence stall audio indefinitely.
const MAX_CHARS: usize = 200;

/// Clause punctuation is only a *fallback* split once speech is under way.
///
/// Every chunk is synthesised as a standalone utterance: it gets capitalised,
/// gets a full stop if it does not end in punctuation, and starts fresh
/// prosody. Splitting "We can chat, brainstorm ideas, solve a problem, or just
/// pass the time." at its commas therefore produces a trailing-comma hang, then
/// "Or just pass the time." as a new sentence -- audibly a pause and a reset in
/// the middle of a thought. Only the opening clause is worth that cost, and
/// only because nothing is playing yet.
const CLAUSE_FALLBACK: usize = 140;

/// Accumulates streamed text and yields speakable clauses.
#[derive(Default)]
pub struct Chunker {
    buf: String,
    /// Nothing has been spoken yet this turn. The first clause is the only one
    /// the listener actually waits on -- every later clause is synthesised
    /// while the previous one plays -- so it flushes far earlier, trading a
    /// little prosody for a much lower time to first sound.
    opening: bool,
    /// Fenced code is skipped entirely: reading punctuation and braces aloud is
    /// noise, and a code block can be hundreds of characters with no clause
    /// boundary at all.
    in_code: bool,
}

impl Chunker {
    /// Feed a delta. Returns any clauses that became speakable.
    /// Call at the start of a turn so the next clause gets opening thresholds.
    pub fn begin(&mut self) {
        self.opening = true;
        self.buf.clear();
        self.in_code = false;
    }

    pub fn push(&mut self, delta: &str) -> Vec<Chunk> {
        let mut out = Vec::new();
        for ch in delta.chars() {
            if ch == '\n' {
                // The fence marker sits at the start of the current *line*,
                // not of the buffer -- the buffer usually still holds prose
                // from before the block, which must be kept.
                let ls = self.buf.rfind('\n').map_or(0, |i| i + 1);
                let fence = self.buf[ls..].trim_start().starts_with("```");
                if fence {
                    self.in_code = !self.in_code;
                    self.buf.truncate(ls);
                    continue;
                }
                if self.in_code {
                    self.buf.truncate(ls);
                    continue;
                }
            }
            if self.in_code {
                self.buf.push(ch);
                continue;
            }

            self.buf.push(ch);
            if let Some(chunk) = self.take_if_ready() {
                out.push(chunk);
            }
        }
        out
    }

    /// End of turn: speak whatever is left, however short.
    pub fn flush(&mut self) -> Option<Chunk> {
        self.in_code = false;
        let text = normalize(&std::mem::take(&mut self.buf));
        (!text.is_empty()).then_some(text)
    }

    fn take_if_ready(&mut self) -> Option<Chunk> {
        let n = self.buf.chars().count();
        let last = self.buf.chars().next_back()?;

        // A sentence end is worth speaking sooner than a clause break, and a
        // clause break sooner than an arbitrary cut.
        // Opening thresholds are roughly a third of the steady-state ones.
        let ready = if self.opening {
            match last {
                '.' | '!' | '?' => n >= 6,
                ',' | ';' | ':' | '—' | '–' => n >= 12,
                _ => n >= 24 && last == ' ',
            }
        } else {
            match last {
                '.' | '!' | '?' => n >= 20,
                // Long enough that the sentence was going to sound like two
                // anyway; below this, keep it together.
                ',' | ';' | ':' | '—' | '–' => n >= CLAUSE_FALLBACK,
                _ => false,
            }
        };
        let ceiling = if self.opening { 32 } else { MAX_CHARS };
        if !ready && n < ceiling {
            return None;
        }

        if !ready {
            // Over the ceiling with no boundary in sight: cut at the last space
            // so a word is never split across two utterances.
            let cut = self.buf.rfind(' ')?;
            let rest = self.buf.split_off(cut);
            let head = std::mem::replace(&mut self.buf, rest.trim_start().to_owned());
            self.opening = false;
            return Some(normalize(&head));
        }

        let text = normalize(&std::mem::take(&mut self.buf));
        if !text.is_empty() {
            self.opening = false;
        }
        (!text.is_empty()).then_some(text)
    }
}

/// Collapse all whitespace runs to a single space. Line breaks are layout, not
/// speech, and a TTS engine given "word.\n\nword" may pause oddly.
fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Anything that can speak clauses in order.
/// Voices the bundled Pocket TTS engine ships with.
pub const VOICES: &[&str] = &[
    "alba", "marius", "javert", "jean", "fantine", "cosette", "eponine", "azelma",
];

/// What travels to the engine worker.
enum Msg {
    Speak(String),
    /// A line the engine interprets rather than pronounces.
    Control(String),
    Cancel,
    /// Start the engine without giving it anything to say, so the model is
    /// loaded before the first clause rather than during it.
    Warm,
}

pub trait StreamingTts: Send {
    fn enqueue(&mut self, text: Chunk);
    /// Start loading now. Model load takes seconds; doing it on the first
    /// clause puts all of that in front of the first word.
    fn warm(&mut self) {}
    /// Drop everything not yet spoken. Used when a turn is interrupted.
    fn cancel(&mut self);
    /// Switch voice mid-session. Returns false when the engine cannot do it
    /// without a restart, so the caller can say so instead of silently
    /// appearing to succeed.
    fn set_voice(&mut self, _name: &str) -> bool {
        false
    }
}

/// Speaks by piping each clause to an external command -- `say` on macOS,
/// `piper`/`espeak-ng` elsewhere. Zero extra crates, and it makes the chunker
/// usable before any model is wired in.
///
/// Chunks are spoken strictly in order by a single worker: overlapping two
/// utterances would garble them, and the queue is what lets synthesis of the
/// next clause overlap playback of the current one.
pub struct CommandTts {
    tx: mpsc::UnboundedSender<Msg>,
    resident: bool,
}

impl CommandTts {
    pub fn spawn(program: String, args: Vec<String>) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<Msg>();
        tokio::spawn(async move {
            while let Some(item) = rx.recv().await {
                match item {
                    Msg::Cancel => while rx.try_recv().is_ok() {},
                    // A speak-and-exit command has nothing to warm and no way
                    // to be told anything.
                    Msg::Control(_) | Msg::Warm => {}
                    Msg::Speak(text) => {
                        let _ = Command::new(&program).args(&args).arg(&text).status().await;
                    }
                }
            }
        });
        Self {
            tx,
            resident: false,
        }
    }

    /// One long-lived process fed clauses on stdin, one per line.
    ///
    /// Spawning a fresh process per clause costs its startup and voice load
    /// every time -- for `say` that is most of the gap between utterances --
    /// and resets prosody at every boundary. A resident process pays it once.
    /// The tradeoff is that cancelling means killing the child, since there is
    /// no way to tell it to stop mid-sentence.
    /// `notices` receives the engine's stderr, line by line. First run downloads
    /// a torch wheel and model weights, which takes minutes -- silence there
    /// looks identical to a hang.
    pub fn spawn_resident(
        program: String,
        args: Vec<String>,
        notices: mpsc::UnboundedSender<String>,
    ) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<Msg>();
        tokio::spawn(async move {
            let mut child: Option<tokio::process::Child> = None;
            while let Some(item) = rx.recv().await {
                match item {
                    Msg::Cancel => {
                        while rx.try_recv().is_ok() {}
                        if let Some(mut c) = child.take() {
                            let _ = c.kill().await;
                        }
                    }
                    Msg::Warm | Msg::Speak(_) | Msg::Control(_) => {
                        let line = match &item {
                            Msg::Speak(t) | Msg::Control(t) => Some(t.clone()),
                            _ => None,
                        };
                        if child.is_none() {
                            let mut spawned = Command::new(&program)
                                .args(&args)
                                .stdin(std::process::Stdio::piped())
                                .stderr(std::process::Stdio::piped())
                                .spawn()
                                .ok();
                            if let Some(err) = spawned.as_mut().and_then(|c| c.stderr.take()) {
                                let notices = notices.clone();
                                tokio::spawn(async move {
                                    let mut lines = tokio::io::BufReader::new(err).lines();
                                    while let Ok(Some(line)) = lines.next_line().await {
                                        if notices.send(line).is_err() {
                                            break;
                                        }
                                    }
                                });
                            }
                            child = spawned;
                        }
                        let Some(text) = line else { continue };
                        let Some(c) = child.as_mut() else { continue };
                        let Some(stdin) = c.stdin.as_mut() else {
                            continue;
                        };
                        if stdin
                            .write_all(format!("{text}\n").as_bytes())
                            .await
                            .is_err()
                            || stdin.flush().await.is_err()
                        {
                            // Died or refused input; rebuild on the next clause.
                            child = None;
                        }
                    }
                }
            }
        });
        Self { tx, resident: true }
    }
}

impl StreamingTts for CommandTts {
    fn enqueue(&mut self, text: Chunk) {
        let _ = self.tx.send(Msg::Speak(text));
    }
    fn cancel(&mut self) {
        let _ = self.tx.send(Msg::Cancel);
    }
    fn warm(&mut self) {
        if self.resident {
            let _ = self.tx.send(Msg::Warm);
        }
    }
    fn set_voice(&mut self, name: &str) -> bool {
        // Only a resident engine can be told anything; a speak-and-exit command
        // would just pronounce the control line.
        if !self.resident {
            return false;
        }
        let _ = self.tx.send(Msg::Control(format!(":voice {name}")));
        true
    }
}

/// Default port kobold pushes audio to. Paired with an SSH remote forward:
///
/// ```text
/// ssh -R 7077:localhost:7077 <host>
/// ```
///
/// so a connection to 127.0.0.1:7077 *on the server* is carried back to a
/// player running on the laptop. The server synthesises; the laptop plays.
pub const AUDIO_PORT: u16 = 7077;

/// Synthesises on this machine and ships each utterance to a listener.
///
/// Framing is a 4-byte big-endian length followed by one complete WAV file, so
/// the player never has to guess where an utterance ends and can hand each one
/// to the system player untouched.
pub struct SocketTts {
    tx: mpsc::UnboundedSender<Option<Chunk>>,
}

impl SocketTts {
    /// `synth` receives the text as its final argument and must write a WAV to
    /// stdout. Chosen over a linked model so the engine can be swapped without
    /// rebuilding, and so a missing engine fails loudly instead of silently.
    pub fn spawn(synth: Vec<String>, addr: String) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<Option<Chunk>>();
        tokio::spawn(async move {
            // Reconnected lazily: the laptop-side player may not be listening
            // when kobold starts, and voice may never be switched on.
            let mut sock: Option<TcpStream> = None;
            while let Some(item) = rx.recv().await {
                let Some(text) = item else {
                    while rx.try_recv().is_ok() {}
                    continue;
                };
                let Some((program, args)) = synth.split_first() else {
                    continue;
                };
                let Ok(out) = Command::new(program).args(args).arg(&text).output().await else {
                    continue;
                };
                if !out.status.success() || out.stdout.is_empty() {
                    continue;
                }
                if sock.is_none() {
                    sock = TcpStream::connect(&addr).await.ok();
                    if let Some(s) = sock.as_ref() {
                        let _ = s.set_nodelay(true);
                    }
                }
                let Some(s) = sock.as_mut() else { continue };
                let len = (out.stdout.len() as u32).to_be_bytes();
                if s.write_all(&len).await.is_err() || s.write_all(&out.stdout).await.is_err() {
                    // Player went away; drop the socket and retry on the next
                    // utterance rather than killing the whole worker.
                    sock = None;
                }
            }
        });
        Self { tx }
    }
}

impl StreamingTts for SocketTts {
    fn enqueue(&mut self, text: Chunk) {
        let _ = self.tx.send(Some(text));
    }
    fn cancel(&mut self) {
        let _ = self.tx.send(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(c: &mut Chunker, s: &str) -> Vec<Chunk> {
        // One character at a time, the way deltas actually arrive.
        s.chars().flat_map(|ch| c.push(&ch.to_string())).collect()
    }

    #[test]
    fn sentence_end_flushes_once_long_enough() {
        let mut c = Chunker::default();
        assert!(
            feed(&mut c, "Yes. ").is_empty(),
            "too short to be worth speaking"
        );
        let out = feed(&mut c, "That is possible for several reasons.");
        assert_eq!(out, vec!["Yes. That is possible for several reasons."]);
    }

    #[test]
    fn a_sentence_is_not_split_at_its_commas() {
        // The case that produced audible pauses mid-thought: each chunk is
        // spoken as its own sentence, so splitting here is worse than waiting.
        let mut c = Chunker::default();
        let out = feed(
            &mut c,
            "We can chat, brainstorm ideas, solve a problem, or just pass the time.",
        );
        assert_eq!(out.len(), 1, "split into {out:?}");
        assert!(out[0].ends_with('.'));
    }

    #[test]
    fn a_very_long_clause_still_breaks_before_it_stalls_audio() {
        let mut c = Chunker::default();
        let long = "and then, ".repeat(30);
        let out = feed(&mut c, &long);
        assert!(!out.is_empty(), "a run-on must not hold audio indefinitely");
    }

    #[test]
    fn long_run_on_breaks_at_a_word_boundary() {
        let mut c = Chunker::default();
        // Long enough to pass the ceiling now that clause splitting is a
        // fallback rather than the normal path.
        let out = feed(&mut c, &"alpha ".repeat(60));
        assert!(!out.is_empty());
        for chunk in &out {
            assert!(chunk.chars().count() <= MAX_CHARS);
            assert!(!chunk.ends_with("alph"), "word was split: {chunk}");
        }
    }

    #[test]
    fn code_blocks_are_not_spoken() {
        let mut c = Chunker::default();
        let mut out = feed(
            &mut c,
            "Here it is.\n```rust\nfn main() { println!(\"hi\"); }\n```\n",
        );
        out.extend(feed(&mut c, "That prints a greeting to the console."));
        let all = out.join(" ");
        assert!(!all.contains("println"), "code leaked into speech: {all}");
        assert!(all.contains("greeting"));
    }

    #[test]
    fn opening_clause_flushes_far_earlier() {
        let text = "Yes, that is right and here is why it happens in practice.";

        let mut opening = Chunker::default();
        opening.begin();
        let first = feed(&mut opening, text);
        let head = first.first().expect("opening should produce a clause");
        assert!(
            head.chars().count() <= 32,
            "opening clause too long: {head:?}"
        );

        // Steady state waits for much more text before the first flush.
        let mut steady = Chunker::default();
        let later = feed(&mut steady, text);
        assert!(
            later[0].chars().count() > head.chars().count(),
            "steady state should wait longer than the opening clause"
        );
    }

    #[test]
    fn flush_emits_the_tail() {
        let mut c = Chunker::default();
        feed(&mut c, "short tail");
        assert_eq!(c.flush().as_deref(), Some("short tail"));
        assert_eq!(c.flush(), None);
    }
}
