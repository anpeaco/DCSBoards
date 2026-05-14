//! TTS engine abstraction + pronunciation tuning.
//!
//! Pronunciation is engine-agnostic: abbreviation expansion happens in
//! `spoken_for` *before* the text reaches any synthesizer, so swapping
//! engines doesn't lose the customisation.

use anyhow::Result;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

#[cfg(windows)]
pub mod piper;
#[cfg(windows)]
pub mod winrt;
#[cfg(not(windows))]
pub mod noop;

#[cfg(windows)]
pub use piper::PiperTts;
#[cfg(windows)]
pub use winrt::WinRtTts;
#[cfg(not(windows))]
pub use noop::NoopTts;

pub trait TtsEngine: Send {
    fn speak(&mut self, text: &str, interrupt: bool) -> Result<()>;
    fn stop(&mut self) -> Result<()>;
    fn name(&self) -> &'static str;

    /// Set the speaking rate multiplier. 1.0 is normal speed, higher is
    /// faster, lower is slower. Engines clamp to their supported range.
    /// Default impl is a no-op so engines that don't support rate compile.
    fn set_rate(&mut self, _rate: f32) {}

    /// Set the output volume in 0.0..=1.0. Applies to playback.
    fn set_volume(&mut self, _volume: f32) {}
}

/// Pronunciation overrides loaded from `pronunciation.toml`.
///
/// `abbreviations`: whole-word matches on alphanumeric tokens — punctuation
/// breaks the token, single words only. `PWR = "POWER"` swaps the word.
///
/// `phrases`: literal multi-word substring replacements applied **before**
/// the abbreviation pass, longest-first so "POS Mode" wins over a bare "POS"
/// rule. Use this for two-word fixes where the per-word rules over-trigger.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PronunciationConfig {
    #[serde(default)]
    pub abbreviations: HashMap<String, String>,
    #[serde(default)]
    pub phrases: HashMap<String, String>,
}

impl PronunciationConfig {
    pub fn load_or_default(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => match toml::from_str::<Self>(&text) {
                Ok(cfg) => {
                    eprintln!(
                        "[tts] loaded pronunciation overrides: {} abbreviations, {} phrases from {}",
                        cfg.abbreviations.len(),
                        cfg.phrases.len(),
                        path.display()
                    );
                    if !cfg.phrases.is_empty() {
                        let mut keys: Vec<&String> = cfg.phrases.keys().collect();
                        keys.sort();
                        for k in keys {
                            if let Some(v) = cfg.phrases.get(k) {
                                eprintln!("[tts]   phrase: {:?} -> {:?}", k, v);
                            }
                        }
                    }
                    cfg
                }
                Err(e) => {
                    eprintln!("[tts] {} parse failed: {e}", path.display());
                    Self::default()
                }
            },
            Err(_) => {
                eprintln!("[tts] no {} found; no abbreviation expansion", path.display());
                Self::default()
            }
        }
    }
}

/// Derive the text to speak from an item.
///
/// 1. If the item has an explicit `spoken` override, use that as-is (the author
///    knows best).
/// 2. Otherwise strip the `| context` portion (the pilot is looking at the
///    method, doesn't need to hear it) and convert ` ... ` separators to
///    commas for a natural pause before the target state.
/// 3. Apply whole-word abbreviation expansion from the pronunciation config.
pub fn spoken_for(text: &str, override_: Option<&str>, cfg: &PronunciationConfig) -> String {
    let base = match override_ {
        Some(s) => s.to_string(),
        None => normalise_step(text),
    };
    // Phrases first (longest-key first so longer matches win) then per-word.
    let phrased = apply_phrases(&base, &cfg.phrases);
    let result = expand_abbreviations(&phrased, &cfg.abbreviations);
    // One-shot diagnostic: prove the abbreviation map has the keys we
    // expect. Fires the first time spoken_for is called per process.
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        eprintln!(
            "[pron] map size={}, WPN={:?}, SOI={:?}, osb={:?}, REQ={:?}",
            cfg.abbreviations.len(),
            cfg.abbreviations.get("WPN"),
            cfg.abbreviations.get("SOI"),
            cfg.abbreviations.get("osb"),
            cfg.abbreviations.get("REQ"),
        );
    });
    if base != result {
        eprintln!("[pron] {:?} -> {:?}", base, result);
    }
    result
}

fn normalise_step(text: &str) -> String {
    let stripped = match text.split_once(" | ") {
        Some((before, after)) => match after.split_once("...") {
            Some((_method, action)) => format!("{}, {}", before.trim(), action.trim()),
            None => before.trim().to_string(),
        },
        None => text.replace(" ... ", ", "),
    };
    // Forward slash in source text ("CAT 1 / CAT 11", "AGM-65D/G", "TGP/LST")
    // reads naturally as a short pause — commas give the synthesiser a brief
    // beat rather than the "slash" being spoken literally.
    let with_slashes = stripped.replace('/', ", ");
    expand_page_refs(&with_slashes)
}

/// Generator convention: "PT.2", "Pt.3" etc. are page references; spoken as
/// "Page 2", "Page 3". TTS naturally reads digits as words, so the listener
/// hears "Page two" without further transformation. Only matches when the
/// "PT" is a word-start (so we don't munge a stray "OPT.2").
fn expand_page_refs(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let next_is_digit = chars
            .get(i + 3)
            .is_some_and(|c| c.is_ascii_digit());
        if i + 3 < chars.len()
            && (chars[i] == 'P' || chars[i] == 'p')
            && (chars[i + 1] == 'T' || chars[i + 1] == 't')
            && chars[i + 2] == '.'
            && next_is_digit
            && (i == 0 || !chars[i - 1].is_alphanumeric())
        {
            out.push_str("Page ");
            i += 3;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Literal substring replace, applied longest-key first so a longer phrase
/// beats a shorter one that's a prefix of it. Case-INSENSITIVE — phrases
/// are higher-level intent ("POS Mode" should match "POS MODE" and "pos mode"
/// alike); per-word abbreviations keep their case-sensitive behaviour so
/// `PWR` doesn't munge a lowercase `pwr` accidentally.
fn apply_phrases(text: &str, map: &HashMap<String, String>) -> String {
    if map.is_empty() {
        return text.to_string();
    }
    let mut keys: Vec<&String> = map.keys().collect();
    keys.sort_by_key(|k| std::cmp::Reverse(k.len()));
    let mut out = text.to_string();
    for k in keys {
        if k.is_empty() {
            continue;
        }
        if let Some(v) = map.get(k) {
            out = replace_ascii_ci(&out, k, v);
        }
    }
    out
}

/// Case-insensitive substring replace. ASCII-only (which all our pronunciation
/// keys are). Lowercases for matching but splices the replacement verbatim.
fn replace_ascii_ci(haystack: &str, needle: &str, replacement: &str) -> String {
    if needle.is_empty() {
        return haystack.to_string();
    }
    let lower_h = haystack.to_ascii_lowercase();
    let lower_n = needle.to_ascii_lowercase();
    let mut out = String::with_capacity(haystack.len());
    let mut last = 0usize;
    let mut from = 0usize;
    while let Some(pos) = lower_h[from..].find(&lower_n) {
        let abs = from + pos;
        out.push_str(&haystack[last..abs]);
        out.push_str(replacement);
        last = abs + needle.len();
        from = last;
        if from > haystack.len() {
            break;
        }
    }
    out.push_str(&haystack[last..]);
    out
}

fn expand_abbreviations(text: &str, map: &HashMap<String, String>) -> String {
    if map.is_empty() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut tok = String::new();
    for c in text.chars() {
        if c.is_alphanumeric() {
            tok.push(c);
        } else {
            flush(&mut tok, &mut out, map);
            out.push(c);
        }
    }
    flush(&mut tok, &mut out, map);
    out
}

fn flush(tok: &mut String, out: &mut String, map: &HashMap<String, String>) {
    if tok.is_empty() {
        return;
    }
    match map.get(tok.as_str()) {
        Some(expansion) => out.push_str(expansion),
        None => out.push_str(tok),
    }
    tok.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn expands_page_refs() {
        assert_eq!(normalise_step("Go to PT.2 next"), "Go to Page 2 next");
        assert_eq!(normalise_step("PT.10 reference"), "Page 10 reference");
        assert_eq!(normalise_step("OPT.2 stuff"), "OPT.2 stuff");
        assert_eq!(normalise_step("PT.abc"), "PT.abc");
    }

    #[test]
    fn strips_context_and_separator() {
        assert_eq!(
            normalise_step("FLCS PWR TEST | hold switch to TEST ... TEST"),
            "FLCS PWR TEST, TEST"
        );
        assert_eq!(normalise_step("MAIN PWR switch ... BATT"), "MAIN PWR switch, BATT");
    }

    #[test]
    fn expands_whole_words_only() {
        let m = map(&[("PWR", "POWER"), ("FLCS", "Flickus")]);
        assert_eq!(expand_abbreviations("FLCS PWR TEST", &m), "Flickus POWER TEST");
        assert_eq!(expand_abbreviations("PWRX", &m), "PWRX");
    }

    #[test]
    fn preserves_punctuation_and_case() {
        let m = map(&[("PWR", "POWER")]);
        assert_eq!(expand_abbreviations("MAIN PWR, BATT", &m), "MAIN POWER, BATT");
    }
}
