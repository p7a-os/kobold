//! Streaming Pocket TTS engine.
//!
//! Reads one clause per line on stdin and plays it, or writes it to a WAV.
//! Everything expensive happens once at startup: model load, voice state,
//! warmup. A clause arriving later pays only inference.
//!
//! Pipeline, following Kyutai's own reference loop:
//!
//!     clause -> prompt_text -> generate_step -> latent --(channel)--> decode_latent -> PCM -> audio
//!                                  ^                                       |
//!                                  +--- keeps stepping while Mimi decodes --+
//!
//! The channel is the point: waiting for Mimi to decode frame N before
//! generating frame N+1 serialises two things that are independent, and costs
//! most of the streaming advantage.

use std::io::{BufRead, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;

use anyhow::{Context, Result};
use ptts::tts_model::{prepare_text_prompt, TTSConfig, TTSModel};
use sha2::{Digest, Sha256};
use xn::nn::VB;
use xn::Tensor;

/// Where the model is mirrored, so running kobold needs no Hugging Face
/// account.
///
/// Upstream (`kyutai/pocket-tts`) is gated: fetching it means accepting terms
/// and holding a token. That is a fair thing to ask of whoever publishes this
/// and not of whoever runs it. The model is CC-BY-4.0, so the gate is passed
/// once at publish time and the terms travel with the files as NOTICE.
///
/// Path-style, deliberately. The bucket name contains dots, and S3's wildcard
/// certificate is only one label deep, so `kobold.prd.gm.s3.<region>...` fails
/// the TLS hostname check before a request is even sent.
const MIRROR: &str = "https://s3.us-east-2.amazonaws.com/kobold.prd.gm/models/tts/pocket-tts/v1";

/// SHA-256 of every file the engine fetches, as published in the bundle.
///
/// Compiled in rather than read from the mirror: a checksum served by the same
/// host as the file it describes proves the transfer arrived intact and
/// nothing more. These ship with the binary, so a mirror that begins serving
/// something else is caught rather than trusted. Pinning them is only safe
/// because the `v1` prefix is immutable by construction -- new weights become
/// `v2`, and this table moves with them.
const SUMS: &[(&str, &str)] = &[
    (
        "tokenizer.model",
        "d461765ae179566678c93091c5fa6f2984c31bbe990bf1aa62d92c64d91bc3f6",
    ),
    (
        "tts_b6369a24.safetensors",
        "a4246e239af0f35a1c495b6d180961a6f10b379dc24dd537f64c695c08e4e216",
    ),
    (
        "embeddings/alba.safetensors",
        "ad234695323e4030336b6afc8a050c97e3110603e11ecd8226d9562488300a50",
    ),
    (
        "embeddings/azelma.safetensors",
        "ef33fad34437cb187d2702f0a946d8ba7a01efdb8efbc8088c770d49c181ba73",
    ),
    (
        "embeddings/cosette.safetensors",
        "ca8926c4f234afa9d722173967e7bebdc6269538ca5910d65f41c3c1317717d3",
    ),
    (
        "embeddings/eponine.safetensors",
        "bb31940f62da665391de139da2e57d740757df26b73d7ec24152c78a3b8ac0c5",
    ),
    (
        "embeddings/fantine.safetensors",
        "b6918a2ece002d2d9037ff53c4ea38730175e8798786658b0958443edf49d355",
    ),
    (
        "embeddings/javert.safetensors",
        "2e857904ee76657e083b0e92664d21bd133e37df320af6eb04f752e679422d91",
    ),
    (
        "embeddings/jean.safetensors",
        "329530f87ce503061acefca8669300963420ff97e43647a326aa46bd987b983c",
    ),
    (
        "embeddings/marius.safetensors",
        "33f75e45fac0005630671f4b1bb632d51b6a083b18417de94855bbd7596a0630",
    ),
];

const TEMPERATURE: f32 = 0.6;
const VOICES: &[&str] = &[
    "alba", "marius", "javert", "jean", "fantine", "cosette", "eponine", "azelma",
];
const WEIGHTS: &str = "tts_b6369a24.safetensors";
const TOKENIZER: &str = "tokenizer.model";

struct Args {
    voice: String,
    quant: String,
    wav: Option<String>,
    max_frames: usize,
    buffer_ms: usize,
}

impl Args {
    fn parse() -> Self {
        let mut a = Args {
            // Env default so kobold's settings.json reaches us without the
            // caller having to build an argv.
            voice: std::env::var("KOBOLD_TTS_VOICE").unwrap_or_else(|_| "alba".into()),
            quant: "q8".into(),
            wav: None,
            max_frames: 2000,
            buffer_ms: audio::DEFAULT_BUFFER_MS,
        };
        let argv: Vec<String> = std::env::args().skip(1).collect();
        let mut i = 0;
        while i < argv.len() {
            let next = argv.get(i + 1).cloned().unwrap_or_default();
            match argv[i].as_str() {
                "--voice" => a.voice = next,
                // Unquantized is the reference; q8 is the one worth beating.
                "--quant" => a.quant = next,
                // Verification path for a machine with no audio device.
                "--wav" => a.wav = Some(next),
                "--max-frames" => a.max_frames = next.parse().unwrap_or(a.max_frames),
                // Raise if speech breaks up; lower to start sooner.
                "--buffer-ms" => a.buffer_ms = next.parse().unwrap_or(a.buffer_ms),
                _ => {
                    i += 1;
                    continue;
                }
            }
            i += 2;
        }
        a
    }
}

struct SpTokenizer(sentencepiece::SentencePieceProcessor);

impl ptts::Tokenizer for SpTokenizer {
    fn encode(&self, text: &str) -> xn::Result<Vec<u32>> {
        Ok(self
            .0
            .encode(text)
            .map_err(xn::Error::wrap)?
            .into_iter()
            .map(|v| v.id)
            .collect())
    }
    fn decode(&self, tokens: &[u32]) -> xn::Result<String> {
        self.0.decode_piece_ids(tokens).map_err(xn::Error::wrap)
    }
}

/// The published weights use different tensor names than the crate expects.
fn remap_key(name: &str) -> Option<String> {
    if name.contains("flow.w_s_t")
        || name.contains("quantizer.vq")
        || name.contains("quantizer.logvar_proj")
    {
        return None;
    }
    let mut name = name.to_string();
    name = name.replace(
        "flow_lm.condition_provider.conditioners.speaker_wavs.output_proj.weight",
        "flow_lm.speaker_proj_weight",
    );
    name = name.replace(
        "flow_lm.condition_provider.conditioners.transcript_in_segment.",
        "flow_lm.conditioner.",
    );
    name = name.replace("flow_lm.backbone.", "flow_lm.transformer.");
    name = name.replace("flow_lm.flow.", "flow_lm.flow_net.");
    name = name.replace("mimi.model.", "mimi.");
    Some(name)
}

/// Fold typographic punctuation to ASCII before tokenising.
///
/// Not cosmetic. The tokenizer fragments around U+2019: measured on the same
/// six words, the curly form produced six voiced runs against three, and 0.3s
/// more silence, because it splits mid-word and the model pauses at each
/// piece. Models emit these characters constantly, so this is the common case,
/// not an edge one.
fn to_ascii_punct(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' | '\u{2032}' => '\'',
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' | '\u{2033}' => '"',
            // Dashes read as a break; a hyphen is the safe spoken equivalent.
            '\u{2010}'..='\u{2015}' | '\u{2212}' => '-',
            // Non-breaking and other exotic spaces.
            '\u{00A0}' | '\u{2007}' | '\u{202F}' | '\u{2009}' => ' ',
            other => other,
        })
        .flat_map(|c| {
            // One character that has to become three.
            if c == '\u{2026}' {
                "...".chars().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}

fn note(msg: &str) {
    // stderr, because stdout may be carrying audio and kobold reads these
    // lines to show engine progress in its transcript.
    eprintln!("kobold-tts: {msg}");
}

/// Resolve the three files a run needs, fetching whatever is not cached.
fn download(voice: &str) -> Result<(std::path::PathBuf, std::path::PathBuf, std::path::PathBuf)> {
    if !VOICES.contains(&voice) {
        anyhow::bail!("unknown voice '{voice}'; available: {}", VOICES.join(", "));
    }
    let weights = fetch(WEIGHTS)?;
    let tokenizer = fetch(TOKENIZER).context("tokenizer")?;
    let voice = fetch(&format!("embeddings/{voice}.safetensors"))
        .with_context(|| format!("voice '{voice}'"))?;
    Ok((weights, tokenizer, voice))
}

/// Where fetched files live between runs.
fn cache_dir() -> Result<std::path::PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cache")))
        .context("neither XDG_CACHE_HOME nor HOME is set, so there is nowhere to cache")?;
    // Versioned to match the mirror prefix: a future v2 caches alongside
    // rather than on top of what a running v1 install already trusts.
    Ok(base.join("kobold").join("pocket-tts").join("v1"))
}

/// Fetch `rel` from the mirror into the cache, or return the cached copy.
///
/// Verified on the way in and trusted on the way out. Re-hashing 225 MB at
/// every start would cost about a second to re-establish what the atomic
/// rename below already guarantees: a file only appears at its final path
/// once it has been downloaded whole and checked.
fn fetch(rel: &str) -> Result<std::path::PathBuf> {
    let dest = cache_dir()?.join(rel);
    if dest.is_file() {
        return Ok(dest);
    }
    let want = SUMS
        .iter()
        .find(|(name, _)| *name == rel)
        .map(|(_, hash)| *hash)
        .with_context(|| format!("no published checksum for '{rel}'"))?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Overridable so the bundle can be served from somewhere else -- a local
    // copy, an internal mirror, a CloudFront distribution in front of the same
    // bucket -- without a rebuild. The checksums still have to match, so this
    // relocates the download without weakening what is accepted.
    let base = std::env::var("KOBOLD_TTS_MIRROR").unwrap_or_default();
    let base = match base.trim() {
        "" => MIRROR,
        set => set,
    };
    let url = format!("{}/{rel}", base.trim_end_matches('/'));
    note(&format!("fetching {rel} (first run only)"));
    let mut resp = ureq::get(&url)
        .call()
        .with_context(|| format!("fetching {url}"))?;
    let total: u64 = resp
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    // A unique suffix, so two engines starting at once cannot scribble on one
    // another's partial file. The rename that follows is atomic regardless.
    let part = dest.with_extension(format!("part.{}", std::process::id()));
    let mut out = std::fs::File::create(&part)?;
    let mut reader = resp.body_mut().as_reader();
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    let (mut done, mut reported) = (0u64, 0u64);
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                let _ = std::fs::remove_file(&part);
                return Err(anyhow::Error::from(e).context(format!("reading {rel}")));
            }
        };
        hasher.update(&buf[..n]);
        if let Err(e) = out.write_all(&buf[..n]) {
            let _ = std::fs::remove_file(&part);
            return Err(anyhow::Error::from(e).context(format!("writing {rel}")));
        }
        done += n as u64;
        // The weights are the better part of a gigabit; silence for that long
        // reads as a hang.
        if total > 0 && done * 10 / total > reported {
            reported = done * 10 / total;
            note(&format!("{rel}: {}%", reported * 10));
        }
    }
    out.flush()?;
    drop(out);

    let got = hex::encode(hasher.finalize());
    if got != want {
        let _ = std::fs::remove_file(&part);
        // Truncation, a captive-portal login page, or a mirror serving
        // something else entirely all land here rather than as an inscrutable
        // tensor error several seconds later.
        anyhow::bail!(
            "{rel} does not match its published checksum\n  expected {want}\n  got      {got}"
        );
    }
    std::fs::rename(&part, &dest)?;
    Ok(dest)
}

/// Precomputed voice state. Deriving it from audio every time would add an
/// encoder pass for no benefit.
fn embed_voice<Q: xn::BackendQ<B = xn::CpuDevice>>(
    path: &std::path::Path,
    dev: &Q::B,
) -> Result<Tensor<Q::T, Q::B>> {
    let vb = VB::load(&[path], *dev)?;
    let names = vb.tensor_names();
    let key = names.first().context("voice file has no tensors")?;
    let shape = vb.shape(key).context("voice tensor")?;
    let dims = shape.dims().to_vec();
    let emb: Tensor<f32, Q::B> = vb.tensor(key, shape)?;
    let emb = if dims.len() == 2 {
        emb.reshape((1, dims[0], dims[1]))?
    } else {
        emb
    };
    Ok(emb.to::<Q::T>()?)
}

fn load_voice<Q: xn::BackendQ<B = xn::CpuDevice>>(
    name: &str,
    dev: &Q::B,
) -> Result<Tensor<Q::T, Q::B>> {
    if !VOICES.contains(&name) {
        anyhow::bail!("unknown voice '{name}'; available: {}", VOICES.join(", "));
    }
    embed_voice::<Q>(&fetch(&format!("embeddings/{name}.safetensors"))?, dev)
}

fn main() -> Result<()> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv
        .iter()
        .any(|a| a == "--version" || a == "-v" || a == "-V")
    {
        println!("kobold-tts {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if argv.iter().any(|a| a == "--help" || a == "-h") {
        eprintln!("Usage: kobold-tts [OPTIONS]");
        eprintln!("Streaming Pocket-TTS local neural voice engine.\n");
        eprintln!("Options:");
        eprintln!("      --voice <NAME>     Voice preset (alba, marius, javert, jean, etc.)");
        eprintln!("      --quant <QUANT>    Quantization (none, q8, q8k, q6k; default: q8)");
        eprintln!("      --wav <PATH>       Write output to WAV file instead of playing");
        eprintln!("      --max-frames <N>   Max audio frames (default: 2000)");
        eprintln!("      --buffer-ms <N>    Audio buffer in milliseconds");
        eprintln!("  -V, --version          Print version");
        eprintln!("  -h, --help             Print help");
        return Ok(());
    }
    let args = Args::parse();
    match args.quant.as_str() {
        "none" | "f32" => run::<xn::Unquantized<f32, _>>(args),
        "q8" | "q8_0" => run::<xn::quantized::Q80F32>(args),
        "q8k" => run::<xn::quantized::Q8kF32>(args),
        "q6k" => run::<xn::quantized::Q6kF32>(args),
        other => anyhow::bail!("unknown --quant '{other}' (none, q8, q8k, q6k)"),
    }
}

fn run<Q: xn::BackendQ<B = xn::CpuDevice> + 'static>(args: Args) -> Result<()> {
    let dev = xn::CPU;
    let (weights, tokenizer_path, voice_path) = download(&args.voice)?;

    let sp = sentencepiece::SentencePieceProcessor::open(
        tokenizer_path.to_str().context("tokenizer path")?,
    )?;
    let tokenizer = SpTokenizer(sp);
    let encoder = sentencepiece::SentencePieceProcessor::open(
        tokenizer_path.to_str().context("tokenizer path")?,
    )?;
    let cfg = TTSConfig::v202601(TEMPERATURE);

    note("loading model");
    let vb = VB::load_with_key_map(&[&weights], dev, remap_key)?;
    let vb = vb.root();
    // Shared with the decoder thread. TTSModel is not Clone, and the weights
    // are large enough that copying them per clause would be absurd anyway.
    let model: Arc<TTSModel<Q>> = Arc::new(TTSModel::load(&vb, Box::new(tokenizer), &cfg)?);

    let voice_emb = embed_voice::<Q>(&voice_path, &dev)?;

    let sample_rate = model.sample_rate();
    let sink = audio::Sink::new(sample_rate, args.wav.as_deref(), args.buffer_ms)?;
    note(&format!("ready ({} Hz, quant {})", sample_rate, args.quant));

    let mut voice_emb = voice_emb;
    for line in std::io::stdin().lock().lines() {
        let text = line?;
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        // Control line rather than a restart: switching voice only needs a new
        // embedding, a few hundred KB, against a model load measured in
        // seconds.
        if let Some(name) = text.strip_prefix(":voice ") {
            match load_voice::<Q>(name.trim(), &dev) {
                Ok(v) => {
                    voice_emb = v;
                    note(&format!("voice {}", name.trim()));
                }
                Err(e) => note(&format!("voice: {e}")),
            }
            continue;
        }
        // Normalisation is not cosmetic: this capitalises, appends a full stop
        // when the clause ends on a word, and pads very short text. Feeding raw
        // text instead makes the model produce a garbled, repeating tail.
        let (normalized, frames_after_eos) = prepare_text_prompt(&to_ascii_punct(text));
        let ids: Vec<u32> = encoder
            .encode(&normalized)
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .into_iter()
            .map(|p| p.id)
            .collect();
        speak(
            &model,
            &voice_emb,
            &cfg,
            &dev,
            &ids,
            frames_after_eos,
            args.max_frames,
            &sink,
        )?;
    }
    sink.finish()
}

// Same as the loop in kobold: the arguments are the pieces of one synthesis
// run, and bundling them into a struct would name a thing that does not exist.
#[allow(clippy::too_many_arguments)]
fn speak<Q: xn::BackendQ + 'static>(
    model: &Arc<TTSModel<Q>>,
    voice_emb: &Tensor<Q::T, Q::B>,
    cfg: &TTSConfig,
    dev: &Q::B,
    tokens: &[u32],
    frames_after_eos: usize,
    max_frames: usize,
    sink: &audio::Sink,
) -> Result<()> {
    let started = std::time::Instant::now();
    let mut state = model.init_flow_lm_state(1, tokens.len() + max_frames)?;
    model.prompt_audio(&mut state, voice_emb)?;
    model.prompt_text(&mut state, tokens)?;

    let mut mimi_state = model.init_mimi_state(1, 250)?;
    let (latent_tx, latent_rx) = mpsc::channel::<Tensor<Q::T, Q::B>>();
    let done = Arc::new(AtomicBool::new(false));

    // Mimi decodes on its own thread so the flow model never blocks on it.
    // Cloned by value: the thread outlives this borrow, and TTSModel clones
    // share the weights rather than copying them.
    let decoder = std::thread::spawn({
        let model = Arc::clone(model);
        let sink = sink.clone();
        move || -> Result<()> {
            let mut first = false;
            while let Ok(latent) = latent_rx.recv() {
                let pcm = model.decode_latent(&latent, &mut mimi_state)?;
                sink.push_timed(&pcm.to_vec()?, started, &mut first)?;
            }
            Ok(())
        }
    });

    let ldim = cfg.flow_lm.ldim;
    let mut prev: Tensor<Q::T, Q::B> =
        Tensor::from_vec(vec![f32::NAN; ldim], (1, 1, ldim), dev)?.to::<Q::T>()?;
    let mut rng = Gaussian::new(TEMPERATURE.sqrt(), 0x5eed_1234);

    // EOS is not the last frame worth decoding: the model needs a few more to
    // let the utterance tail off. Stopping the instant EOS fires clips the end
    // of the last word.
    let mut countdown: Option<usize> = None;
    for _ in 0..max_frames {
        let (next, eos) = model.generate_step(&mut state, &prev, &mut rng)?;
        latent_tx.send(next.clone())?;
        prev = next;

        if eos && countdown.is_none() {
            countdown = Some(frames_after_eos);
        }
        if let Some(left) = countdown.as_mut() {
            if *left == 0 {
                done.store(true, Ordering::Relaxed);
                break;
            }
            *left -= 1;
        }
    }
    drop(latent_tx);
    decoder
        .join()
        .map_err(|_| anyhow::anyhow!("decoder thread panicked"))??;
    Ok(())
}

mod audio;

/// Gaussian noise for the flow sampler.
///
/// Hand-rolled rather than pulling `rand` + `rand_distr` for one distribution:
/// xorshift plus Box-Muller is exact enough here and keeps the dependency list
/// honest. Seeded, so a run is reproducible.
struct Gaussian {
    state: u64,
    std: f32,
    spare: Option<f32>,
}

impl Gaussian {
    fn new(std: f32, seed: u64) -> Self {
        Self {
            state: seed | 1,
            std,
            spare: None,
        }
    }

    fn uniform(&mut self) -> f32 {
        // xorshift64*
        self.state ^= self.state >> 12;
        self.state ^= self.state << 25;
        self.state ^= self.state >> 27;
        let v = self.state.wrapping_mul(0x2545_F491_4F6C_DD1D);
        // Open interval: Box-Muller takes ln(u), so u must never be zero.
        ((v >> 11) as f32 + 0.5) / (1u64 << 53) as f32
    }
}

impl ptts::flow_lm::Rng for Gaussian {
    fn sample(&mut self) -> f32 {
        if let Some(v) = self.spare.take() {
            return v * self.std;
        }
        let (u1, u2) = (self.uniform(), self.uniform());
        let r = (-2.0 * u1.ln()).sqrt();
        let (s, c) = (std::f32::consts::TAU * u2).sin_cos();
        self.spare = Some(r * c);
        r * s * self.std
    }
}

#[cfg(test)]
mod tests {
    use super::to_ascii_punct;

    #[test]
    fn folds_typographic_punctuation() {
        assert_eq!(
            to_ascii_punct("That\u{2019}s \u{201C}fine\u{201D}"),
            "That's \"fine\""
        );
        assert_eq!(to_ascii_punct("wait\u{2014}then go"), "wait-then go");
        assert_eq!(to_ascii_punct("hmm\u{2026}"), "hmm...");
        assert_eq!(to_ascii_punct("a\u{00A0}b"), "a b");
    }

    #[test]
    fn leaves_ordinary_text_alone() {
        let text = "It's a \"test\", 1-2-3.";
        assert_eq!(to_ascii_punct(text), text);
    }
}
