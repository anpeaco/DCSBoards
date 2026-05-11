# Whisper models

Download the GGML quantised whisper.cpp model here. The overlay looks for
`models/ggml-base.en.bin` first; it falls back to `ggml-small.en.bin` then
`ggml-tiny.en.bin` if that's missing.

Quick fetch (PowerShell):

```powershell
Invoke-WebRequest `
  -Uri "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin" `
  -OutFile "models/ggml-base.en.bin"
```

Or `ggml-tiny.en.bin` (~75 MB, very fast, lower accuracy) for a smaller
download.
