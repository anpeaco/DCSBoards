//! TTS engine abstraction + WinRT implementation + pronunciation tuning.
//!
//! Pronunciation is engine-agnostic: abbreviation expansion happens in
//! `spoken_for` *before* the text reaches any synthesizer, so swapping to
//! Piper/Kokoro later (per SPEC §7.4) doesn't lose the customisation.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

pub trait TtsEngine: Send {
    fn speak(&mut self, text: &str, interrupt: bool) -> Result<()>;
    fn stop(&mut self) -> Result<()>;
    fn name(&self) -> &'static str;
}

/// Pronunciation overrides loaded from `pronunciation.toml`. Whole-word matches
/// (alphanumeric runs only — punctuation breaks tokens) get replaced verbatim.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PronunciationConfig {
    #[serde(default)]
    pub abbreviations: HashMap<String, String>,
}

impl PronunciationConfig {
    pub fn load_or_default(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => match toml::from_str::<Self>(&text) {
                Ok(cfg) => {
                    eprintln!(
                        "[tts] loaded pronunciation overrides: {} entries from {}",
                        cfg.abbreviations.len(),
                        path.display()
                    );
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
    expand_abbreviations(&base, &cfg.abbreviations)
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
/// "Page 2", "Page 3". Whisper TTS naturally reads digits as words, so the
/// listener hears "Page two" without further transformation. Only matches
/// when the "PT" is a word-start (so we don't munge a stray "OPT.2").
fn expand_page_refs(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let next_is_digit = chars
            .get(i + 3)
            .map_or(false, |c| c.is_ascii_digit());
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
        // word-start guard: don't expand inside another word
        assert_eq!(normalise_step("OPT.2 stuff"), "OPT.2 stuff");
        // need at least one digit
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
        // Substring match should not fire (no PWRX expansion).
        assert_eq!(expand_abbreviations("PWRX", &m), "PWRX");
    }

    #[test]
    fn preserves_punctuation_and_case() {
        let m = map(&[("PWR", "POWER")]);
        assert_eq!(expand_abbreviations("MAIN PWR, BATT", &m), "MAIN POWER, BATT");
    }
}

#[cfg(windows)]
pub use win::WinRtTts;

#[cfg(windows)]
mod win {
    use super::*;
    use windows::core::{Interface, HSTRING};
    use windows::Media::Core::MediaSource;
    use windows::Media::Playback::MediaPlayer;
    use windows::Media::SpeechSynthesis::{SpeechSynthesizer, VoiceInformation};
    use windows::Storage::Streams::IRandomAccessStream;

    pub struct WinRtTts {
        synth: SpeechSynthesizer,
        player: MediaPlayer,
    }

    impl WinRtTts {
        pub fn new() -> Result<Self> {
            let synth = SpeechSynthesizer::new().context("create SpeechSynthesizer")?;
            let player = MediaPlayer::new().context("create MediaPlayer")?;

            // Log what's available and try to pick something better than the default.
            list_voices_to_stderr();
            match pick_best_voice() {
                Ok(Some(v)) => {
                    let name = v.DisplayName().map(|h| h.to_string()).unwrap_or_default();
                    let lang = v.Language().map(|h| h.to_string()).unwrap_or_default();
                    eprintln!("[tts] selected: {name} ({lang})");
                    if let Err(e) = synth.SetVoice(&v) {
                        eprintln!("[tts] SetVoice failed: {e:?}");
                    }
                }
                Ok(None) => eprintln!("[tts] no voices enumerated; using system default"),
                Err(e) => eprintln!("[tts] voice selection failed: {e:?}"),
            }

            Ok(Self { synth, player })
        }
    }

    impl TtsEngine for WinRtTts {
        fn speak(&mut self, text: &str, interrupt: bool) -> Result<()> {
            if interrupt {
                let _ = self.player.Pause();
            }
            let text_h = HSTRING::from(text);
            let op = self
                .synth
                .SynthesizeTextToStreamAsync(&text_h)
                .context("SynthesizeTextToStreamAsync")?;
            let stream = op.get().context("await synthesis result")?;
            let ras: IRandomAccessStream =
                stream.cast().context("cast to IRandomAccessStream")?;
            let source = MediaSource::CreateFromStream(&ras, &HSTRING::new())
                .context("MediaSource::CreateFromStream")?;
            self.player.SetSource(&source).context("MediaPlayer.SetSource")?;
            self.player.Play().context("MediaPlayer.Play")?;
            Ok(())
        }

        fn stop(&mut self) -> Result<()> {
            self.player.Pause().context("MediaPlayer.Pause")?;
            Ok(())
        }

        fn name(&self) -> &'static str {
            "winrt"
        }
    }

    fn enumerate_voices() -> Result<Vec<VoiceInformation>> {
        let view = SpeechSynthesizer::AllVoices().context("AllVoices")?;
        let n = view.Size().context("voices Size")?;
        let mut out = Vec::with_capacity(n as usize);
        for i in 0..n {
            out.push(view.GetAt(i).context("voices GetAt")?);
        }
        Ok(out)
    }

    fn list_voices_to_stderr() {
        match enumerate_voices() {
            Ok(voices) => {
                eprintln!("[tts] {} voices installed:", voices.len());
                for v in &voices {
                    let name = v.DisplayName().map(|h| h.to_string()).unwrap_or_default();
                    let lang = v.Language().map(|h| h.to_string()).unwrap_or_default();
                    eprintln!("  - {name} ({lang})");
                }
                eprintln!(
                    "[tts] tip: Settings > Time & Language > Speech > Manage voices"
                );
                eprintln!(
                    "      installs higher-quality Natural voices (offline) on Win 11."
                );
            }
            Err(e) => eprintln!("[tts] enumerate voices failed: {e:?}"),
        }
    }

    /// Quality heuristic: prefer Natural voices, then Aria/Jenny/Guy/Davis
    /// (the modern OneCore neural-ish voices), then anything that isn't David
    /// (widely considered the worst-sounding default).
    fn pick_best_voice() -> Result<Option<VoiceInformation>> {
        let voices = enumerate_voices()?;
        if voices.is_empty() {
            return Ok(None);
        }

        // Filter to English variants when possible.
        let en: Vec<VoiceInformation> = voices
            .iter()
            .filter(|v| {
                v.Language()
                    .map(|h| h.to_string().starts_with("en"))
                    .unwrap_or(false)
            })
            .cloned()
            .collect();
        let pool = if !en.is_empty() { en } else { voices };

        let priorities = ["Natural", "Aria", "Jenny", "Guy", "Davis", "Sonia", "Libby"];
        for needle in priorities {
            if let Some(v) = pool.iter().find(|v| {
                v.DisplayName()
                    .map(|h| h.to_string().contains(needle))
                    .unwrap_or(false)
            }) {
                return Ok(Some(v.clone()));
            }
        }

        // Otherwise prefer anything non-David.
        if let Some(v) = pool.iter().find(|v| {
            v.DisplayName()
                .map(|h| !h.to_string().contains("David"))
                .unwrap_or(true)
        }) {
            return Ok(Some(v.clone()));
        }

        Ok(pool.into_iter().next())
    }
}

#[cfg(not(windows))]
pub struct NoopTts;

#[cfg(not(windows))]
impl NoopTts {
    pub fn new() -> Result<Self> {
        Ok(Self)
    }
}

#[cfg(not(windows))]
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
