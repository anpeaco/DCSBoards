# One-shot Piper installer for DCS Kneeboard.
#
# Downloads the latest piper_windows_amd64.zip release from GitHub,
# extracts piper.exe + its DLLs + espeak-ng-data into models/piper/.
# Does not touch any voices you may already have in models/piper/voices/.
#
# Usage from the repo root:
#   .\scripts\install-piper.ps1

$ErrorActionPreference = "Stop"

$repoRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
$target   = Join-Path $repoRoot "models/piper"
$tmpDir   = Join-Path $env:TEMP ("piper-install-" + [Guid]::NewGuid().ToString("N").Substring(0,8))
$tmpZip   = Join-Path $tmpDir "piper.zip"

New-Item -ItemType Directory -Force -Path $tmpDir | Out-Null
New-Item -ItemType Directory -Force -Path $target | Out-Null

Write-Host "Querying latest Piper release..."
$api = "https://api.github.com/repos/rhasspy/piper/releases/latest"
$headers = @{ "User-Agent" = "DCSBoards-piper-installer" }
$release = Invoke-RestMethod -Uri $api -Headers $headers
$asset = $release.assets |
    Where-Object { $_.name -like "*windows*amd64*.zip" } |
    Select-Object -First 1
if (-not $asset) {
    Write-Error "No piper_windows_amd64.zip asset in latest release. See $($release.html_url)"
    exit 1
}

Write-Host "Downloading $($asset.name) ($([math]::Round($asset.size / 1MB, 1)) MB) from release '$($release.tag_name)'..."
Invoke-WebRequest -Uri $asset.browser_download_url -OutFile $tmpZip -UseBasicParsing

Write-Host "Extracting..."
$extractDir = Join-Path $tmpDir "extracted"
Expand-Archive -Path $tmpZip -DestinationPath $extractDir -Force

# The zip wraps its contents in a `piper\` top-level directory.
# Find piper.exe wherever it landed and copy that whole directory.
$piperExe = Get-ChildItem -Path $extractDir -Recurse -Filter piper.exe | Select-Object -First 1
if (-not $piperExe) {
    Write-Error "piper.exe not found inside the downloaded zip. Layout:"
    Get-ChildItem -Path $extractDir -Recurse | Select-Object FullName | Format-Table -AutoSize
    exit 1
}

$srcDir = Split-Path $piperExe.FullName -Parent
Write-Host "Copying from $srcDir -> $target"
# Mirror everything but leave voices/ + cache/ in target untouched.
Copy-Item -Path (Join-Path $srcDir "*") -Destination $target -Recurse -Force

Write-Host "Cleaning up..."
Remove-Item -Path $tmpDir -Recurse -Force

# --- Patch espeak-ng.dll so it stops looking for /usr/share/espeak-ng-data ---
# The DLL has the Linux data path baked in at compile time and ignores
# --espeak_data / ESPEAK_DATA_PATH. We find the literal string in the
# binary and replace it with a cwd-relative path the same length.
# Piper.exe is launched with cwd = its install dir, so `.\espeak-ng-data`
# resolves correctly.
$dll = Join-Path $target "espeak-ng.dll"
if (Test-Path $dll) {
    Write-Host "Patching espeak-ng.dll data path..."
    $bytes = [System.IO.File]::ReadAllBytes($dll)
    $needle = [System.Text.Encoding]::ASCII.GetBytes("/usr/share/espeak-ng-data")
    $replacement = [System.Text.Encoding]::ASCII.GetBytes(".\espeak-ng-data") + [byte[]](@(0) * 9)
    if ($replacement.Length -ne $needle.Length) {
        Write-Error "Patch payload size mismatch (needle=$($needle.Length), replacement=$($replacement.Length))."
        exit 1
    }

    $hits = 0
    $i = 0
    while ($i -le $bytes.Length - $needle.Length) {
        $match = $true
        for ($j = 0; $j -lt $needle.Length; $j++) {
            if ($bytes[$i + $j] -ne $needle[$j]) { $match = $false; break }
        }
        if ($match) {
            for ($j = 0; $j -lt $replacement.Length; $j++) {
                $bytes[$i + $j] = $replacement[$j]
            }
            $hits++
            $i += $needle.Length
        } else {
            $i++
        }
    }

    if ($hits -eq 0) {
        Write-Warning "espeak-ng.dll did not contain the expected hardcoded path."
        Write-Warning "Piper may still fail with 'phontab not found'. If so, this script needs updating."
    } else {
        [System.IO.File]::WriteAllBytes($dll, $bytes)
        Write-Host "Patched $hits occurrence(s) of the hardcoded data path."
    }
} else {
    Write-Warning "espeak-ng.dll not found at $dll — skipped patch."
}

Write-Host ""
Write-Host "Done. Layout:"
Get-ChildItem -Path $target |
    Select-Object Mode, @{N='Size';E={ if ($_.PSIsContainer) { '<DIR>' } else { '{0,8:N0}' -f $_.Length }}}, Name |
    Format-Table -AutoSize

Write-Host ""
if (Test-Path (Join-Path $target "piper.exe")) {
    Write-Host "piper.exe is in place."
    Write-Host "Next: drop a voice into models/piper/voices/ — both the .onnx and .onnx.json."
    Write-Host "Voice catalogue + previews: https://rhasspy.github.io/piper-samples/"
} else {
    Write-Warning "Install finished but piper.exe is not at $target\piper.exe. Check the layout above."
}
