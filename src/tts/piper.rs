//! Piper TTS via subprocess.
//!
//! Piper is an open-source local neural TTS (https://github.com/rhasspy/piper).
//! We launch `piper.exe` per utterance, pipe the source text to its stdin,
//! and have it write a WAV file. Once the process exits we hand the file to
//! Windows' MediaPlayer for playback — same plumbing the WinRT engine uses.
//!
//! A dedicated worker thread serialises synthesis and gives us a clean
//! cancellation point: a `Stop` message kills the in-flight piper process
//! and pauses MediaPlayer immediately.
//!
//! ## Caching
//!
//! Each synthesised utterance is written directly into a per-voice cache
//! directory (`models/piper/cache/<voice-stem>/<hash>.wav`). Future calls
//! with the same text + same voice find the file already on disk and skip
//! piper.exe entirely, dropping latency from ~300 ms to single-digit ms.
//! No eviction in v1; the cache is bounded by the number of unique strings
//! the user navigates through.

use super::TtsEngine;
use anyhow::{Context, Result};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use windows::core::HSTRING;
use windows::Foundation::Uri;
use windows::Media::Core::MediaSource;
use windows::Media::Playback::MediaPlayer;

enum PiperMsg {
    Speak(String),
    Stop,
    SetRate(f32),
    SetVolume(f32),
}

/// FNV-1a 64-bit. Stable across runs (unlike DefaultHasher), no external
/// dep. Collisions are theoretically possible but for our use case (a few
/// thousand checklist strings) the probability is vanishing.
fn fnv1a64(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce4_84222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

pub struct PiperTts {
    sender: mpsc::Sender<PiperMsg>,
    /// Cached locally so `set_rate` / `set_volume` are cheap and the worker
    /// always has the latest values without us echoing them back.
    rate: f32,
    volume: f32,
}

impl PiperTts {
    /// Validate that piper.exe and the voice model exist, set up the cache
    /// directory, then start the worker thread. Returns Err if any expected
    /// file is missing so callers can fall back to the WinRT engine.
    pub fn new(piper_path: PathBuf, voice_path: PathBuf) -> Result<Self> {
        if !piper_path.exists() {
            anyhow::bail!("piper.exe not found at {}", piper_path.display());
        }
        if !voice_path.exists() {
            anyhow::bail!("voice model not found at {}", voice_path.display());
        }
        let config_path = voice_path.with_extension("onnx.json");
        if !config_path.exists() {
            anyhow::bail!(
                "voice config not found at {} — pair the .onnx with its .onnx.json",
                config_path.display()
            );
        }

        // Resolve to absolute paths so the subprocess (which we launch with a
        // different cwd) can still find everything.
        let piper_path = piper_path
            .canonicalize()
            .with_context(|| format!("canonicalize piper exe {}", piper_path.display()))?;
        let voice_path = voice_path
            .canonicalize()
            .with_context(|| format!("canonicalize voice {}", voice_path.display()))?;

        // espeak-ng.dll bundled with piper hardcodes `/usr/share/espeak-ng-data`
        // at compile time. We work around that by patching the DLL at
        // install time (scripts/install-piper.ps1) to use `.\espeak-ng-data`
        // instead. With piper.exe launched with cwd = its own install dir,
        // that relative path resolves to models\piper\espeak-ng-data\.
        let piper_dir = piper_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));

        // One cache dir per voice — different voices produce different WAVs
        // for the same text, so they share no cache entries.
        let voice_stem = voice_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("voice");
        let cache_dir = PathBuf::from("models/piper/cache").join(voice_stem);
        std::fs::create_dir_all(&cache_dir)
            .with_context(|| format!("create piper cache dir {}", cache_dir.display()))?;
        let cache_dir = cache_dir
            .canonicalize()
            .with_context(|| format!("canonicalize cache dir {}", cache_dir.display()))?;

        eprintln!(
            "[piper] ready: exe={}, voice={}, cache={}",
            piper_path.display(),
            voice_path.display(),
            cache_dir.display(),
        );
        let (tx, rx) = mpsc::channel();
        thread::Builder::new()
            .name("piper-tts".into())
            .spawn(move || piper_worker(rx, piper_path, voice_path, cache_dir, piper_dir))
            .context("spawn piper worker")?;
        Ok(Self { sender: tx, rate: 1.0, volume: 1.0 })
    }
}

impl TtsEngine for PiperTts {
    fn speak(&mut self, text: &str, _interrupt: bool) -> Result<()> {
        // Speak always interrupts — the worker kills any in-flight synthesis
        // before starting a new one, matching the WinRT behaviour.
        self.sender
            .send(PiperMsg::Speak(text.to_string()))
            .map_err(|e| anyhow::anyhow!("piper worker disconnected: {e}"))?;
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        let _ = self.sender.send(PiperMsg::Stop);
        Ok(())
    }

    fn name(&self) -> &'static str {
        "piper"
    }

    fn set_rate(&mut self, rate: f32) {
        let clamped = rate.clamp(0.5, 2.0);
        self.rate = clamped;
        let _ = self.sender.send(PiperMsg::SetRate(clamped));
    }

    fn set_volume(&mut self, volume: f32) {
        let clamped = volume.clamp(0.0, 1.0);
        self.volume = clamped;
        let _ = self.sender.send(PiperMsg::SetVolume(clamped));
    }
}

// `piper_dir` is the working dir for the piper.exe subprocess — its own
// install dir, so the patched-in `.\espeak-ng-data` relative path resolves
// correctly and DLLs (onnxruntime, espeak-ng) load from siblings.
fn piper_worker(
    rx: mpsc::Receiver<PiperMsg>,
    piper: PathBuf,
    voice: PathBuf,
    cache_dir: PathBuf,
    piper_dir: PathBuf,
) {
    let player = match MediaPlayer::new() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[piper] MediaPlayer init failed: {e:?}");
            return;
        }
    };

    let mut active_child: Option<Child> = None;
    // The WAV path the child is writing to. We keep it around so we can
    // play it once the process exits.
    let mut pending_output: Option<PathBuf> = None;
    // Latest rate / volume — updated whenever the UI sends a Set*.
    let mut rate: f32 = 1.0;
    let mut volume: f32 = 1.0;
    let _ = player.SetVolume(volume as f64);

    loop {
        let timeout = if active_child.is_some() {
            Duration::from_millis(30)
        } else {
            Duration::from_millis(500)
        };

        match rx.recv_timeout(timeout) {
            Ok(PiperMsg::SetRate(r)) => {
                rate = r;
                continue;
            }
            Ok(PiperMsg::SetVolume(v)) => {
                volume = v;
                let _ = player.SetVolume(volume as f64);
                continue;
            }
            Ok(PiperMsg::Speak(text)) => {
                kill_child(&mut active_child);
                let _ = player.Pause();
                pending_output = None;

                // Cache key includes the length_scale so changing the rate
                // doesn't replay audio synthesised at the old rate.
                let length_scale = (1.0_f32 / rate).clamp(0.4, 3.0);
                let key_text = format!("ls={:.3}|{}", length_scale, text);
                let cache_path = cache_dir.join(format!("{:016x}.wav", fnv1a64(&key_text)));
                if cache_path.exists() {
                    eprintln!("[piper] cache hit: {:?}", text);
                    if let Err(e) = play_wav(&player, &cache_path) {
                        eprintln!("[piper] cached playback failed: {e:?}");
                    }
                    continue;
                }

                // Miss: synthesise directly into the cache path. cwd is
                // piper's install dir so the patched `.\espeak-ng-data`
                // path inside espeak-ng.dll resolves correctly.
                let mut cmd = Command::new(&piper);
                cmd.current_dir(&piper_dir)
                    .arg("--model")
                    .arg(&voice)
                    .arg("--output_file")
                    .arg(&cache_path)
                    // length_scale = 1/rate; higher length_scale = slower.
                    .arg("--length_scale")
                    .arg(format!("{:.3}", length_scale))
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .stderr(Stdio::piped());
                match cmd.spawn() {
                    Ok(mut child) => {
                        if let Some(mut stdin) = child.stdin.take() {
                            if let Err(e) = writeln!(stdin, "{}", text) {
                                eprintln!("[piper] write text failed: {e:?}");
                            }
                        }
                        eprintln!("[piper] synthesising (cache miss): {:?}", text);
                        active_child = Some(child);
                        pending_output = Some(cache_path);
                    }
                    Err(e) => {
                        eprintln!("[piper] spawn failed: {e:?}");
                    }
                }
            }
            Ok(PiperMsg::Stop) => {
                kill_child(&mut active_child);
                let _ = player.Pause();
                // If we were partway through writing a cache entry, throw it
                // out — half-written WAVs would just produce noise on replay.
                if let Some(p) = pending_output.take() {
                    let _ = std::fs::remove_file(p);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if let Some(child) = active_child.as_mut() {
                    match child.try_wait() {
                        Ok(Some(status)) => {
                            // try_wait already reaped the child on
                            // Ok(Some(_)). The explicit wait() below is
                            // a no-op idempotent reap that silences
                            // clippy::zombie_processes — it can't see
                            // through the try_wait branch.
                            let mut child = active_child.take().unwrap();
                            let _ = child.wait();
                            let out = pending_output.take();
                            if !status.success() {
                                let mut buf = String::new();
                                if let Some(mut err) = child.stderr.take() {
                                    let _ = err.read_to_string(&mut buf);
                                }
                                let trimmed = buf.trim();
                                eprintln!(
                                    "[piper] piper.exe exited with {status}{}",
                                    if trimmed.is_empty() {
                                        String::new()
                                    } else {
                                        format!(":\n{trimmed}")
                                    }
                                );
                                if let Some(p) = out {
                                    let _ = std::fs::remove_file(p);
                                }
                                continue;
                            }
                            if let Some(p) = out {
                                if let Err(e) = play_wav(&player, &p) {
                                    eprintln!("[piper] playback failed: {e:?}");
                                }
                            }
                        }
                        Ok(None) => {}
                        Err(e) => {
                            eprintln!("[piper] try_wait error: {e:?}");
                            active_child = None;
                            pending_output = None;
                        }
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    kill_child(&mut active_child);
}

fn kill_child(child: &mut Option<Child>) {
    if let Some(mut c) = child.take() {
        let _ = c.kill();
        let _ = c.wait();
    }
}

fn play_wav(player: &MediaPlayer, path: &Path) -> Result<()> {
    // MediaPlayer wants a URI for file-based sources. Encode the absolute
    // path with forward slashes so Windows accepts it as file:///C:/...
    let abs = path
        .canonicalize()
        .with_context(|| format!("canonicalize {}", path.display()))?;
    let abs_str = abs.to_string_lossy();
    // Strip the Windows extended-path prefix that canonicalize adds.
    let stripped = abs_str
        .strip_prefix(r"\\?\")
        .unwrap_or(&abs_str)
        .replace('\\', "/");
    let uri_str = format!("file:///{}", stripped);
    let uri = Uri::CreateUri(&HSTRING::from(uri_str.as_str())).context("create file URI")?;
    let source = MediaSource::CreateFromUri(&uri).context("MediaSource from URI")?;
    player.SetSource(&source).context("SetSource")?;
    player.Play().context("Play")?;
    Ok(())
}
