//! User-tunable runtime settings persisted to `settings.toml`.
//!
//! Surface so far: read/advance behavior toggles, window position, tab/aircraft
//! state, and (M4) a bindings table mapping triggers → actions.

use crate::input::Bindings;
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

    /// Trigger → action bindings. Empty on first run; populated from
    /// `Bindings::defaults()` after load so existing settings.toml files
    /// keep working without manual migration.
    #[serde(default)]
    pub bindings: Bindings,

    /// Preferred cpal input device name. None → use the system default.
    /// Stored by name (rather than index) so headphone reconnects keep the
    /// right mic selected even if the device list reorders.
    #[serde(default)]
    pub audio_input: Option<String>,

    /// How long the last-transcript pill stays fully visible before fading,
    /// in seconds. 0 disables the pill entirely.
    #[serde(default = "Settings::default_transcript_pill")]
    pub transcript_pill_seconds: f32,

    /// Watch the active tab's source directory and reload pages when files
    /// on disk change. Off by default — useful when iterating in the
    /// generator's `node build.js preview` loop, but unnecessary otherwise.
    #[serde(default)]
    pub hot_reload: bool,

    /// Discard hot-mic audio while TTS is reading, so the synthesiser's
    /// voice doesn't bleed back through the mic and get transcribed.
    /// Headphone users may want to turn this off.
    #[serde(default = "Settings::default_mute_mic_during_speech")]
    pub mute_mic_during_speech: bool,

    /// Apply WS_EX_TRANSPARENT so mouse clicks pass through to whatever's
    /// behind. Useful while flying DCS — but ALL UI clicks (gear, nav,
    /// drag) stop working too, so bind ToggleClickThrough to a HOTAS button
    /// before you turn this on.
    #[serde(default)]
    pub click_through: bool,
}

impl Settings {
    fn default_auto_read() -> bool { true }   // most users expect Next to also speak
    fn default_auto_advance() -> bool { false }
    fn default_delay() -> f32 { 1.5 }
    fn default_read_notes() -> bool { false }
    fn default_transcript_pill() -> f32 { 5.0 }
    fn default_mute_mic_during_speech() -> bool { true }

    pub fn load_or_default(path: &Path) -> Self {
        let mut s = match std::fs::read_to_string(path) {
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
        };
        // A first-run load (or one from a pre-M4 file) has no bindings yet —
        // seed with defaults and write them straight back so the table shows
        // up in settings.toml for manual inspection / editing. Users with a
        // customised table keep what they have.
        if s.bindings.0.is_empty() {
            s.bindings = Bindings::defaults();
            if let Err(e) = s.save(path) {
                eprintln!("[settings] seed-save failed: {e:?}");
            }
        }
        s
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
            bindings: Bindings::default(),
            audio_input: None,
            transcript_pill_seconds: Self::default_transcript_pill(),
            hot_reload: false,
            mute_mic_during_speech: Self::default_mute_mic_during_speech(),
            click_through: false,
        }
    }
}

pub fn settings_path() -> PathBuf {
    PathBuf::from("settings.toml")
}
