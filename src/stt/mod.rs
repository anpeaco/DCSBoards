//! Speech-to-text. M4.3: whisper.cpp (via whisper-rs) running on a worker
//! thread so STT latency never blocks the UI.

pub mod whisper;

use anyhow::Result;

pub trait SttEngine: Send {
    /// Transcribe 16 kHz mono f32 PCM. Returns the recognised text, trimmed.
    fn transcribe(&self, pcm: &[f32]) -> Result<String>;
    fn name(&self) -> &str;
}

pub use whisper::{find_default_model, WhisperStt};
