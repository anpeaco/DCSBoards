//! VR-side kneeboard rendering. Bypasses Slint entirely (#30 phase 2):
//! we don't get a buffer view of slint's femtovg-rendered window
//! without re-architecting the whole UI to SoftwareRenderer, and the
//! pilot in VR doesn't interact with the chrome anyway — they navigate
//! by HOTAS + voice. So we just rasterise the page PNG + a highlight
//! rectangle directly into an RGBA buffer that SteamVR's SetOverlayRaw
//! can take.
//!
//! Phase 2 ships page + highlight only. A later phase can add text
//! overlays for pills (Listening, Transcribing, transcript) if real
//! pilots ask for them — early hypothesis is they won't, since the
//! audio side already gives them.

use anyhow::{Context, Result};
use image::{Rgba, RgbaImage};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Decoded-PNG cache keyed by file path. Decoding a 1358x2037 PNG is
/// 50–100 ms on cold start; we don't want that hitch at every frame
/// when the cursor is on the same page as last frame.
struct PageImageCache {
    last_path: Option<PathBuf>,
    last_image: Option<RgbaImage>,
}

static CACHE: Mutex<PageImageCache> = Mutex::new(PageImageCache {
    last_path: None,
    last_image: None,
});

/// Take the current cursor's page PNG path + its bbox-in-image-space
/// and produce the overlay frame. Bbox coords are in the original PNG
/// pixel space (x, y, w, h); the highlight is drawn directly on top
/// at native scale. Output is the page image with the highlight
/// composited on it. Sized = original PNG dimensions.
pub fn render_kneeboard_frame(page_path: &Path, bbox: Option<[u32; 4]>) -> Result<RgbaImage> {
    let mut frame = load_page_cached(page_path)?;
    if let Some([x, y, w, h]) = bbox {
        draw_highlight(&mut frame, x, y, w, h);
    }
    Ok(frame)
}

/// Load + cache the page PNG. Returns a fresh clone of the cached
/// buffer each call so the caller can mutate (draw highlight on top).
/// Cache size is 1 — we trade memory for re-decode cost on every
/// page change, which is negligible (single-digit ms after first hit).
fn load_page_cached(path: &Path) -> Result<RgbaImage> {
    let mut cache = CACHE
        .lock()
        .map_err(|_| anyhow::anyhow!("page-cache mutex poisoned"))?;
    let path_buf = path.to_path_buf();
    let cached = cache
        .last_path
        .as_ref()
        .map(|p| p == &path_buf)
        .unwrap_or(false);
    if cached {
        if let Some(img) = cache.last_image.as_ref() {
            return Ok(img.clone());
        }
    }
    let decoded = image::open(path)
        .with_context(|| format!("decode page PNG {}", path.display()))?
        .to_rgba8();
    cache.last_path = Some(path_buf);
    cache.last_image = Some(decoded.clone());
    Ok(decoded)
}

/// Draw a 2px yellow border + ~13 % alpha yellow fill highlight on
/// `frame`, clipped to the image bounds. Matches the desktop UI's
/// highlight look (ffcc33 border / ffcc33 ~13 % fill) so VR and
/// desktop modes feel identical to a pilot toggling between them.
fn draw_highlight(frame: &mut RgbaImage, x: u32, y: u32, w: u32, h: u32) {
    let (img_w, img_h) = (frame.width(), frame.height());
    if x >= img_w || y >= img_h || w == 0 || h == 0 {
        return;
    }
    let x_end = (x + w).min(img_w);
    let y_end = (y + h).min(img_h);
    let border_thickness = 2;
    let border_color = Rgba([0xff, 0xcc, 0x33, 0xff]);
    let fill_color = Rgba([0xff, 0xcc, 0x33, 0x22]); // ~13 % alpha

    // Fill: alpha-blend over existing pixels.
    for py in y..y_end {
        for px in x..x_end {
            let dst = frame.get_pixel_mut(px, py);
            *dst = blend(*dst, fill_color);
        }
    }
    // Border (top + bottom strips, then left + right).
    for py in y..y.saturating_add(border_thickness).min(y_end) {
        for px in x..x_end {
            *frame.get_pixel_mut(px, py) = border_color;
        }
    }
    for py in y_end.saturating_sub(border_thickness).max(y)..y_end {
        for px in x..x_end {
            *frame.get_pixel_mut(px, py) = border_color;
        }
    }
    for py in y..y_end {
        for px in x..x.saturating_add(border_thickness).min(x_end) {
            *frame.get_pixel_mut(px, py) = border_color;
        }
        for px in x_end.saturating_sub(border_thickness).max(x)..x_end {
            *frame.get_pixel_mut(px, py) = border_color;
        }
    }
}

/// Standard "src over dst" alpha blend. Premultiplies on the fly —
/// good enough for a 13 % yellow fill over an opaque page background;
/// not a colour-managed compositor.
fn blend(dst: Rgba<u8>, src: Rgba<u8>) -> Rgba<u8> {
    let sa = src[3] as u32;
    let inv = 255 - sa;
    let r = (src[0] as u32 * sa + dst[0] as u32 * inv) / 255;
    let g = (src[1] as u32 * sa + dst[1] as u32 * inv) / 255;
    let b = (src[2] as u32 * sa + dst[2] as u32 * inv) / 255;
    Rgba([r as u8, g as u8, b as u8, 0xff])
}
