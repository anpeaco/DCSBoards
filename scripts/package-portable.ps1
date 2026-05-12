# Build the portable test bundle and zip it.
#
# Output:
#   release/DCSBoards-Portable/     (folder you can copy to another machine)
#   release/DCSBoards-Portable.zip  (same thing, zipped)
#
# Prereqs:
#   - Rust toolchain + LLVM (for the whisper-stt feature) installed.
#   - models/ggml-base.en.bin present (Whisper STT model).
#   - models/piper/ populated by scripts/install-piper.ps1, plus at least one
#     voice in models/piper/voices/. Both can be skipped with -SkipPiper /
#     -SkipWhisper if you only want the WinRT-TTS bundle.
#
# Usage:
#   .\scripts\package-portable.ps1            # full bundle, rebuilds exe
#   .\scripts\package-portable.ps1 -NoBuild   # reuse target/release/dcs-kneeboard.exe
#   .\scripts\package-portable.ps1 -SkipPiper # smaller bundle, WinRT TTS only

param(
    [switch]$NoBuild,
    [switch]$SkipPiper,
    [switch]$SkipWhisper
)

$ErrorActionPreference = "Stop"

$repoRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
$bundle   = Join-Path $repoRoot "release/DCSBoards-Portable"
$zip      = Join-Path $repoRoot "release/DCSBoards-Portable.zip"

# --- 1. Build the release exe ---
#
# Pin whisper.cpp to a portable x86-64-v3 baseline (AVX2 + FMA + F16C). The
# default GGML_NATIVE=ON would emit AVX-512 on an AMD/AVX-512 build host;
# the resulting binary crashes silently on Intel Alder Lake and any other
# CPU without AVX-512. whisper-rs-sys 0.15 forwards any env var starting
# with GGML_/WHISPER_/CMAKE_ to CMake as a -D define (see its build.rs).
# x86-64-v3 covers every Intel CPU from Haswell (2013) and every AMD CPU
# from Zen 1 (2017) — a realistic floor for DCS-capable hardware.
#
# A cargo clean of whisper-rs-sys is required: CMake caches previous flags,
# so without it the env-var change is a no-op against an existing build.
if (-not $NoBuild) {
    Write-Host "Building dcs-kneeboard (release, whisper-stt)..."
    Push-Location $repoRoot
    $ggmlVars = [ordered]@{
        GGML_NATIVE      = 'OFF'
        GGML_AVX         = 'ON'
        GGML_AVX2        = 'ON'
        GGML_FMA         = 'ON'
        GGML_F16C        = 'ON'
        GGML_AVX512      = 'OFF'
        GGML_AVX512_VBMI = 'OFF'
        GGML_AVX512_VNNI = 'OFF'
    }
    $prevEnv = @{}
    foreach ($k in $ggmlVars.Keys) {
        $prevEnv[$k] = [Environment]::GetEnvironmentVariable($k, 'Process')
        [Environment]::SetEnvironmentVariable($k, $ggmlVars[$k], 'Process')
    }
    try {
        Write-Host "  forcing portable baseline: GGML_NATIVE=OFF, AVX2/FMA/F16C=ON, AVX512=OFF"
        # `cargo clean -p whisper-rs-sys` only removes the compiled .rlib in deps/;
        # the CMake build directory under target/<profile>/build/whisper-rs-sys-*/out
        # is left intact, so the next build reuses its cached -DGGML_* flags and
        # the env-var change is a silent no-op. Wipe the build directory directly.
        cargo clean -p whisper-rs-sys
        if ($LASTEXITCODE -ne 0) { throw "cargo clean failed (exit $LASTEXITCODE)" }
        Get-ChildItem (Join-Path $repoRoot 'target/release/build') -Filter 'whisper-rs-sys-*' -ErrorAction SilentlyContinue |
            ForEach-Object { Write-Host "  wiping $($_.FullName)"; Remove-Item -Recurse -Force $_.FullName }
        cargo build --release --features whisper-stt
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed (exit $LASTEXITCODE)" }
    } finally {
        foreach ($k in $ggmlVars.Keys) {
            [Environment]::SetEnvironmentVariable($k, $prevEnv[$k], 'Process')
        }
        Pop-Location
    }
} else {
    Write-Host "Skipping build (--NoBuild)."
}

$exeSrc = Join-Path $repoRoot "target/release/dcs-kneeboard.exe"
if (-not (Test-Path $exeSrc)) {
    throw "Expected $exeSrc -- run without -NoBuild first."
}

# --- 2. Recreate the bundle directory ---
Write-Host "Wiping $bundle..."
if (Test-Path $bundle) { Remove-Item -Recurse -Force $bundle }
New-Item -ItemType Directory -Force -Path $bundle | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $bundle "models") | Out-Null

# --- 3. App files ---
Write-Host "Copying app + configs..."
Copy-Item $exeSrc                                    (Join-Path $bundle "dcs-kneeboard.exe")
Copy-Item (Join-Path $repoRoot "config.toml")        $bundle
Copy-Item (Join-Path $repoRoot "pronunciation.toml") $bundle
$aliasesSrc = Join-Path $repoRoot "query_aliases.toml"
if (Test-Path $aliasesSrc) {
    Copy-Item $aliasesSrc $bundle
} else {
    Write-Warning "query_aliases.toml missing from repo root -- voice-query aliases will be unavailable in the bundle."
}

# --- 4. Sample pages ---
$pagesSrc = Join-Path $repoRoot "pages-sample"
if (Test-Path $pagesSrc) {
    Write-Host "Copying pages-sample/..."
    Copy-Item -Recurse $pagesSrc (Join-Path $bundle "pages-sample")
} else {
    Write-Warning "pages-sample/ not found at repo root -- bundle will have no checklists."
}

# --- 5. Whisper STT model ---
if (-not $SkipWhisper) {
    $whisperSrc = Join-Path $repoRoot "models/ggml-base.en.bin"
    if (Test-Path $whisperSrc) {
        Write-Host "Copying Whisper model (148 MB)..."
        Copy-Item $whisperSrc (Join-Path $bundle "models/ggml-base.en.bin")
    } else {
        Write-Warning "models/ggml-base.en.bin missing -- STT will not work on the test machine."
    }
}

# --- 6. Piper TTS (engine + dlls + espeak-ng-data + voices) ---
if (-not $SkipPiper) {
    $piperSrc = Join-Path $repoRoot "models/piper"
    if (Test-Path (Join-Path $piperSrc "piper.exe")) {
        Write-Host "Copying Piper (engine, DLLs, espeak-ng-data, voices)..."
        $piperDst = Join-Path $bundle "models/piper"
        New-Item -ItemType Directory -Force -Path $piperDst | Out-Null
        # Mirror everything except the runtime cache (Piper recreates it).
        Get-ChildItem $piperSrc -Force | Where-Object { $_.Name -ne "cache" } | ForEach-Object {
            Copy-Item -Recurse -Force $_.FullName (Join-Path $piperDst $_.Name)
        }
        $voiceCount = (Get-ChildItem (Join-Path $piperDst "voices") -Filter *.onnx -ErrorAction SilentlyContinue | Measure-Object).Count
        if ($voiceCount -eq 0) {
            Write-Warning "No .onnx voices in models/piper/voices/ -- Piper TTS will have nothing to speak."
        } else {
            Write-Host "  $voiceCount voice(s) included."
        }
    } else {
        Write-Warning "models/piper/piper.exe missing -- skipping Piper. Run scripts/install-piper.ps1 first."
    }
}

# --- 7. README ---
Write-Host "Writing README.txt..."
$readme = @'
DCS Kneeboard -- Portable Test Build
====================================

How to run
----------
1. Unzip this folder anywhere (e.g. Desktop\DCSBoards).
2. Double-click `dcs-kneeboard.exe`.

A console window opens with debug logs and the overlay appears.

Quick test
----------
- Hover the top edge: title bar slides in.
- Hover the bottom edge: footer with nav buttons.
- Hover the left edge: tab strip.
- Click the gear icon for Settings. BINDINGS lets you capture HOTAS /
  keyboard shortcuts -- click a row and press a button.
- Click the mic icon for the full voice-command list.

Voice control
-------------
Bind a HOTAS button (or keyboard key) to "Push-to-talk" or "Hot mic (toggle)"
in BINDINGS, then:

- Push-to-talk: hold the button, speak, release. Audio is transcribed
  and dispatched (e.g. "next page" -> advance one page).
- Hot mic: press to start listening, press again to stop. Commands are
  detected and run as you speak.

The transcript pill near the top-right shows what was heard.

Three layers of voice control:

- COMMANDS - literal phrases like "next", "previous heading", "what
  was that". Full list: open the voice-commands dialog (mic icon on
  the bottom edge, "Voice commands").
- VOICE QUERIES - free-form intents:
    "go to page 3"        -- jump to a specific page
    "go to AGM-65"        -- find a section by name (fuzzy match)
    "go to the JDAM tab"  -- switch tab
    "list sections"       -- TTS reads the section headers
- PHONETIC ALIASES - rewrite spoken aliases to canonical designators
  before fuzzy matching. Defaults: "Maverick" -> "AGM-65",
  "Sidewinder" -> "AIM-9", "Slammer" -> "AIM-120", "HARM" -> "AGM-88",
  "Mark 82" -> "MK-82". Edit query_aliases.toml + press F5 to add your
  own.

F5 reloads pronunciation.toml AND query_aliases.toml without restart.

Text-to-speech
--------------
Settings > TEXT-TO-SPEECH lets you pick the engine:

- WinRT (system) -- uses installed Windows voices. Instant, OK quality.
- Piper (neural) -- open-source local neural TTS. Better voices, slightly
  higher latency. Voices in models\piper\voices\ appear automatically;
  this bundle ships at least one English voice.

Speed and Volume sliders affect both engines. Press Test to hear a
sample.

Default keyboard shortcuts (when the window has focus)
------------------------------------------------------
Space            Next item
Backspace        Previous item
R                Play / pause
H / Shift+H      Next / previous heading
PageDn / PageUp  Next / previous page
F5               Reload pronunciation.toml
Esc              Close panels / stop speech

System requirements
-------------------
- Windows 10 / 11 x64
- A microphone (any default input device works)
- No installation required; nothing is written outside this folder.

Files in this bundle
--------------------
dcs-kneeboard.exe                The app
config.toml                      Aircraft + tab list (edit to add tabs)
pronunciation.toml               TTS abbreviation overrides
pages-sample\                    Sample F-16C checklists used by the
                                 "checklists" tab
models\ggml-base.en.bin          Whisper STT model (~148 MB)
models\piper\piper.exe + DLLs    Piper TTS engine
models\piper\espeak-ng-data\     Piper phoneme tables
models\piper\voices\*.onnx       Piper voices (with matching .onnx.json)

The app creates `settings.toml` on first run to remember your bindings,
window position, audio device, etc. Delete it to reset to defaults.

Troubleshooting
---------------
- No voice match: Settings > BINDINGS > tick "Test mode" to confirm the
  button you're pressing is registered.
- TTS silent (WinRT): Settings > Time & Language > Speech > Manage
  voices in Windows lets you install higher quality ones.
- TTS silent (Piper): if Test produces nothing, run
  `models\piper\piper.exe --model models\piper\voices\<voice>.onnx
  --output_file out.wav` in a shell to see the error directly.
- Crash on startup: check the console for the offending log line.
'@
Set-Content -Path (Join-Path $bundle "README.txt") -Value $readme -Encoding UTF8

# --- 8. Zip it ---
Write-Host "Zipping -> $zip..."
if (Test-Path $zip) { Remove-Item -Force $zip }
Compress-Archive -Path (Join-Path $bundle "*") -DestinationPath $zip -CompressionLevel Optimal

$zipSize = (Get-Item $zip).Length
Write-Host ""
Write-Host ("Done.  {0}  ({1:N1} MB)" -f $zip, ($zipSize / 1MB))
Write-Host ("       {0}" -f $bundle)
