# Drive the dcs-kneeboard window through several UI states and capture each
# to docs/screenshots/. App must already be running.
#
# Captures:
#   overview-clean.png    — kneeboard page, no chrome
#   overview-chrome.png   — title bar + footer visible
#   settings.png          — Settings panel open
#   voice-commands.png    — Voice Commands panel open

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
    [DllImport("user32.dll")]
    public static extern bool SetCursorPos(int X, int Y);
    [DllImport("user32.dll")]
    public static extern void mouse_event(uint dwFlags, uint dx, uint dy, uint dwData, IntPtr dwExtraInfo);
    [DllImport("user32.dll")]
    public static extern void keybd_event(byte bVk, byte bScan, uint dwFlags, IntPtr dwExtraInfo);
    public const uint MOUSEEVENTF_LEFTDOWN = 0x0002;
    public const uint MOUSEEVENTF_LEFTUP   = 0x0004;
    public const uint KEYEVENTF_KEYUP      = 0x0002;
    public const byte VK_ESCAPE            = 0x1B;
    [StructLayout(LayoutKind.Sequential)]
    public struct RECT { public int Left, Top, Right, Bottom; }
}
"@

function Find-AppWindow {
    $script:found = [IntPtr]::Zero
    $cb = [DcsCap+EnumWindowsProc]{
        param($h, $l)
        if (-not [DcsCap]::IsWindowVisible($h)) { return $true }
        $len = [DcsCap]::GetWindowTextLength($h)
        if ($len -eq 0) { return $true }
        $sb = New-Object System.Text.StringBuilder ($len + 1)
        [void][DcsCap]::GetWindowText($h, $sb, $sb.Capacity)
        if ($sb.ToString() -eq "DCS Kneeboard") {
            $script:found = $h
            return $false
        }
        return $true
    }
    [void][DcsCap]::EnumWindows($cb, [IntPtr]::Zero)
    return $script:found
}

function Get-WindowRect($hwnd) {
    $r = New-Object DcsCap+RECT
    [void][DcsCap]::GetWindowRect($hwnd, [ref]$r)
    return $r
}

function Move-CursorTo($x, $y) {
    [void][DcsCap]::SetCursorPos($x, $y)
}

function Click-At($x, $y) {
    Move-CursorTo $x $y
    Start-Sleep -Milliseconds 80
    [DcsCap]::mouse_event([DcsCap]::MOUSEEVENTF_LEFTDOWN, 0, 0, 0, [IntPtr]::Zero)
    Start-Sleep -Milliseconds 60
    [DcsCap]::mouse_event([DcsCap]::MOUSEEVENTF_LEFTUP, 0, 0, 0, [IntPtr]::Zero)
}

function Press-Esc {
    [DcsCap]::keybd_event([DcsCap]::VK_ESCAPE, 0, 0, [IntPtr]::Zero)
    Start-Sleep -Milliseconds 30
    [DcsCap]::keybd_event([DcsCap]::VK_ESCAPE, 0, [DcsCap]::KEYEVENTF_KEYUP, [IntPtr]::Zero)
}

function Capture($name, $rect) {
    $width  = $rect.Right - $rect.Left
    $height = $rect.Bottom - $rect.Top
    $bmp = New-Object System.Drawing.Bitmap $width, $height
    $g   = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($rect.Left, $rect.Top, 0, 0, $bmp.Size)
    $outDir = Join-Path $PSScriptRoot "..\docs\screenshots"
    if (-not (Test-Path $outDir)) {
        New-Item -ItemType Directory -Path $outDir -Force | Out-Null
    }
    $outPath = Join-Path $outDir "$name.png"
    $bmp.Save($outPath, [System.Drawing.Imaging.ImageFormat]::Png)
    $g.Dispose(); $bmp.Dispose()
    Write-Host "  -> $outPath ($width x $height)"
}

# --- find window + bring to foreground ---
$hwnd = [IntPtr]::Zero
for ($i = 0; $i -lt 20; $i++) {
    $hwnd = Find-AppWindow
    if ($hwnd -ne [IntPtr]::Zero) { break }
    Start-Sleep -Milliseconds 500
}
if ($hwnd -eq [IntPtr]::Zero) {
    Write-Error "App window not found. Start dcs-kneeboard first."
    exit 1
}
[void][DcsCap]::SetForegroundWindow($hwnd)
Start-Sleep -Milliseconds 300
$rect = Get-WindowRect $hwnd
$cx = ($rect.Left + $rect.Right) / 2
"Window at $($rect.Left),$($rect.Top) -> $($rect.Right),$($rect.Bottom)"

# 1) Clean overview — move cursor far away so no chrome shows.
Move-CursorTo ($rect.Left - 50) ($rect.Top + 200)
Start-Sleep -Milliseconds 600
"Capturing overview-clean..."
Capture "overview-clean" (Get-WindowRect $hwnd)

# 2) Chrome visible — hover near the top so title bar fades in.
Move-CursorTo $cx ($rect.Top + 5)
Start-Sleep -Milliseconds 500
"Capturing overview-chrome..."
Capture "overview-chrome" (Get-WindowRect $hwnd)

# 3) Settings — click the gear (x = right - 33, y = top + 11).
Click-At ($rect.Right - 33) ($rect.Top + 11)
Start-Sleep -Milliseconds 800
"Capturing settings..."
# Move cursor off so it's not on top of the panel.
Move-CursorTo ($rect.Left - 50) ($rect.Top + 200)
Start-Sleep -Milliseconds 200
Capture "settings" (Get-WindowRect $hwnd)

# Close settings and open voice commands.
Press-Esc
Start-Sleep -Milliseconds 400
[void][DcsCap]::SetForegroundWindow($hwnd)
Start-Sleep -Milliseconds 200
# Voice icon sits at x = right - 55 (66 - 11 since IconCell is 22 wide at x=parent.width-66).
$rect = Get-WindowRect $hwnd
Move-CursorTo $cx ($rect.Top + 5)
Start-Sleep -Milliseconds 400
Click-At ($rect.Right - 55) ($rect.Top + 11)
Start-Sleep -Milliseconds 800
"Capturing voice-commands..."
Move-CursorTo ($rect.Left - 50) ($rect.Top + 200)
Start-Sleep -Milliseconds 200
Capture "voice-commands" (Get-WindowRect $hwnd)

# Restore — Esc to close panel.
Press-Esc
Write-Host "Done."
