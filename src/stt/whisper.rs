//! whisper.cpp-backed STT via the `whisper-rs` crate.

use super::SttEngine;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

pub struct WhisperStt {
    ctx: WhisperContext,
    name: String,
}

impl WhisperStt {
    pub fn new(model_path: &Path) -> Result<Self> {
        check_cpu_features()?;
        let model_str = model_path
            .to_str()
            .context("model path is not valid UTF-8")?;
        let ctx = WhisperContext::new_with_params(model_str, WhisperContextParameters::default())
            .with_context(|| format!("load whisper model from {}", model_path.display()))?;
        let name = model_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("whisper")
            .to_string();
        Ok(Self { ctx, name })
    }
}

// Portable builds target x86-64-v3 (AVX2 + FMA + F16C). A CPU that lacks any
// of those would hit EXCEPTION_ILLEGAL_INSTRUCTION inside whisper_full() and
// the OS would kill the process with no Rust panic — a silent crash that's
// nearly impossible to triage in the field. Fail early with a clear message.
#[cfg(target_arch = "x86_64")]
fn check_cpu_features() -> Result<()> {
    let avx2 = std::is_x86_feature_detected!("avx2");
    let fma = std::is_x86_feature_detected!("fma");
    let f16c = std::is_x86_feature_detected!("f16c");
    let avx512f = std::is_x86_feature_detected!("avx512f");
    eprintln!(
        "[stt] cpu features: avx2={} fma={} f16c={} avx512f={}",
        yn(avx2),
        yn(fma),
        yn(f16c),
        yn(avx512f)
    );
    let mut missing: Vec<&str> = Vec::new();
    if !avx2 {
        missing.push("AVX2");
    }
    if !fma {
        missing.push("FMA");
    }
    if !f16c {
        missing.push("F16C");
    }
    if !missing.is_empty() {
        anyhow::bail!(
            "STT requires AVX2 + FMA + F16C; this CPU lacks {}. \
             Use a CPU from 2014 (Haswell) or newer.",
            missing.join(" + ")
        );
    }
    Ok(())
}

#[cfg(not(target_arch = "x86_64"))]
fn check_cpu_features() -> Result<()> {
    Ok(())
}

fn yn(b: bool) -> &'static str {
    if b {
        "yes"
    } else {
        "no"
    }
}

impl SttEngine for WhisperStt {
    fn transcribe(&self, pcm: &[f32]) -> Result<String> {
        if pcm.is_empty() {
            return Ok(String::new());
        }
        const TARGET_RATE: usize = 16_000;

        // Whisper hallucinates "you" / "thank you" / "thanks for watching"
        // on silent or near-silent audio — its YouTube training data
        // pre-loads those tokens hard. The previous behaviour padded
        // sub-1-second buffers with zeros, which was a direct trigger.
        //
        // Two-stage gate: RMS energy floor catches dead-quiet captures;
        // a minimum-duration check stops "you" from popping out of a
        // 100 ms accidental PTT tap. Both produce an empty transcript
        // (which the dispatcher already renders as "(no speech)").
        let rms = compute_rms(pcm);
        const RMS_FLOOR: f32 = 0.008; // matches the VAD chunker's threshold
        const MIN_SAMPLES: usize = TARGET_RATE / 2; // 0.5 s
        if rms < RMS_FLOOR {
            eprintln!(
                "[stt] dropped silent buffer ({:.2}s, rms {:.4} < {:.4})",
                pcm.len() as f32 / TARGET_RATE as f32,
                rms,
                RMS_FLOOR
            );
            return Ok(String::new());
        }
        if pcm.len() < MIN_SAMPLES {
            eprintln!(
                "[stt] dropped short buffer ({} samples, {:.2}s — below 0.5s minimum)",
                pcm.len(),
                pcm.len() as f32 / TARGET_RATE as f32
            );
            return Ok(String::new());
        }

        // Whisper's positional embedding wants ≥1 s of context. We only
        // pad with silence *after* the energy + duration gates have proven
        // there's real speech in the buffer; padding a clean half-second
        // utterance is fine, padding noise is not.
        let mut padded;
        let input: &[f32] = if pcm.len() < TARGET_RATE {
            padded = Vec::with_capacity(TARGET_RATE);
            padded.extend_from_slice(pcm);
            padded.resize(TARGET_RATE, 0.0);
            &padded
        } else {
            pcm
        };

        let mut state = self
            .ctx
            .create_state()
            .context("create whisper inference state")?;
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        let threads = (std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            / 2)
        .max(1) as i32;
        params.set_n_threads(threads);
        params.set_language(Some("en"));
        params.set_translate(false);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        // Deterministic, conservative decoding: temperature 0 + suppress
        // blank tokens. Both reduce hallucination probability on edge-case
        // audio that still passed the gates above.
        params.set_temperature(0.0);
        params.set_suppress_blank(true);
        state
            .full(params, input)
            .context("whisper full inference")?;

        let n = state.full_n_segments();
        let mut out = String::new();
        for i in 0..n {
            if let Some(seg) = state.get_segment(i) {
                if let Ok(text) = seg.to_str() {
                    out.push_str(text);
                }
            }
        }
        let trimmed = out.trim();
        if is_known_hallucination(trimmed) {
            eprintln!("[stt] dropped known hallucination: {trimmed:?}");
            return Ok(String::new());
        }
        Ok(trimmed.to_string())
    }

    fn name(&self) -> &str {
        &self.name
    }
}

/// Root-mean-square of an f32 PCM buffer in [-1.0, 1.0] sample space.
fn compute_rms(pcm: &[f32]) -> f32 {
    if pcm.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = pcm.iter().map(|s| s * s).sum();
    (sum_sq / pcm.len() as f32).sqrt()
}

/// Whisper's most-common spurious outputs on silent / very-low-energy audio.
/// These are baked in from YouTube transcripts ("Don't forget to like,
/// subscribe, thank YOU") and reliably appear even when energy + duration
/// gates pass — e.g. a single quiet breath sound that's above the RMS floor
/// but below the speech-detection threshold of the model itself.
fn is_known_hallucination(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let stripped = lower
        .trim()
        .trim_end_matches(|c: char| matches!(c, '.' | '!' | '?'))
        .trim();
    const PHRASES: &[&str] = &[
        "you",
        "thank you",
        "thanks for watching",
        "thank you for watching",
        "please subscribe",
        "subscribe to my channel",
        "bye",
        "bye bye",
        "♪",
        "♪ ♪",
    ];
    PHRASES.iter().any(|&p| stripped == p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rms_zero_buffer() {
        let pcm = vec![0.0_f32; 16_000];
        assert_eq!(compute_rms(&pcm), 0.0);
    }

    #[test]
    fn rms_unit_buffer() {
        let pcm = vec![0.5_f32; 1_000];
        // RMS of constant 0.5 is 0.5.
        assert!((compute_rms(&pcm) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn known_hallucinations_caught() {
        assert!(is_known_hallucination("you"));
        assert!(is_known_hallucination("You"));
        assert!(is_known_hallucination("You."));
        assert!(is_known_hallucination(" You "));
        assert!(is_known_hallucination("Thank you"));
        assert!(is_known_hallucination("Thanks for watching"));
        assert!(is_known_hallucination("♪"));
    }

    #[test]
    fn real_speech_passes() {
        assert!(!is_known_hallucination("next page"));
        assert!(!is_known_hallucination("go to AGM-65"));
        assert!(!is_known_hallucination("read it again"));
        // "you" as a substring of a real utterance is fine — only exact-
        // match (after stripping non-alphanumerics) is filtered.
        assert!(!is_known_hallucination("are you ready"));
    }
}

/// Look in the `models/` directory for a usable whisper model. Returns the
/// first match in preference order (better quality → faster fallback). None
/// if nothing is present so the app can render a banner instead of crashing.
pub fn find_default_model() -> Option<PathBuf> {
    const CANDIDATES: &[&str] = &[
        "models/ggml-base.en.bin",
        "models/ggml-small.en.bin",
        "models/ggml-tiny.en.bin",
        "models/ggml-base.bin",
        "models/ggml-tiny.bin",
    ];
    for c in CANDIDATES {
        let p = PathBuf::from(c);
        if p.exists() {
            return Some(p);
        }
    }
    None
}
