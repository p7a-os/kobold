//! Audio output: the speakers, or a WAV file.
//!
//! The WAV path exists so the inference pipeline can be verified on a machine
//! with no sound card -- which is exactly where this was developed.

use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};

/// Extra time to let the audio device finish what it already holds.
const DRAIN_MS: u64 = 300;

/// Audio held ahead of the ear before playback starts, and re-accumulated
/// whenever the queue runs dry.
///
/// Synthesis is faster than real time on average but not uniformly: a clause
/// boundary, a slow frame, or a gap in the text stream will each starve the
/// device. Without a cushion every one of those is an audible dropout. This is
/// the jitter buffer, and it trades a fixed delay before the first word for
/// continuity after it.
pub const DEFAULT_BUFFER_MS: usize = 250;

/// Queue plus the priming flag, under one lock so the callback takes exactly
/// one.
pub struct Queue {
    pcm: std::collections::VecDeque<f32>,
    /// While priming, the callback emits silence and lets the queue fill.
    priming: bool,
    target: usize,
}

#[derive(Clone)]
pub enum Sink {
    /// Ring buffer drained by the audio callback. The model's 24 kHz mono is
    /// converted to whatever the device actually accepts.
    Live {
        buf: Arc<Mutex<Queue>>,
        rs: Arc<Mutex<Resample>>,
        _stream: Arc<StreamHandle>,
    },
    Wav {
        path: String,
        samples: Arc<Mutex<Vec<f32>>>,
        rate: usize,
    },
}

/// cpal streams are not `Send`, and we only need to keep them alive.
pub struct StreamHandle(#[allow(dead_code)] Box<dyn std::any::Any + Send + Sync>);

impl Sink {
    pub fn new(rate: usize, wav: Option<&str>, buffer_ms: usize) -> Result<Self> {
        if let Some(path) = wav {
            return Ok(Sink::Wav {
                path: path.to_owned(),
                samples: Arc::new(Mutex::new(Vec::new())),
                rate,
            });
        }
        Self::live(rate, buffer_ms)
    }

    fn live(rate: usize, buffer_ms: usize) -> Result<Self> {
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

        let device = cpal::default_host()
            .default_output_device()
            .context("no audio output device; use --wav to write a file instead")?;

        // Take the device's own configuration rather than demanding 24 kHz.
        // macOS output devices typically run at 44.1 or 48 kHz and simply
        // refuse anything else, so the model's rate has to be converted here.
        let default = device
            .default_output_config()
            .context("no usable audio output; use --wav to write a file instead")?;
        let out_rate = default.sample_rate() as usize;
        let channels = default.channels().max(1) as usize;
        let config = cpal::StreamConfig {
            channels: channels as cpal::ChannelCount,
            sample_rate: out_rate as cpal::SampleRate,
            buffer_size: cpal::BufferSize::Default,
        };
        if out_rate != rate {
            eprintln!("kobold-tts: device runs at {out_rate} Hz, resampling from {rate} Hz");
        }
        let rs = Arc::new(Mutex::new(Resample::new(rate, out_rate, channels)));
        let target = out_rate * buffer_ms / 1000 * channels;
        let buf = Arc::new(Mutex::new(Queue {
            pcm: std::collections::VecDeque::with_capacity(target * 4),
            priming: true,
            target,
        }));
        let cb_buf = buf.clone();
        let stream = device.build_output_stream(
            config,
            move |out: &mut [f32], _: &cpal::OutputCallbackInfo| {
                let mut q = cb_buf.lock().expect("audio buffer");
                if q.priming {
                    if q.pcm.len() < q.target {
                        out.fill(0.0);
                        return;
                    }
                    q.priming = false;
                }
                for slot in out.iter_mut() {
                    match q.pcm.pop_front() {
                        Some(v) => *slot = v,
                        // Ran dry: emit silence and re-prime, so the next gap
                        // is one pause rather than a stutter.
                        None => {
                            *slot = 0.0;
                            q.priming = true;
                        }
                    }
                }
            },
            |e| eprintln!("kobold-tts: audio stream error: {e}"),
            None,
        )?;
        stream.play()?;
        Ok(Sink::Live {
            buf,
            rs,
            _stream: Arc::new(StreamHandle(Box::new(stream))),
        })
    }

    /// First-chunk timing, printed once per clause. This is the number that
    /// matters -- model throughput says nothing about when speech starts.
    pub fn push_timed(
        &self,
        pcm: &[f32],
        started: std::time::Instant,
        first: &mut bool,
    ) -> Result<()> {
        if !*first {
            *first = true;
            eprintln!(
                "kobold-tts: ttfa {:.0}ms",
                started.elapsed().as_secs_f64() * 1000.0
            );
        }
        self.push(pcm)
    }

    pub fn push(&self, pcm: &[f32]) -> Result<()> {
        match self {
            Sink::Live { buf, rs, .. } => {
                // Resampled outside the audio lock. The callback runs on a
                // real-time thread; if it blocks waiting for this, the device
                // underruns, which is the whole reason a lock-free ring is the
                // textbook answer here.
                let out = rs.lock().expect("resampler").process(pcm);
                let mut q = buf.lock().expect("audio buffer");
                q.pcm.reserve(out.len());
                q.pcm.extend(out);
            }
            Sink::Wav { samples, .. } => samples.lock().expect("wav buffer").extend_from_slice(pcm),
        }
        Ok(())
    }

    pub fn finish(&self) -> Result<()> {
        match self {
            Sink::Live { buf, .. } => {
                // Wait for our queue to empty...
                loop {
                    let mut q = buf.lock().expect("audio buffer");
                    if q.pcm.is_empty() {
                        break;
                    }
                    // The tail is shorter than the priming target, so let it
                    // play rather than waiting for a fill that never comes.
                    q.priming = false;
                    drop(q);
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                // ...then keep the stream alive a little longer. An empty queue
                // only means the callback has *taken* the samples, not that the
                // device has played them. Exiting here drops the stream with
                // audio still in the driver's buffer, and CoreAudio repeats its
                // last buffer when that happens -- which sounds like the final
                // syllable stuttering.
                std::thread::sleep(std::time::Duration::from_millis(DRAIN_MS));
                Ok(())
            }
            Sink::Wav {
                path,
                samples,
                rate,
            } => {
                let s = samples.lock().expect("wav buffer");
                write_wav(path, &s, *rate)?;
                // Said out loud: with --wav the process just exits quietly,
                // which is indistinguishable from having done nothing.
                eprintln!(
                    "kobold-tts: wrote {path} ({:.2}s, {} Hz) — play it with afplay/aplay",
                    s.len() as f64 / *rate as f64,
                    rate
                );
                Ok(())
            }
        }
    }
}

/// Linear-interpolation resampler with continuity across chunks.
///
/// Linear rather than windowed sinc: speech at 24 kHz has little energy near
/// Nyquist, so the aliasing it introduces is inaudible here, and it costs a
/// multiply per sample against a budget measured in tens of milliseconds. The
/// fractional position and previous sample persist between calls -- resetting
/// them per chunk would put a click at every clause boundary.
pub struct Resample {
    step: f64,
    pos: f64,
    prev: f32,
    channels: usize,
    passthrough: bool,
}

impl Resample {
    fn new(from: usize, to: usize, channels: usize) -> Self {
        Self {
            step: from as f64 / to as f64,
            pos: 0.0,
            prev: 0.0,
            channels,
            passthrough: from == to && channels == 1,
        }
    }

    /// `pos` is a position in input-sample space: 0.0 is `input[0]`, and a
    /// negative value refers back into the previous chunk via `prev`.
    fn process(&mut self, input: &[f32]) -> Vec<f32> {
        if input.is_empty() {
            return Vec::new();
        }
        if self.passthrough {
            return input.to_vec();
        }
        let last = (input.len() - 1) as f64;
        let mut out = Vec::with_capacity(
            (input.len() as f64 / self.step).ceil() as usize * self.channels + 4,
        );

        while self.pos <= last {
            let i = self.pos.floor() as i64;
            let frac = (self.pos - i as f64) as f32;
            let a = if i < 0 { self.prev } else { input[i as usize] };
            let bi = (i + 1).max(0) as usize;
            let b = if bi < input.len() { input[bi] } else { a };
            let v = a + (b - a) * frac;
            // Mono duplicated across channels; the model has no stereo image.
            for _ in 0..self.channels {
                out.push(v);
            }
            self.pos += self.step;
        }
        // Carry the remainder into the next chunk so no click appears at a
        // clause boundary.
        self.pos -= input.len() as f64;
        self.prev = input[input.len() - 1];
        out
    }
}

/// 16-bit PCM WAV. Hand-written rather than pulling a crate for 44 bytes of
/// header.
fn write_wav(path: &str, samples: &[f32], rate: usize) -> Result<()> {
    let mut out = Vec::with_capacity(44 + samples.len() * 2);
    let data_len = (samples.len() * 2) as u32;
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&(rate as u32).to_le_bytes());
    out.extend_from_slice(&((rate * 2) as u32).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
    }
    std::fs::write(path, out)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resample_passthrough_is_exact() {
        let mut r = Resample::new(24000, 24000, 1);
        assert_eq!(r.process(&[0.1, 0.2, 0.3]), vec![0.1, 0.2, 0.3]);
    }

    #[test]
    fn resample_doubles_sample_count_for_double_rate() {
        let mut r = Resample::new(24000, 48000, 1);
        let out = r.process(&[0.0; 100]);
        // Within one sample: the fractional position carries between chunks.
        assert!((out.len() as i64 - 200).abs() <= 1, "got {}", out.len());
    }

    #[test]
    fn resample_is_continuous_across_chunks() {
        // Two halves of one ramp must resample to the same thing as the whole.
        let ramp: Vec<f32> = (0..200).map(|i| i as f32 / 200.0).collect();
        let mut whole = Resample::new(24000, 44100, 1);
        let a = whole.process(&ramp);
        let mut split = Resample::new(24000, 44100, 1);
        let mut b = split.process(&ramp[..100]);
        b.extend(split.process(&ramp[100..]));
        assert_eq!(a.len(), b.len(), "chunking changed the output length");
        for (x, y) in a.iter().zip(&b) {
            assert!((x - y).abs() < 1e-6, "discontinuity at a clause boundary");
        }
    }

    #[test]
    fn resample_duplicates_mono_across_channels() {
        let mut r = Resample::new(24000, 24000, 2);
        let out = r.process(&[0.5, -0.5]);
        assert_eq!(out, vec![0.5, 0.5, -0.5, -0.5]);
    }

    #[test]
    fn wav_header_is_well_formed() {
        let path = std::env::temp_dir().join("kobold-tts-test.wav");
        let p = path.to_str().unwrap();
        write_wav(p, &[0.0, 0.5, -0.5, 1.0], 24000).unwrap();
        let bytes = std::fs::read(p).unwrap();
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(bytes.len(), 44 + 4 * 2);
        // Full scale must not wrap to negative.
        let last = i16::from_le_bytes([bytes[50], bytes[51]]);
        assert_eq!(last, 32767);
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn wav_clamps_out_of_range_samples() {
        let path = std::env::temp_dir().join("kobold-tts-clamp.wav");
        let p = path.to_str().unwrap();
        write_wav(p, &[9.0, -9.0], 24000).unwrap();
        let b = std::fs::read(p).unwrap();
        assert_eq!(i16::from_le_bytes([b[44], b[45]]), 32767);
        assert_eq!(i16::from_le_bytes([b[46], b[47]]), -32767);
        std::fs::remove_file(p).ok();
    }
}
