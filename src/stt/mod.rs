//! Speech-to-text. M4.3: whisper.cpp (via whisper-rs) running on a worker
//! thread so STT latency never blocks the UI.

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

pub use whisper::{find_default_model, WhisperStt};
