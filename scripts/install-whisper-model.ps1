# Download the Whisper STT model into ./models/.
#
# Run this from either:
#   - the repo root, after `git clone`, before the first `cargo run --features whisper-stt`
#   - the unzipped portable bundle, when you grabbed the small bundle (the
#     one built with `package-portable.ps1 -SkipWhisper`) and need voice
#     control to work
#
# Default model is ggml-base.en.bin (~148 MB). It's the right balance for
# real-time PTT on a modern CPU. -Tiny picks the ~75 MB ggml-tiny.en.bin
# instead — smaller download, faster, less accurate. -Small picks the
# ~466 MB ggml-small.en.bin — bigger download, slower, more accurate.
#
# Idempotent: skips the download if the target file already exists with a
# plausible size, unless -Force is passed.
#
# Usage:
#   .\scripts\install-whisper-model.ps1            # base.en (default)
#   .\scripts\install-whisper-model.ps1 -Tiny      # tiny.en
#   .\scripts\install-whisper-model.ps1 -Small     # small.en
#   .\scripts\install-whisper-model.ps1 -Force     # re-download even if present

param(
    [switch]$Tiny,
    [switch]$Small,
    [switch]$Force
)

$ErrorActionPreference = "Stop"

# Pick the variant. -Tiny / -Small win over the default; if both passed,
# Small wins (it's the more deliberate choice).
if ($Small) {
    $name      = "ggml-small.en.bin"
    $expectMin = 400MB
} elseif ($Tiny) {
    $name      = "ggml-tiny.en.bin"
    $expectMin = 60MB
} else {
    $name      = "ggml-base.en.bin"
    $expectMin = 130MB
}

$url   = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/$name"

# Resolve ./models/ relative to the script's parent dir (= repo root for
# checkouts, bundle root when run from a portable zip).
$modelsDir = Join-Path (Split-Path -Parent $PSScriptRoot) "models"
if (-not (Test-Path $modelsDir)) {
    New-Item -ItemType Directory -Force -Path $modelsDir | Out-Null
}
$dest = Join-Path $modelsDir $name

# Skip if already present at a plausible size — guards against re-downloading
# the model when re-running the script (idempotent), but a partial / truncated
# file from a previous failed download will be re-fetched because it falls
# under the size threshold.
if ((Test-Path $dest) -and -not $Force) {
    $sizeMB = (Get-Item $dest).Length / 1MB
    if ((Get-Item $dest).Length -ge $expectMin) {
        Write-Host ("[install-whisper-model] {0} already present ({1:N0} MB) -- skipping. Pass -Force to re-download." -f $name, $sizeMB)
        exit 0
    } else {
        Write-Warning ("[install-whisper-model] {0} exists but is suspiciously small ({1:N1} MB) -- re-downloading." -f $name, $sizeMB)
    }
}

Write-Host "[install-whisper-model] downloading $name from HuggingFace..."
Write-Host "                        $url"
Write-Host "                        -> $dest"
Write-Host ""

# Use BITS when available for resumable + progress; fall back to
# Invoke-WebRequest. BITS is faster on flaky connections and shows a real
# progress bar; IWR works everywhere PowerShell 5+ runs.
try {
    Start-BitsTransfer -Source $url -Destination $dest -ErrorAction Stop
} catch {
    Write-Warning "[install-whisper-model] BITS failed ($_), falling back to Invoke-WebRequest"
    # IWR's default progress UI is brutally slow on PS 5; turning it off
    # via $ProgressPreference triples the throughput.
    $oldPref = $ProgressPreference
    $ProgressPreference = 'SilentlyContinue'
    try {
        Invoke-WebRequest -Uri $url -OutFile $dest -UseBasicParsing
    } finally {
        $ProgressPreference = $oldPref
    }
}

$finalSizeMB = (Get-Item $dest).Length / 1MB
if ((Get-Item $dest).Length -lt $expectMin) {
    Remove-Item $dest -Force
    throw "Downloaded file was too small ({0:N1} MB) -- HuggingFace may have returned an error page. Re-run." -f $finalSizeMB
}

Write-Host ""
Write-Host ("[install-whisper-model] OK -- {0} ({1:N0} MB)" -f $dest, $finalSizeMB)
