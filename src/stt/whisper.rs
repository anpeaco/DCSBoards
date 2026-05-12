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
        // Whisper needs >= 1 second of audio to produce reliable output;
        // shorter buffers tend to come back empty. Pad with silence so the
        // user can still test with brief utterances.
        const TARGET_RATE: usize = 16_000;
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
        Ok(out.trim().to_string())
    }

    fn name(&self) -> &str {
        &self.name
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
