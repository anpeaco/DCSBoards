# Piper TTS (optional)

Piper is an open-source local neural TTS. Voices sound much better than the
Windows SAPI defaults; latency is sub-second for short utterances. The app
launches `piper.exe` per spoken phrase and plays the resulting WAV.

## Install

1. **Download piper.exe (~25 MB)** from
   https://github.com/rhasspy/piper/releases — pick the latest
   `piper_windows_amd64.zip`. Extract `piper.exe` into this folder so the
   final path is:

   ```
   models/piper/piper.exe
   ```

2. **Download a voice** from
   https://huggingface.co/rhasspy/piper-voices — each voice has a `.onnx`
   file and a matching `.onnx.json` config. Drop **both** into:

   ```
   models/piper/voices/<voice>.onnx
   models/piper/voices/<voice>.onnx.json
   ```

   Good starter voices (~63 MB each):

   - **en_GB-alan-medium** — British male, calm.
   - **en_GB-southern_english_female-low** — British female, clear.
   - **en_US-lessac-medium** — American female, neutral.
   - **en_US-ryan-medium** — American male, expressive.

3. Run the app. In **Settings ▸ TEXT-TO-SPEECH**, switch the engine to
   **Piper (neural)**, then pick a voice from the list that appears. The
   **Test** button speaks a sample phrase.

## Troubleshooting

- **"piper.exe not found"** — the path must be exactly `models/piper/piper.exe`
  relative to the working directory (the project root when running with
  `cargo run`, or the portable bundle's root when running the .exe).
- **"voice config not found"** — every `.onnx` needs its `.onnx.json` sibling.
- **No audio** — Piper writes to your temp folder then asks Windows to play
  it. Check `%TEMP%\dcs-kneeboard-piper.wav` exists after pressing Test;
  if not, the piper.exe call is failing — try `models\piper\piper.exe
  --model voices\<your-voice>.onnx --output_file out.wav` in a shell to see
  the error.
- **Audible click on stop** — the MediaPlayer cancellation cuts mid-frame;
  acceptable for v1.

## Reverting to system voices

Switch the engine back to **WinRT (system)** in Settings. The Piper files
can stay where they are.
