//! Static infrastructure config — defines which tabs and aircraft exist.
//!
//! Loaded from `config.toml` at startup. Distinct from `settings.toml`
//! (user-tunable runtime state like toggles, window position, current aircraft).
//! If `config.toml` is missing, we synthesize a sensible default that just
//! exposes the `pages-sample/` dev fixture as a single tab.

use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub aircraft: Vec<AircraftEntry>,
    #[serde(default, rename = "tabs")]
    pub tabs: Vec<TabConfig>,
    #[serde(default)]
    pub stt: SttConfig,
}

/// Speech-to-text tuning knobs. All three knobs target the same problem
/// from different angles: domain-specific terms like "HARM", "AGM-65",
/// or "JDAM" get transcribed as homophones ("home", "agement", "Jay
/// Damn"). Layer the techniques — they compose.
///
/// 1. `vocabulary` (+ `per_aircraft`) feeds Whisper's `initial_prompt`
///    so the model is biased toward the listed terms during decoding.
/// 2. `corrections` rewrites the transcript after Whisper produces it
///    but before the voice router sees it. Cheap, deterministic, and
///    catches the cases that biasing alone misses.
/// 3. `fuzzy_threshold` controls when the voice router falls back to
///    similarity matching against the action phrase table.
#[derive(Debug, Clone, Deserialize)]
pub struct SttConfig {
    /// Always-on vocabulary terms. Joined into the initial prompt
    /// regardless of active aircraft (common system names, generic
    /// weapon families).
    #[serde(default)]
    pub vocabulary: Vec<String>,
    /// Per-aircraft vocabulary, keyed by aircraft id. Appended on top
    /// of `vocabulary` when that aircraft becomes active.
    #[serde(default)]
    pub per_aircraft: HashMap<String, Vec<String>>,
    /// Phrase → replacement, applied to the transcript before voice
    /// routing. Keys are lowercased; matching is word-boundary so
    /// "home" doesn't rewrite "homestead". Longest key wins so
    /// "jay dam" beats a stray "jay".
    #[serde(default)]
    pub corrections: HashMap<String, String>,
    /// Jaro-Winkler similarity threshold (0..1) for the voice router's
    /// fuzzy fallback against action phrases. Higher = stricter. 0.85
    /// catches obvious misrecognitions ("nest"/"nect" → "next")
    /// without spuriously routing unrelated text.
    #[serde(default = "default_fuzzy_threshold")]
    pub fuzzy_threshold: f32,
}

impl Default for SttConfig {
    fn default() -> Self {
        Self {
            vocabulary: Vec::new(),
            per_aircraft: HashMap::new(),
            corrections: HashMap::new(),
            fuzzy_threshold: default_fuzzy_threshold(),
        }
    }
}

impl SttConfig {
    /// Concatenate global + per-aircraft vocabulary into the string
    /// Whisper accepts as `initial_prompt`. Returns None when no terms
    /// apply so the caller can leave the prompt unset (which is what
    /// Whisper expects — passing "" still nudges decoding).
    ///
    /// Embedded NULs are stripped: `whisper-rs::FullParams::
    /// set_initial_prompt` panics on a CString::new failure, and a
    /// user-edited config.toml is an untrusted input path.
    pub fn build_initial_prompt(&self, aircraft: &str) -> Option<String> {
        let mut terms: Vec<String> = self.vocabulary.to_vec();
        if let Some(extra) = self.per_aircraft.get(aircraft) {
            terms.extend(extra.iter().cloned());
        }
        if terms.is_empty() {
            return None;
        }
        let body = terms
            .into_iter()
            .map(|t| t.replace('\0', ""))
            .filter(|t| !t.trim().is_empty())
            .collect::<Vec<_>>()
            .join(", ");
        if body.is_empty() {
            return None;
        }
        // Frame the list as in-domain vocabulary rather than free text
        // so Whisper biases token selection toward these words instead
        // of treating the prompt as an ongoing utterance to continue.
        Some(format!("DCS checklist vocabulary: {body}."))
    }
}

fn default_fuzzy_threshold() -> f32 {
    0.85
}

#[derive(Debug, Clone, Deserialize)]
pub struct AircraftEntry {
    pub id: String,
    #[serde(default)]
    pub label: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TabConfig {
    pub id: String,
    pub label: String,
    #[serde(default = "default_icon")]
    pub icon: String,
    pub source: SourceConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SourceConfig {
    /// kneeboards-generator output: PNG + sidecar JSON. The `{aircraft}`
    /// placeholder is replaced with the current aircraft id at load time.
    Generator { path: String },
    /// Raw image folder — no sidecar JSONs, page-level nav only.
    ImageFolder {
        path: String,
        #[serde(default)]
        recursive: bool,
    },
    /// DCS Saved Games kneeboards for the current aircraft.
    /// Path resolves to `%USERPROFILE%/Saved Games/DCS/Kneeboard/{aircraft}/`
    /// with a fallback to `DCS.openbeta`.
    DcsKneeboards {
        #[serde(default)]
        base: Option<String>,
    },
}

fn default_icon() -> String {
    "file".to_string()
}

impl AppConfig {
    pub fn load_or_default(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => match toml::from_str::<Self>(&text) {
                Ok(cfg) => {
                    eprintln!(
                        "[config] loaded from {} — {} aircraft, {} tabs",
                        path.display(),
                        cfg.aircraft.len(),
                        cfg.tabs.len()
                    );
                    cfg
                }
                Err(e) => {
                    eprintln!("[config] {} parse failed: {e}", path.display());
                    Self::default_config()
                }
            },
            Err(_) => {
                eprintln!("[config] no {} — using built-in default", path.display());
                Self::default_config()
            }
        }
    }

    fn default_config() -> Self {
        Self {
            aircraft: vec![AircraftEntry {
                id: "F-16C_50".to_string(),
                label: "F-16C Viper".to_string(),
            }],
            tabs: vec![TabConfig {
                id: "checklists".to_string(),
                label: "Checklists".to_string(),
                icon: "clipboard-list".to_string(),
                source: SourceConfig::Generator {
                    path: "pages-sample".to_string(),
                },
            }],
            stt: SttConfig::default(),
        }
    }
}

pub fn config_path() -> PathBuf {
    PathBuf::from("config.toml")
}

/// Replace the `{aircraft}` placeholder in a path string.
pub fn resolve_aircraft(template: &str, aircraft: &str) -> PathBuf {
    PathBuf::from(template.replace("{aircraft}", aircraft))
}

/// Resolve the DCS Saved Games kneeboard folder for an aircraft. Tries
/// `Saved Games/DCS/Kneeboard/<aircraft>/` first, then `DCS.openbeta`.
pub fn resolve_dcs_kneeboard_dir(aircraft: &str, override_base: Option<&str>) -> Option<PathBuf> {
    if let Some(base) = override_base {
        let p = PathBuf::from(base).join(aircraft);
        if p.exists() {
            return Some(p);
        }
    }
    let user = std::env::var("USERPROFILE").ok()?;
    let candidates = [
        PathBuf::from(&user).join("Saved Games").join("DCS").join("Kneeboard").join(aircraft),
        PathBuf::from(&user).join("Saved Games").join("DCS.openbeta").join("Kneeboard").join(aircraft),
    ];
    candidates.into_iter().find(|p| p.exists())
}
