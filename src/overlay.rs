//! Win32 overlay glue. M7: WS_EX_TRANSPARENT click-through.
//!
//! We locate our own window via FindWindowW(title) so we don't have to plumb
//! a raw HWND through slint's window handle API. The title set in the .slint
//! file is "DCS Kneeboard" and is unique enough for the lookup to be safe.

#![cfg(windows)]

use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GetWindowLongPtrW, SetLayeredWindowAttributes, SetWindowLongPtrW, SetWindowPos,
    GWL_EXSTYLE, LWA_ALPHA, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    SWP_NOZORDER, WS_EX_LAYERED, WS_EX_TRANSPARENT,
};

const WINDOW_TITLE: &str = "DCS Kneeboard";

fn find_our_window() -> Option<HWND> {
    let mut wide: Vec<u16> = WINDOW_TITLE.encode_utf16().collect();
    wide.push(0);
    // windows 0.58 returns Result<HWND>; map to Option.
    unsafe { FindWindowW(PCWSTR::null(), PCWSTR(wide.as_ptr())) }.ok()
}

/// Set or clear WS_EX_TRANSPARENT on our window. With WS_EX_TRANSPARENT the
/// window receives no mouse/cursor input — clicks fall through to whatever
/// is behind us. WS_EX_LAYERED is required alongside it; slint already sets
/// it (we render with transparent backgrounds), so we just OR it in defensively.
pub fn set_click_through(enabled: bool) -> bool {
    let Some(hwnd) = find_our_window() else {
        eprintln!("[overlay] could not find our window — click-through skipped");
        return false;
    };
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let transparent_flag = (WS_EX_TRANSPARENT.0 | WS_EX_LAYERED.0) as isize;
        let new_style = if enabled {
            style | transparent_flag
        } else {
            // Only clear the transparent bit; leave LAYERED alone since slint
            // depends on it.
            style & !(WS_EX_TRANSPARENT.0 as isize)
        };
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, new_style);
        // SWP_FRAMECHANGED forces the cached style to refresh.
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
    }
    eprintln!("[overlay] click-through {}", if enabled { "ON" } else { "off" });
    true
}

/// Apply a uniform window opacity in the 0.3..=1.0 range. Sets WS_EX_LAYERED
/// (idempotent — slint already does for us, but click-through assumes the
/// flag is present too) then calls `SetLayeredWindowAttributes` with
/// `LWA_ALPHA`. Slint renders an opaque `#000` background so this fades the
/// whole UI uniformly without needing per-pixel alpha.
pub fn set_opacity(opacity: f32) -> bool {
    let alpha_byte = (opacity.clamp(0.3, 1.0) * 255.0).round() as u8;
    let Some(hwnd) = find_our_window() else {
        eprintln!("[overlay] could not find our window — opacity skipped");
        return false;
    };
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let layered = WS_EX_LAYERED.0 as isize;
        if style & layered == 0 {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style | layered);
        }
        if let Err(e) = SetLayeredWindowAttributes(hwnd, COLORREF(0), alpha_byte, LWA_ALPHA) {
            eprintln!("[overlay] SetLayeredWindowAttributes failed: {e}");
            return false;
        }
    }
    eprintln!("[overlay] opacity {alpha_byte}/255");
    true
}
