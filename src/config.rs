//! Static infrastructure config — defines which tabs and aircraft exist.
//!
//! Loaded from `config.toml` at startup. Distinct from `settings.toml`
//! (user-tunable runtime state like toggles, window position, current aircraft).
//! If `config.toml` is missing, we synthesize a sensible default that just
//! exposes the `pages-sample/` dev fixture as a single tab.

use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub aircraft: Vec<AircraftEntry>,
    #[serde(default, rename = "tabs")]
    pub tabs: Vec<TabConfig>,
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
