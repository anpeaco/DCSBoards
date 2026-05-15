//! VR session lifecycle: init OpenVR, create one IVROverlay, place it
//! world-locked, push pixel buffers + pose updates to it.
//!
//! Phase 4 adds mutating methods (place_here, nudge_translation,
//! nudge_size, reset, apply_saved). Pose state is mirrored locally so
//! we can compute deltas without round-tripping to OpenVR.

use anyhow::{anyhow, Context as AnyhowContext, Result};
use openvr::overlay::OverlayHandle;
use openvr::pose::Matrix3x4;
use openvr::{ApplicationType, Context as OvrContext, TrackingUniverseOrigin};

/// Default world-locked pose: 0.6 m forward of the seated zero-pose,
/// 0.20 m below eye level, 0.30 m wide. Seated origin is the right
/// frame for DCS pilots — the SteamVR seated-zero calibration puts
/// Y=0 at eye height + the user's actual seated position at X=Z=0,
/// so the default forward+down lands as a tablet you can read down at.
/// Standing origin would put Y=0 at the floor, dropping the overlay
/// 20 cm below the chaperone — definitely not what we want.
const DEFAULT_FORWARD_M: f32 = 0.6;
const DEFAULT_DROP_M: f32 = -0.2;
const DEFAULT_WIDTH_M: f32 = 0.30;

/// Identity rotation + (0, DROP, -FORWARD) in seated-origin coords.
fn default_transform() -> Matrix3x4 {
    Matrix3x4([
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, DEFAULT_DROP_M],
        [0.0, 0.0, 1.0, -DEFAULT_FORWARD_M],
    ])
}

pub struct VrSession {
    ctx: OvrContext,
    overlay_handle: OverlayHandle,
    /// Mirrored locally so nudge() can compute deltas off the current
    /// pose without round-tripping to GetOverlayTransformAbsolute on
    /// every action.
    pose: Matrix3x4,
    width_m: f32,
}

impl VrSession {
    /// Replace the overlay's texture with `pixels` (RGBA8, row-major).
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

    /// Snap the overlay to ~0.6 m in front of the HMD's current
    /// position, 0.2 m below eye level, oriented to face the user.
    /// Reads the live HMD pose from the OpenVR system interface.
    pub fn place_here(&mut self) -> Result<()> {
        let system = self
            .ctx
            .system()
            .map_err(|e| anyhow!("ctx.system failed: {e:?}"))?;
        let poses = system.device_to_absolute_tracking_pose(TrackingUniverseOrigin::Seated, 0.0);
        let hmd = poses.first().ok_or_else(|| anyhow!("no HMD pose"))?;
        let hmd_m = *hmd.device_to_absolute_tracking();

        // HMD-local +Z points back (toward user's eyes); -Z is forward.
        // Column index 2 of the 3x3 rotation block IS HMD-local +Z in
        // world coords, so forward-in-world = -col2.
        let forward = [-hmd_m[0][2], -hmd_m[1][2], -hmd_m[2][2]];
        let up = [hmd_m[0][1], hmd_m[1][1], hmd_m[2][1]];
        let pos = [hmd_m[0][3], hmd_m[1][3], hmd_m[2][3]];

        let tx = pos[0] + DEFAULT_FORWARD_M * forward[0] + DEFAULT_DROP_M.abs() * -up[0];
        let ty = pos[1] + DEFAULT_FORWARD_M * forward[1] + DEFAULT_DROP_M.abs() * -up[1];
        let tz = pos[2] + DEFAULT_FORWARD_M * forward[2] + DEFAULT_DROP_M.abs() * -up[2];

        // Use the HMD's rotation so the overlay faces back at the user.
        let new_pose = Matrix3x4([
            [hmd_m[0][0], hmd_m[0][1], hmd_m[0][2], tx],
            [hmd_m[1][0], hmd_m[1][1], hmd_m[1][2], ty],
            [hmd_m[2][0], hmd_m[2][1], hmd_m[2][2], tz],
        ]);
        self.set_pose(new_pose)
    }

    /// Translate the current pose by (dx, dy, dz) meters in world
    /// space. Rotation is preserved.
    pub fn nudge_translation(&mut self, dx: f32, dy: f32, dz: f32) -> Result<()> {
        // Matrix3x4 doesn't impl Copy; rebuild from the inner array.
        let mut m = self.pose.0;
        m[0][3] += dx;
        m[1][3] += dy;
        m[2][3] += dz;
        self.set_pose(Matrix3x4(m))
    }

    /// Adjust overlay width by `delta_m`, clamped to a sane range
    /// (15 cm — 1 m). Below 15 cm the page is unreadable; above 1 m
    /// it dominates the cockpit view.
    pub fn nudge_size(&mut self, delta_m: f32) -> Result<()> {
        let new_width = (self.width_m + delta_m).clamp(0.15, 1.0);
        self.set_width(new_width)
    }

    /// Re-apply the default forward+down pose at default size.
    pub fn reset(&mut self) -> Result<()> {
        self.set_pose(default_transform())?;
        self.set_width(DEFAULT_WIDTH_M)
    }

    /// Restore a previously-saved pose + size (from settings on
    /// aircraft switch). Both calls are pushed to OpenVR.
    pub fn apply_saved(&mut self, transform: [[f32; 4]; 3], size_m: f32) -> Result<()> {
        self.set_pose(Matrix3x4(transform))?;
        self.set_width(size_m.clamp(0.15, 1.0))
    }

    /// Snapshot current pose + size for persistence.
    pub fn snapshot(&self) -> ([[f32; 4]; 3], f32) {
        let m = &self.pose.0;
        (
            [
                [m[0][0], m[0][1], m[0][2], m[0][3]],
                [m[1][0], m[1][1], m[1][2], m[1][3]],
                [m[2][0], m[2][1], m[2][2], m[2][3]],
            ],
            self.width_m,
        )
    }

    fn set_pose(&mut self, pose: Matrix3x4) -> Result<()> {
        let mut overlay = self
            .ctx
            .overlay()
            .map_err(|e| anyhow!("ctx.overlay failed: {e:?}"))?;
        overlay
            .set_transform_absolute(self.overlay_handle, TrackingUniverseOrigin::Seated, &pose)
            .map_err(|e| anyhow!("set_transform_absolute failed: {e:?}"))?;
        self.pose = pose;
        Ok(())
    }

    fn set_width(&mut self, width_m: f32) -> Result<()> {
        let mut overlay = self
            .ctx
            .overlay()
            .map_err(|e| anyhow!("ctx.overlay failed: {e:?}"))?;
        overlay
            .set_width(self.overlay_handle, width_m)
            .map_err(|e| anyhow!("set_width failed: {e:?}"))?;
        self.width_m = width_m;
        Ok(())
    }
}

/// Initialise OpenVR as an overlay-class app, create one overlay
/// placed at the default world-locked pose, and show it. Caller
/// drives subsequent pose / texture updates.
pub fn init_session() -> Result<VrSession> {
    eprintln!("[vr] initialising OpenVR (overlay class)…");
    let ctx = unsafe { openvr::init(ApplicationType::Overlay) }
        .map_err(|e| anyhow!("OpenVR init failed: {e:?}"))
        .context("Is SteamVR running?")?;

    let mut overlay = ctx
        .overlay()
        .map_err(|e| anyhow!("OpenVR did not expose IVROverlay: {e:?}"))?;

    let overlay_handle = overlay
        .create_overlay("dcsboards.kneeboard\0", "DCS Kneeboard\0")
        .map_err(|e| anyhow!("create_overlay failed: {e:?}"))?;
    eprintln!("[vr] overlay created (handle = {:?})", overlay_handle.0);

    overlay
        .set_width(overlay_handle, DEFAULT_WIDTH_M)
        .map_err(|e| anyhow!("set_width failed: {e:?}"))?;

    let pose = default_transform();
    overlay
        .set_transform_absolute(overlay_handle, TrackingUniverseOrigin::Seated, &pose)
        .map_err(|e| anyhow!("set_transform_absolute failed: {e:?}"))?;

    overlay
        .set_visibility(overlay_handle, true)
        .map_err(|e| anyhow!("show overlay failed: {e:?}"))?;
    eprintln!("[vr] overlay shown — feed pixels via VrSession::submit_frame");

    Ok(VrSession {
        ctx,
        overlay_handle,
        pose,
        width_m: DEFAULT_WIDTH_M,
    })
}
