//! Speech-to-text. M4.3: whisper.cpp (via whisper-rs) running on a worker
//! thread so STT latency never blocks the UI.
//!
//! The trait and the `SttCommand` control message compile
//! unconditionally so `main.rs` can hold them in struct fields and
//! channels even when the `whisper-stt` feature is off (#27). The
//! whisper-rs-backed implementation is gated below.

// Without `whisper-stt` the trait + SttCommand payloads have no
// in-crate consumers (only the channel type itself does, and that
// just needs the names to exist). Suppress the dead-code lint at the
// file level so a no-features `cargo clippy -D warnings` stays clean.
// With the feature on these are used by `WhisperStt` and dispatch.
#![allow(dead_code)]

#[cfg(feature = "whisper-stt")]
pub mod whisper;

use anyhow::Result;

pub trait SttEngine: Send {
    /// Transcribe 16 kHz mono f32 PCM. Returns the recognised text, trimmed.
    fn transcribe(&self, pcm: &[f32]) -> Result<String>;
    fn name(&self) -> &str;
    /// Set the decoder's initial-prompt string. The string biases token
    /// selection toward its contents (issue #15) — used to surface
    /// domain-specific vocabulary the base model under-weights. `None`
    /// clears the prompt. Implementations may no-op if biasing isn't
    /// available on their backend.
    fn set_initial_prompt(&self, prompt: Option<String>);
}

/// Message passed from the UI thread to the STT worker. PCM is the
/// hot path; `SetInitialPrompt` is the issue #15 control channel —
/// piggy-backed on the same mpsc so we don't need a second
/// `recv_timeout` loop on the worker.
#[derive(Debug)]
pub enum SttCommand {
    Pcm(Vec<f32>),
    SetInitialPrompt(Option<String>),
}

#[cfg(feature = "whisper-stt")]
pub use whisper::{find_default_model, WhisperStt};
