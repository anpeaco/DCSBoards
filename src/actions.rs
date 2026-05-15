//! User-invokable actions and their string identifiers.
//!
//! Every interactive thing the user can trigger — via keyboard, HOTAS, voice,
//! or an on-screen button — funnels through a single `Action` so the binding
//! system and the voice router both target the same set.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    /// Advance to the next navigable item (cross-page).
    Next,
    /// Step to the previous navigable item (cross-page).
    Previous,
    /// Toggle play/pause on the current item's TTS readout.
    TogglePlay,
    /// Speak the current item without toggling (idempotent re-read).
    ReadCurrent,
    /// Speak the nearest preceding heading so the user can hear which
    /// section they're in. Voice-friendly: "what section?".
    ReadSection,
    /// Jump cursor back to the start of the current section and begin
    /// reading. Voice-friendly: "restart section" / "from the top".
    RestartSection,
    /// Jump cursor to the next heading.
    NextHeading,
    /// Jump cursor to the previous heading.
    PrevHeading,
    /// Move to the first item of the next page.
    PageNext,
    /// Move to the first item of the previous page.
    PagePrev,
    /// Cycle the left tab strip backwards.
    CycleTabPrev,
    /// Cycle the left tab strip forwards.
    CycleTabNext,
    /// Open / close the settings panel.
    OpenSettings,
    /// Open / close the voice-commands help panel.
    OpenVoiceCommands,
    /// Reload `pronunciation.toml` at runtime (dev convenience).
    ReloadPronunciation,
    /// Begin voice capture (press) / submit utterance (release). M4 STT.
    PushToTalk,
    /// Toggle a "hot mic" — press once to start capturing, press again to
    /// stop and submit. Unlike PushToTalk this does not need to be held.
    HotMicToggle,
    /// Toggle the "read supporting notes" setting (voice: "more info on/off").
    ToggleReadNotes,
    /// Stop speaking, dismiss panels, cancel capture.
    Cancel,
    // Stubs for later milestones — kept here so bindings can reference them
    // before the feature lands.
    /// M7: toggle WS_EX_TRANSPARENT click-through.
    ToggleClickThrough,
    /// M7: hide/show the overlay window.
    ToggleVisibility,
    // VR overlay positioning (#30 phase 4). Only meaningful in VR
    // mode; no-op on desktop. Voice-friendly phrasings live in the
    // voice_router phrase table.
    /// Snap the VR overlay to ~0.6 m in front of the HMD's current pose.
    VrPlaceHere,
    /// Move the VR overlay 10 cm closer to / further from the user
    /// (along world Z, i.e. world-forward/back from the SteamVR origin).
    VrMoveCloser,
    VrMoveFurther,
    /// Move the VR overlay 10 cm in world ±X.
    VrMoveLeft,
    VrMoveRight,
    /// Move the VR overlay 10 cm in world ±Y.
    VrMoveUp,
    VrMoveDown,
    /// Grow / shrink overlay width by 5 cm (clamped 15 cm — 1 m).
    VrSizeUp,
    VrSizeDown,
    /// Re-apply the default forward+down pose at default size.
    VrResetPose,
}

impl Action {
    /// Human-readable label for the settings UI.
    pub fn label(self) -> &'static str {
        match self {
            Action::Next => "Next item",
            Action::Previous => "Previous item",
            Action::TogglePlay => "Play / pause",
            Action::ReadCurrent => "Read current item",
            Action::ReadSection => "Read current section",
            Action::RestartSection => "Restart current section",
            Action::NextHeading => "Next heading",
            Action::PrevHeading => "Previous heading",
            Action::PageNext => "Next page",
            Action::PagePrev => "Previous page",
            Action::CycleTabPrev => "Previous tab",
            Action::CycleTabNext => "Next tab",
            Action::OpenSettings => "Open settings",
            Action::OpenVoiceCommands => "Open voice commands",
            Action::ReloadPronunciation => "Reload pronunciation",
            Action::PushToTalk => "Push-to-talk",
            Action::HotMicToggle => "Hot mic (toggle)",
            Action::ToggleReadNotes => "Toggle read-notes",
            Action::Cancel => "Cancel",
            Action::ToggleClickThrough => "Toggle click-through",
            Action::ToggleVisibility => "Toggle visibility",
            Action::VrPlaceHere => "VR: place kneeboard here",
            Action::VrMoveCloser => "VR: move closer",
            Action::VrMoveFurther => "VR: move further",
            Action::VrMoveLeft => "VR: move left",
            Action::VrMoveRight => "VR: move right",
            Action::VrMoveUp => "VR: move up",
            Action::VrMoveDown => "VR: move down",
            Action::VrSizeUp => "VR: bigger",
            Action::VrSizeDown => "VR: smaller",
            Action::VrResetPose => "VR: reset position",
        }
    }

    /// All actions in the order they should appear in the bindings UI.
    pub fn all() -> &'static [Action] {
        &[
            Action::Next,
            Action::Previous,
            Action::TogglePlay,
            Action::ReadCurrent,
            Action::ReadSection,
            Action::RestartSection,
            Action::NextHeading,
            Action::PrevHeading,
            Action::PageNext,
            Action::PagePrev,
            Action::CycleTabPrev,
            Action::CycleTabNext,
            Action::PushToTalk,
            Action::HotMicToggle,
            Action::ToggleReadNotes,
            Action::OpenSettings,
            Action::OpenVoiceCommands,
            Action::ReloadPronunciation,
            Action::Cancel,
            Action::ToggleClickThrough,
            Action::ToggleVisibility,
            Action::VrPlaceHere,
            Action::VrMoveCloser,
            Action::VrMoveFurther,
            Action::VrMoveLeft,
            Action::VrMoveRight,
            Action::VrMoveUp,
            Action::VrMoveDown,
            Action::VrSizeUp,
            Action::VrSizeDown,
            Action::VrResetPose,
        ]
    }
}
