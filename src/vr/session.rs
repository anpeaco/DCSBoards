//! VR session lifecycle: init OpenVR, create one IVROverlay, place it
//! world-locked, push pixel buffers to it via `submit_frame`. Phase 1
//! shipped a static test pattern; phase 2 (this file's current state)
//! exposes submit_frame so the main loop can push live kneeboard
//! renders.

use anyhow::{anyhow, Context as AnyhowContext, Result};
use openvr::overlay::OverlayHandle;
use openvr::pose::Matrix3x4;
use openvr::{ApplicationType, Context as OvrContext, TrackingUniverseOrigin};

/// Default world-locked pose: 0.6 m forward of the standing zero-pose,
/// 0.20 m below eye level, 0.30 m wide. Tuned for "obviously hovering
/// in front of you"; phase 4 makes this user-controlled.
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
    ctx: OvrContext,
    overlay_handle: OverlayHandle,
}

impl VrSession {
    /// Replace the overlay's texture with `pixels` (RGBA8, row-major).
    /// Called once per frame from the main loop's render tick. Cheap
    /// in the steady state — SteamVR keeps a GPU copy and only blits
    /// when SetOverlayRaw is invoked.
    pub fn submit_frame(&self, pixels: &[u8], width: u32, height: u32) -> Result<()> {
        let mut overlay = self
            .ctx
            .overlay()
            .map_err(|e| anyhow!("ctx.overlay failed: {e:?}"))?;
        overlay
            .set_raw_data(
                self.overlay_handle,
                pixels,
                width as usize,
                height as usize,
                4,
            )
            .map_err(|e| anyhow!("set_raw_data failed: {e:?}"))?;
        Ok(())
    }
}

/// Initialise OpenVR as an overlay-class app, create one overlay
/// placed world-locked, and show it. Caller drives subsequent
/// `submit_frame()` calls to push new pixels.
///
/// Errors out if SteamVR isn't running or the OpenVR runtime can't
/// hand us an IVROverlay. The caller treats this as non-fatal: the
/// desktop window keeps working.
pub fn init_session() -> Result<VrSession> {
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
        .create_overlay("dcsboards.kneeboard\0", "DCS Kneeboard\0")
        .map_err(|e| anyhow!("create_overlay failed: {e:?}"))?;
    eprintln!("[vr] overlay created (handle = {:?})", overlay_handle.0);

    overlay
        .set_width(overlay_handle, DEFAULT_WIDTH_M)
        .map_err(|e| anyhow!("set_width failed: {e:?}"))?;

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
    eprintln!("[vr] overlay shown — feed pixels via VrSession::submit_frame");

    Ok(VrSession {
        ctx,
        overlay_handle,
    })
}
