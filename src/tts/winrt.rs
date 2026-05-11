//! WinRT-backed TTS via Windows.Media.SpeechSynthesis. Quality depends on
//! which voices the user has installed — Win 11 Natural voices sound much
//! better than the legacy SAPI ones.

use super::TtsEngine;
use anyhow::{Context, Result};
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

    fn set_rate(&mut self, rate: f32) {
        // SpeechSynthesizer.Options.SpeakingRate accepts 0.5..=6.0 (Microsoft
        // docs); clamp to a sane subset for the UI slider.
        let clamped = rate.clamp(0.5, 2.0) as f64;
        if let Ok(opts) = self.synth.Options() {
            let _ = opts.SetSpeakingRate(clamped);
        }
    }

    fn set_volume(&mut self, volume: f32) {
        let v = volume.clamp(0.0, 1.0) as f64;
        // AudioVolume scales the synthesised PCM amplitude; MediaPlayer.Volume
        // scales playback. Set both so very low values can fully silence.
        if let Ok(opts) = self.synth.Options() {
            let _ = opts.SetAudioVolume(v);
        }
        let _ = self.player.SetVolume(v);
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

    if let Some(v) = pool.iter().find(|v| {
        v.DisplayName()
            .map(|h| !h.to_string().contains("David"))
            .unwrap_or(true)
    }) {
        return Ok(Some(v.clone()));
    }

    Ok(pool.into_iter().next())
}
