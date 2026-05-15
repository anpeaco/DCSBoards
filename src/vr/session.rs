//! Phase-1 VR session: init OpenVR, create one IVROverlay, upload a
//! recognisable test pattern, place it ~0.6 m forward of the play-area
//! origin. Sufficient to prove SteamVR runtime + linkage + the openvr
//! crate's overlay surface area work end-to-end. Phase 2 swaps the test
//! pattern for actual Slint frames.

use anyhow::{anyhow, Context as AnyhowContext, Result};
use openvr::overlay::OverlayHandle;
use openvr::pose::Matrix3x4;
use openvr::{ApplicationType, Context as OvrContext, TrackingUniverseOrigin};

/// Edge length (px) of the test pattern texture.
const TEST_TEXTURE_SIZE: u32 = 256;

/// Default world-locked pose: 0.6 m forward of the standing zero-pose,
/// 0.20 m below eye level, 0.30 m wide. Tuned for "obviously hovering
/// in front of you" during the spike; phase 4 makes this user-controlled.
const DEFAULT_FORWARD_M: f32 = 0.6;
const DEFAULT_DROP_M: f32 = -0.2;
const DEFAULT_WIDTH_M: f32 = 0.30;

/// Holds the OpenVR runtime + overlay handle for the lifetime of VR
/// mode. Dropping the session triggers Context::drop which calls
/// VR_Shutdown and implicitly destroys all overlays we created.
///
/// The openvr crate's `Overlay` struct is `&'static` over the
/// IVROverlay function-table — we don't store it because every call
/// needs `&mut self` and we'd rather grab a fresh `Overlay` from the
/// Context per-call than wrap it in a RefCell.
pub struct VrSession {
    // Held to keep the OpenVR runtime alive; Drop on Context calls
    // VR_Shutdown which implicitly destroys the overlay we created.
    // Phase 2 starts reading these to push new pixels each tick.
    #[allow(dead_code)]
    ctx: OvrContext,
    #[allow(dead_code)]
    overlay_handle: OverlayHandle,
}

/// Phase-1 entry point: initialise OpenVR as an overlay-class app,
/// create one overlay, upload the test pattern, place it world-locked,
/// show it. Returns the live session — keep alive while VR mode is on.
///
/// Errors out if SteamVR isn't running or the OpenVR runtime can't
/// hand us an IVROverlay. The caller treats this as non-fatal: the
/// desktop window keeps working.
pub fn init_test_pattern_session() -> Result<VrSession> {
    eprintln!("[vr] initialising OpenVR (overlay class)…");
    let ctx = unsafe { openvr::init(ApplicationType::Overlay) }
        .map_err(|e| anyhow!("OpenVR init failed: {e:?}"))
        .context("Is SteamVR running?")?;

    let mut overlay = ctx
        .overlay()
        .map_err(|e| anyhow!("OpenVR did not expose IVROverlay: {e:?}"))?;

    // Pre-null-terminate: openvr 0.9's create_overlay passes &str bytes
    // directly to the C API without inserting a NUL, so the runtime
    // would otherwise read past the slice. Workaround until the crate
    // grows a CString-aware path.
    let overlay_handle = overlay
        .create_overlay("dcsboards.spike\0", "DCS Kneeboard (VR spike)\0")
        .map_err(|e| anyhow!("create_overlay failed: {e:?}"))?;
    eprintln!("[vr] overlay created (handle = {:?})", overlay_handle.0);

    overlay
        .set_width(overlay_handle, DEFAULT_WIDTH_M)
        .map_err(|e| anyhow!("set_width failed: {e:?}"))?;

    let pixels = build_test_pattern();
    overlay
        .set_raw_data(
            overlay_handle,
            &pixels,
            TEST_TEXTURE_SIZE as usize,
            TEST_TEXTURE_SIZE as usize,
            4,
        )
        .map_err(|e| anyhow!("set_raw_data failed: {e:?}"))?;
    eprintln!(
        "[vr] uploaded {sz}x{sz} test pattern",
        sz = TEST_TEXTURE_SIZE
    );

    // World-locked pose. OpenVR's Matrix3x4 is row-major: rows are
    // (Rxx Rxy Rxz Tx), (Ryx Ryy Ryz Ty), (Rzx Rzy Rzz Tz).
    // Identity rotation + (0, DROP, -FORWARD) translation puts the
    // overlay in front of and slightly below the standing zero-pose.
    let transform = Matrix3x4([
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, DEFAULT_DROP_M],
        [0.0, 0.0, 1.0, -DEFAULT_FORWARD_M],
    ]);
    overlay
        .set_transform_absolute(overlay_handle, TrackingUniverseOrigin::Standing, &transform)
        .map_err(|e| anyhow!("set_transform_absolute failed: {e:?}"))?;

    overlay
        .set_visibility(overlay_handle, true)
        .map_err(|e| anyhow!("show overlay failed: {e:?}"))?;
    eprintln!("[vr] overlay shown — should be visible 0.6 m forward in HMD");

    Ok(VrSession {
        ctx,
        overlay_handle,
    })
}

/// 256×256 RGBA test pattern: bright yellow background, black
/// quadrant cross, red border. Picked to be unambiguous in a VR view
/// — if you see a yellow square with a black `+` and a red frame,
/// OpenVR found us.
fn build_test_pattern() -> Vec<u8> {
    let n = TEST_TEXTURE_SIZE as usize;
    let mut out = vec![0u8; n * n * 4];
    let yellow = [0xff, 0xcc, 0x33, 0xff];
    let black = [0x00, 0x00, 0x00, 0xff];
    let red = [0xff, 0x33, 0x33, 0xff];
    let border = 4;
    let cross_thickness = 4;
    let mid = n / 2;
    for y in 0..n {
        for x in 0..n {
            let on_border = x < border || x >= n - border || y < border || y >= n - border;
            let on_cross = (x + cross_thickness >= mid && x < mid + cross_thickness)
                || (y + cross_thickness >= mid && y < mid + cross_thickness);
            let rgba = if on_border {
                red
            } else if on_cross {
                black
            } else {
                yellow
            };
            let i = (y * n + x) * 4;
            out[i..i + 4].copy_from_slice(&rgba);
        }
    }
    out
}
