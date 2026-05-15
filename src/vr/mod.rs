//! VR overlay support (issue #30). Phase 1 (this PR): cargo feature
//! gating + a yellow-and-black test pattern submitted as a world-locked
//! SteamVR overlay so we can prove SteamVR sees us at all. Subsequent
//! phases swap in the real Slint render, add HMD-presence detection, and
//! wire up the position UX.
//!
//! All public API is `#[cfg(feature = "vr")]` — desktop-only builds (the
//! cargo default + the `--no-default-features` CI guard) skip the
//! openvr dep entirely.

#![cfg(feature = "vr")]

pub mod render;
pub mod session;

pub use render::render_kneeboard_frame;
pub use session::{init_session, VrSession};
