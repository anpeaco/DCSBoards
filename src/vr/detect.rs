//! HMD-presence + SteamVR-runtime detection. Cheap to call (no
//! VR_Init required) so we poll periodically to drive auto-switching
//! between desktop and VR modes (#30 phase 3).

/// Should the VR overlay be active right now?
///
/// `mode` is the user-set policy from settings:
///   - "vr"      → always yes (force).
///   - "desktop" → always no (force).
///   - anything else (typically "auto") → yes iff SteamVR runtime is
///     installed AND an HMD is currently powered on/connected.
///
/// `is_runtime_installed` checks for the SteamVR install on disk; it
/// doesn't need SteamVR to actually be running. `is_hmd_present`
/// returns true only when SteamVR is up AND a headset is detected, so
/// the combined check covers both "no SteamVR" and "SteamVR but
/// headset off" cases without any extra plumbing.
pub fn should_be_active(mode: &str) -> bool {
    match mode {
        "vr" => true,
        "desktop" => false,
        _ => openvr::is_runtime_installed() && openvr::is_hmd_present(),
    }
}
