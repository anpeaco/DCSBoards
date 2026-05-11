# Undo any leftover shims from earlier attempts to make Piper work:
#   - SUBST drive letter pointing at %TEMP%\dcs-kneeboard-piper-shim
#   - %TEMP%\dcs-kneeboard-piper-shim\... directory + its junction
#   - C:\usr\share\espeak-ng-data junction (if you ever ran fix-piper-espeak.ps1)
#
# Safe to run multiple times; non-existent shims are silently ignored.
# No admin needed for the SUBST + temp-dir cleanup. Removing
# C:\usr\share\espeak-ng-data DOES need admin — the script skips it
# without complaining if it can't.

$ErrorActionPreference = "Stop"

# 1) Any SUBST drive whose target is our temp shim
$substOutput = & cmd /c subst 2>&1
foreach ($line in $substOutput) {
    if ($line -match '^\s*([A-Z]):\\?\s*=>\s*(.+?)\s*$') {
        $drive  = $matches[1]
        $target = $matches[2]
        if ($target -like "*dcs-kneeboard-piper-shim*") {
            Write-Host "Removing SUBST drive ${drive}: -> $target"
            & cmd /c "subst ${drive}: /D" | Out-Null
        }
    }
}

# 2) Temp shim directory + the junction inside it
$shim = Join-Path $env:TEMP "dcs-kneeboard-piper-shim"
if (Test-Path $shim) {
    Write-Host "Removing $shim"
    # Use cmd's rmdir so junctions are deleted as junctions, not traversed.
    & cmd /c rmdir /S /Q `"$shim`" | Out-Null
}

# 3) C:\usr\share\espeak-ng-data — only delete if it's a junction we created,
# never a real install. Test by checking for a ReparsePoint attribute.
$legacy = "C:\usr\share\espeak-ng-data"
if (Test-Path $legacy) {
    $item = Get-Item $legacy
    $isLink = ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0
    if ($isLink) {
        try {
            & cmd /c rmdir `"$legacy`" | Out-Null
            Write-Host "Removed junction $legacy"
            # Tidy up empty parent dirs we may have created.
            foreach ($p in @("C:\usr\share", "C:\usr")) {
                if ((Test-Path $p) -and (Get-ChildItem $p -Force | Measure-Object).Count -eq 0) {
                    Remove-Item $p -Force -ErrorAction SilentlyContinue
                }
            }
        } catch {
            Write-Warning "Couldn't remove $legacy (try running this script as administrator)."
        }
    } else {
        Write-Host "Keeping $legacy — it's a real directory, not a junction."
    }
}

Write-Host ""
Write-Host "Cleanup complete. Open a new Explorer window — any phantom drives should be gone."
