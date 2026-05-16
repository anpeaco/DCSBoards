//! User-tunable runtime settings persisted to `settings.toml`.
//!
//! Surface so far: read/advance behavior toggles, window position, tab/aircraft
//! state, and (M4) a bindings table mapping triggers → actions.

use crate::input::Bindings;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Saved VR overlay pose (#30 phase 4). One per aircraft so a pilot
/// can leave the F-16 kneeboard pinned to its dash spot and the A-10
/// kneeboard pinned to a different spot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VrPose {
    /// OpenVR row-major 3x4 transform (rotation + translation in m).
    pub transform: [[f32; 4]; 3],
    /// Overlay width in meters. Height is implied by the page aspect.
    pub size_m: f32,
}

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
    /// Cleared/clamped at startup if the persisted point isn't inside any
    /// visible monitor (e.g. user unplugged the external display we were
    /// pinned to) so the overlay can't get stranded off-screen.
    #[serde(default)]
    pub window_x: Option<i32>,
    #[serde(default)]
    pub window_y: Option<i32>,

    /// Last-known window size in physical pixels. None on first run, in
    /// which case the slint default (600x900) applies. Saved on close and
    /// on the same debounce as position.
    #[serde(default)]
    pub window_w: Option<u32>,
    #[serde(default)]
    pub window_h: Option<u32>,

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

    /// Window opacity, 0.3..=1.0. Applied via Win32
    /// `SetLayeredWindowAttributes` so the whole overlay (image, chrome,
    /// pills) fades uniformly. Floor of 0.3 keeps the UI legible — going
    /// lower would let users render the app effectively invisible with no
    /// way to find it again.
    #[serde(default = "Settings::default_window_opacity")]
    pub window_opacity: f32,

    /// Which TTS engine to use: "winrt" (Windows system voices, default) or
    /// "piper" (open-source neural — needs models/piper/piper.exe + a .onnx
    /// voice). Falls back to winrt if piper is selected but unavailable.
    #[serde(default = "Settings::default_tts_engine")]
    pub tts_engine: String,

    /// Path to the chosen Piper voice (.onnx). Files placed in
    /// `models/piper/voices/` appear automatically in the voice picker.
    #[serde(default)]
    pub tts_piper_voice: Option<String>,

    /// Speaking rate multiplier (1.0 = normal). Sensible UI range: 0.5–2.0.
    #[serde(default = "Settings::default_tts_rate")]
    pub tts_rate: f32,

    /// Output volume, 0.0..=1.0.
    #[serde(default = "Settings::default_tts_volume")]
    pub tts_volume: f32,

    /// First-run welcome overlay dismissal marker. Defaults true so
    /// existing settings.toml files (from before this field existed)
    /// don't suddenly show the welcome on upgrade — the only path that
    /// flips it back to false is `main` detecting the absence of
    /// settings.toml at startup.
    #[serde(default = "Settings::default_welcome_shown")]
    pub welcome_shown: bool,

    /// Verbose per-dispatch tracing. When on, every Action that fires
    /// gets a one-line `[dispatch] <source> → <action> (<label>)` log
    /// showing what physical input (keyboard / gamepad / HID / voice /
    /// on-screen button / startup) produced it. Off by default — useful
    /// when binding HOTAS buttons to debug which button fires what.
    #[serde(default)]
    pub dispatch_log: bool,

    /// VR overlay mode (#30 phase 3). One of:
    ///   "auto"    — enter VR if SteamVR is running AND an HMD is
    ///               present (default).
    ///   "vr"      — force VR mode even without HMD detection.
    ///   "desktop" — force desktop mode; never init OpenVR.
    /// Re-evaluated every 2 s so plugging/unplugging the headset mid-
    /// session triggers the auto-switch transparently.
    #[serde(default = "Settings::default_vr_mode")]
    pub vr_mode: String,

    /// Saved VR overlay poses keyed by aircraft id (#30 phase 4).
    /// Updated by Vr* actions (place-here, nudge, resize, reset);
    /// loaded on aircraft switch so each module remembers where the
    /// pilot last pinned its kneeboard.
    #[serde(default)]
    pub vr_poses: HashMap<String, VrPose>,
}

impl Settings {
    fn default_auto_read() -> bool { true }   // most users expect Next to also speak
    fn default_auto_advance() -> bool { false }
    fn default_delay() -> f32 { 1.5 }
    fn default_read_notes() -> bool { false }
    fn default_transcript_pill() -> f32 { 5.0 }
    fn default_mute_mic_during_speech() -> bool { true }
    fn default_tts_engine() -> String { "winrt".into() }
    fn default_tts_rate() -> f32 { 1.0 }
    fn default_tts_volume() -> f32 { 1.0 }
    fn default_window_opacity() -> f32 { 1.0 }
    fn default_welcome_shown() -> bool { true }
    fn default_vr_mode() -> String { "auto".into() }

    /// Clamp a stored or UI-supplied opacity into the legal 0.3..=1.0
    /// range. Centralised so the floor lives in one place.
    pub fn clamp_window_opacity(v: f32) -> f32 {
        v.clamp(0.3, 1.0)
    }

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
            window_w: None,
            window_h: None,
            current_aircraft: None,
            last_tab: None,
            bindings: Bindings::default(),
            audio_input: None,
            transcript_pill_seconds: Self::default_transcript_pill(),
            hot_reload: false,
            mute_mic_during_speech: Self::default_mute_mic_during_speech(),
            click_through: false,
            window_opacity: Self::default_window_opacity(),
            tts_engine: Self::default_tts_engine(),
            tts_piper_voice: None,
            tts_rate: Self::default_tts_rate(),
            tts_volume: Self::default_tts_volume(),
            welcome_shown: Self::default_welcome_shown(),
            vr_mode: Self::default_vr_mode(),
            vr_poses: HashMap::new(),
            dispatch_log: false,
        }
    }
}

pub fn settings_path() -> PathBuf {
    PathBuf::from("settings.toml")
}
