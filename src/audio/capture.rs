//! cpal-backed microphone capture.
//!
//! Holds a long-lived input stream so PTT has no warm-up cost; the cpal
//! callback only accumulates samples while `enabled` is set. On stop we
//! downmix to mono and resample to 16 kHz so STT (whisper) gets the format
//! it expects.
//!
//! Threading: cpal invokes the data callback on its audio thread. The
//! callback briefly acquires a `Mutex<Vec<f32>>` to append samples; this is
//! the common pattern and contention is negligible at typical 5–20 ms cpal
//! buffer sizes. If we see dropouts under load we'd swap in a lock-free
//! ringbuf.

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, SampleFormat, Stream};
use rubato::{FftFixedInOut, Resampler};
use std::sync::{Arc, Mutex};

pub const TARGET_RATE: u32 = 16_000;

#[derive(Default)]
struct RecordingState {
    enabled: bool,
    /// Interleaved samples at the device's native sample rate.
    samples: Vec<f32>,
}

pub struct AudioCapture {
    /// Held for the lifetime of the capture so the stream keeps running.
    _stream: Stream,
    state: Arc<Mutex<RecordingState>>,
    input_sample_rate: u32,
    input_channels: u16,
    input_name: String,
}

impl AudioCapture {
    pub fn input_name(&self) -> &str {
        &self.input_name
    }

    /// Begin accumulating audio. Clears any prior buffer.
    pub fn start(&self) {
        let mut st = self.state.lock().unwrap();
        st.enabled = true;
        st.samples.clear();
    }

    /// Stop accumulating and return 16 kHz mono f32 PCM. Empty if the user
    /// released PTT before any audio was captured.
    pub fn stop(&self) -> Result<Vec<f32>> {
        let raw = {
            let mut st = self.state.lock().unwrap();
            st.enabled = false;
            std::mem::take(&mut st.samples)
        };
        if raw.is_empty() {
            return Ok(Vec::new());
        }
        let mono = downmix_mono(&raw, self.input_channels as usize);
        if self.input_sample_rate == TARGET_RATE {
            return Ok(mono);
        }
        resample_to_16k(mono, self.input_sample_rate)
    }

    /// Throw away anything currently in the buffer without stopping
    /// capture. Used while TTS is playing so speaker bleed never reaches
    /// the STT engine.
    pub fn discard_pending(&self) {
        let mut st = self.state.lock().unwrap();
        if !st.samples.is_empty() {
            st.samples.clear();
        }
    }

    /// While capturing, check if a complete utterance is sitting in the
    /// buffer. Returns Some(16 kHz mono PCM) when a speech run is followed
    /// by ~600 ms of silence, and the speech itself was at least ~200 ms.
    /// Returns None while the user is mid-utterance or silent.
    ///
    /// Used by hot-mic mode to drive continuous voice commands without
    /// the user having to toggle the mic for each one.
    pub fn try_take_utterance(&self) -> Result<Option<Vec<f32>>> {
        let channels = self.input_channels as usize;
        let chunk: Vec<f32> = {
            let mut st = self.state.lock().unwrap();
            if !st.enabled {
                return Ok(None);
            }
            let Some(cutoff) = find_utterance_boundary(
                &st.samples,
                self.input_sample_rate,
                channels,
            ) else {
                return Ok(None);
            };
            st.samples.drain(..cutoff).collect()
        };
        if chunk.is_empty() {
            return Ok(None);
        }
        let mono = downmix_mono(&chunk, channels);
        if self.input_sample_rate == TARGET_RATE {
            return Ok(Some(mono));
        }
        resample_to_16k(mono, self.input_sample_rate).map(Some)
    }
}

/// Find a frame boundary in `samples` where a chunk of speech has been
/// followed by ~350 ms of silence. Returns the index (in interleaved
/// samples) at the *end* of that silence so the caller can drain up to
/// there. None if the buffer is still mid-utterance or silent.
///
/// Tuning notes: single-word commands like "next" run ~250–400 ms with a
/// tight onset, so MIN_SPEECH_FRAMES has to stay short or the first word
/// gets bundled with the next one. RMS_THRESHOLD is intentionally low (~−42
/// dBFS) so quiet speakers and trailing aspirate sounds don't get clipped.
fn find_utterance_boundary(samples: &[f32], rate: u32, channels: usize) -> Option<usize> {
    // 20 ms analysis frames. At 48 kHz mono that's 960 samples per frame.
    let frame_samples = ((rate as usize / 50).max(1)) * channels;
    if samples.len() < frame_samples * 4 {
        return None; // not enough audio for a meaningful decision yet
    }
    const RMS_THRESHOLD: f32 = 0.008;     // ~-42 dBFS — sensitive enough for quiet voices
    const SILENCE_FRAMES_END: usize = 18; // 18 * 20 ms = 360 ms trailing silence
    const MIN_SPEECH_FRAMES: usize = 4;   // 80 ms of speech is enough — covers "go", "OK"
    // Cap the chunk size so a constant noise floor that nudges above
    // threshold can't accumulate indefinitely; ship periodically anyway.
    const MAX_UTTERANCE_FRAMES: usize = 250; // 5 s

    let n_frames = samples.len() / frame_samples;
    let mut first_speech: Option<usize> = None;
    let mut speech_count = 0usize;
    let mut silence_run = 0usize;

    for fi in 0..n_frames {
        let frame = &samples[fi * frame_samples..(fi + 1) * frame_samples];
        let sum_sq: f32 = frame.iter().map(|s| s * s).sum();
        let rms = (sum_sq / frame.len() as f32).sqrt();

        if rms >= RMS_THRESHOLD {
            if first_speech.is_none() {
                first_speech = Some(fi);
            }
            speech_count += 1;
            silence_run = 0;
        } else if first_speech.is_some() {
            silence_run += 1;
            if silence_run >= SILENCE_FRAMES_END && speech_count >= MIN_SPEECH_FRAMES {
                let cutoff = (fi + 1) * frame_samples;
                return Some(cutoff.min(samples.len()));
            }
        }

        // Hard cap: if the speaker hasn't paused in 5 s, ship what we have
        // so STT gets a chance to transcribe before the buffer balloons.
        if let Some(start) = first_speech {
            if speech_count >= MIN_SPEECH_FRAMES && fi - start >= MAX_UTTERANCE_FRAMES {
                let cutoff = (fi + 1) * frame_samples;
                return Some(cutoff.min(samples.len()));
            }
        }
    }
    None
}

/// List every input device the default host can see. Names come back in the
/// same form `Device::name()` reports, which is what `open_named` matches on.
pub fn enumerate_inputs() -> Vec<String> {
    let host = cpal::default_host();
    let default_name = host
        .default_input_device()
        .and_then(|d| d.name().ok());
    let mut out = Vec::new();
    if let Ok(devices) = host.input_devices() {
        for d in devices {
            if let Ok(name) = d.name() {
                out.push(name);
            }
        }
    }
    // Float the default to the top so it's the first thing the user sees.
    if let Some(default_name) = default_name {
        if let Some(pos) = out.iter().position(|n| n == &default_name) {
            let n = out.remove(pos);
            out.insert(0, n);
        }
    }
    out
}

/// Open the system's default input device and return a started capture
/// handle. Returns Err if the host has no input device or the configured
/// sample format isn't one we support.
pub fn open_default() -> Result<AudioCapture> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .context("no default audio input device")?;
    open_with(device)
}

/// Open a specific input device by name. Falls back to the default if no
/// device matches.
pub fn open_named(target: &str) -> Result<AudioCapture> {
    let host = cpal::default_host();
    if let Ok(devices) = host.input_devices() {
        for d in devices {
            if d.name().ok().as_deref() == Some(target) {
                return open_with(d);
            }
        }
    }
    eprintln!("[audio] device '{target}' not found, falling back to default");
    open_default()
}

fn open_with(device: Device) -> Result<AudioCapture> {
    let input_name = device.name().unwrap_or_else(|_| "<unknown>".into());
    let supported = device
        .default_input_config()
        .context("query default input config")?;
    let sample_rate = supported.sample_rate().0;
    let channels = supported.channels();
    let format = supported.sample_format();
    eprintln!(
        "[audio] input device: {input_name} | {sample_rate} Hz | {channels} ch | {format:?}"
    );

    let state = Arc::new(Mutex::new(RecordingState::default()));
    let err_fn = |e| eprintln!("[audio] stream error: {e}");
    let config: cpal::StreamConfig = supported.config();

    let stream = match format {
        SampleFormat::F32 => {
            let state = state.clone();
            device.build_input_stream(
                &config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    let mut st = state.lock().unwrap();
                    if st.enabled {
                        st.samples.extend_from_slice(data);
                    }
                },
                err_fn,
                None,
            )?
        }
        SampleFormat::I16 => {
            let state = state.clone();
            device.build_input_stream(
                &config,
                move |data: &[i16], _: &cpal::InputCallbackInfo| {
                    let mut st = state.lock().unwrap();
                    if st.enabled {
                        st.samples
                            .extend(data.iter().map(|&s| s as f32 / i16::MAX as f32));
                    }
                },
                err_fn,
                None,
            )?
        }
        SampleFormat::U16 => {
            let state = state.clone();
            device.build_input_stream(
                &config,
                move |data: &[u16], _: &cpal::InputCallbackInfo| {
                    let mut st = state.lock().unwrap();
                    if st.enabled {
                        st.samples.extend(
                            data.iter()
                                .map(|&s| (s as f32 - u16::MAX as f32 / 2.0) / (u16::MAX as f32 / 2.0)),
                        );
                    }
                },
                err_fn,
                None,
            )?
        }
        other => anyhow::bail!("unsupported sample format: {other:?}"),
    };
    stream.play().context("start input stream")?;

    Ok(AudioCapture {
        _stream: stream,
        state,
        input_sample_rate: sample_rate,
        input_channels: channels,
        input_name,
    })
}

/// Average interleaved channels into a single mono channel. No-op for mono input.
fn downmix_mono(samples: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return samples.to_vec();
    }
    samples
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect()
}

/// FFT-based resampler. FftFixedInOut works in fixed-size blocks with built-in
/// anti-aliasing — fine for speech and avoids the aliasing of naïve linear
/// interp. We pad the tail with zeros so the final partial block doesn't get
/// dropped.
fn resample_to_16k(input: Vec<f32>, from: u32) -> Result<Vec<f32>> {
    // Block size: 1024 input frames is a good balance of resampler cost and
    // tail-padding overhead at typical rates (44.1k/48k).
    let chunk_in = 1024usize;
    let mut resampler = FftFixedInOut::<f32>::new(
        from as usize,
        TARGET_RATE as usize,
        chunk_in,
        1, // mono
    )
    .context("init resampler")?;

    let actual_in = resampler.input_frames_next();
    let mut out: Vec<f32> = Vec::with_capacity(
        (input.len() as f64 * TARGET_RATE as f64 / from as f64) as usize + actual_in,
    );

    let mut pos = 0;
    while pos + actual_in <= input.len() {
        let chunk = &input[pos..pos + actual_in];
        let processed = resampler.process(&[chunk], None).context("resampler block")?;
        out.extend_from_slice(&processed[0]);
        pos += actual_in;
    }

    // Tail: pad to a full block and drop the synthetic output proportional to
    // the padding ratio so the result roughly matches input duration.
    if pos < input.len() {
        let real = input.len() - pos;
        let mut tail = Vec::with_capacity(actual_in);
        tail.extend_from_slice(&input[pos..]);
        tail.resize(actual_in, 0.0);
        let processed = resampler.process(&[tail.as_slice()], None).context("resampler tail")?;
        let keep = (real as f64 * TARGET_RATE as f64 / from as f64).round() as usize;
        out.extend_from_slice(&processed[0][..keep.min(processed[0].len())]);
    }

    Ok(out)
}
