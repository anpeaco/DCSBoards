# Capture the dcs-kneeboard window to docs/screenshots/<name>.png
#
# Usage from the repo root:
#   .\scripts\capture-screenshot.ps1 overview
#   .\scripts\capture-screenshot.ps1 settings
#
# The app must be running. If a name isn't passed, "screenshot" is used.

param(
    [string]$Name = "screenshot"
)

Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
public class DcsCap {
    public delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);
    [DllImport("user32.dll")]
    public static extern bool EnumWindows(EnumWindowsProc enumProc, IntPtr lParam);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    public static extern int GetWindowText(IntPtr hWnd, StringBuilder lpString, int nMaxCount);
    [DllImport("user32.dll")]
    public static extern int GetWindowTextLength(IntPtr hWnd);
    [DllImport("user32.dll")]
    public static extern bool IsWindowVisible(IntPtr hWnd);
    [DllImport("user32.dll")]
    public static extern bool GetWindowRect(IntPtr hWnd, out RECT lpRect);
    [DllImport("user32.dll")]
    public static extern bool SetForegroundWindow(IntPtr hWnd);
    [StructLayout(LayoutKind.Sequential)]
    public struct RECT { public int Left, Top, Right, Bottom; }
}
"@

function Find-AppWindow {
    $found = [IntPtr]::Zero
    $cb = [DcsCap+EnumWindowsProc]{
        param($h, $l)
        if (-not [DcsCap]::IsWindowVisible($h)) { return $true }
        $len = [DcsCap]::GetWindowTextLength($h)
        if ($len -eq 0) { return $true }
        $sb = New-Object System.Text.StringBuilder ($len + 1)
        [void][DcsCap]::GetWindowText($h, $sb, $sb.Capacity)
        if ($sb.ToString() -eq "DCS Kneeboard") {
            $script:found = $h
            return $false  # stop enum
        }
        return $true
    }
    [void][DcsCap]::EnumWindows($cb, [IntPtr]::Zero)
    return $script:found
}

# Slint may take a beat to actually create the HWND after the process starts.
$hwnd = [IntPtr]::Zero
for ($i = 0; $i -lt 20; $i++) {
    $hwnd = Find-AppWindow
    if ($hwnd -ne [IntPtr]::Zero) { break }
    Start-Sleep -Milliseconds 500
}
if ($hwnd -eq [IntPtr]::Zero) {
    Write-Error "DCS Kneeboard window not found after 10s. Start the app first (cargo run --features whisper-stt)."
    exit 1
}

[void][DcsCap]::SetForegroundWindow($hwnd)
Start-Sleep -Milliseconds 150

$rect = New-Object DcsCap+RECT
[void][DcsCap]::GetWindowRect($hwnd, [ref]$rect)
$width  = $rect.Right - $rect.Left
$height = $rect.Bottom - $rect.Top
if ($width -le 0 -or $height -le 0) {
    Write-Error "Window has zero size (rect: $($rect.Left),$($rect.Top) -> $($rect.Right),$($rect.Bottom))."
    exit 1
}

$bmp = New-Object System.Drawing.Bitmap $width, $height
$g   = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($rect.Left, $rect.Top, 0, 0, $bmp.Size)

$outDir = Join-Path $PSScriptRoot "..\docs\screenshots"
if (-not (Test-Path $outDir)) {
    New-Item -ItemType Directory -Path $outDir -Force | Out-Null
}
$outPath = Join-Path $outDir "$Name.png"
$bmp.Save($outPath, [System.Drawing.Imaging.ImageFormat]::Png)
$g.Dispose()
$bmp.Dispose()

Write-Host "Saved $outPath ($width x $height)"
