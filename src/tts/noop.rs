//! Non-Windows fallback. Logs what would be spoken; produces no audio.

use super::TtsEngine;
use anyhow::Result;

pub struct NoopTts;

impl NoopTts {
    pub fn new() -> Result<Self> {
        Ok(Self)
    }
}

impl TtsEngine for NoopTts {
    fn speak(&mut self, text: &str, _interrupt: bool) -> Result<()> {
        eprintln!("[noop tts] would speak: {}", text);
        Ok(())
    }
    fn stop(&mut self) -> Result<()> {
        Ok(())
    }
    fn name(&self) -> &'static str {
        "noop"
    }
}
