//! User-tunable runtime settings persisted to `settings.toml`.
//!
//! M3.5 surface: read/advance behavior toggles. The bindings table in SPEC §7.5
//! will land here too once the action→trigger system is built out in M4.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default = "Settings::default_auto_read")]
    pub auto_read_on_next: bool,

    #[serde(default = "Settings::default_auto_advance")]
    pub auto_advance: bool,

    /// Pause between end-of-speech (estimated) and stepping to the next item,
    /// in seconds. The auto-advance timer waits `est_speech_ms + this`.
    #[serde(default = "Settings::default_delay")]
    pub advance_delay_sec: f32,

    /// When reading a step, also read any immediately-following supporting
    /// notes (note-info, +>, ?>, !>, !!>, @>) as part of the same utterance.
    #[serde(default = "Settings::default_read_notes")]
    pub read_notes: bool,

    /// Last-known window position in physical pixels. None on first run.
    #[serde(default)]
    pub window_x: Option<i32>,
    #[serde(default)]
    pub window_y: Option<i32>,

    /// Aircraft module id (matches DCS folder names: F-16C_50, FA-18C_hornet, ...).
    /// Used by tab sources that resolve `{aircraft}` in their paths and by the
    /// DCS kneeboards source. None on first run → fall back to first aircraft
    /// defined in config.toml.
    #[serde(default)]
    pub current_aircraft: Option<String>,

    /// Last-active tab id so reopening the app picks up where you left off.
    #[serde(default)]
    pub last_tab: Option<String>,
}

impl Settings {
    fn default_auto_read() -> bool { true }   // most users expect Next to also speak
    fn default_auto_advance() -> bool { false }
    fn default_delay() -> f32 { 1.5 }
    fn default_read_notes() -> bool { false }

    pub fn load_or_default(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => match toml::from_str::<Self>(&text) {
                Ok(s) => {
                    eprintln!("[settings] loaded from {}", path.display());
                    s
                }
                Err(e) => {
                    eprintln!("[settings] {} parse failed: {e}", path.display());
                    Self::default()
                }
            },
            Err(_) => {
                eprintln!("[settings] no {} — using defaults", path.display());
                Self::default()
            }
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let text = toml::to_string_pretty(self)?;
        std::fs::write(path, text)?;
        Ok(())
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            auto_read_on_next: Self::default_auto_read(),
            auto_advance: Self::default_auto_advance(),
            advance_delay_sec: Self::default_delay(),
            read_notes: Self::default_read_notes(),
            window_x: None,
            window_y: None,
            current_aircraft: None,
            last_tab: None,
        }
    }
}

pub fn settings_path() -> PathBuf {
    PathBuf::from("settings.toml")
}
