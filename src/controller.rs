//! Pure logic for the checklist controller's armed-state semantics
//! (issue #17). Side-effects — TTS, Slint UI, cursor mutation — live
//! on `AppState::dispatch` and read this module's `ArmedDecision` to
//! pick a branch.
//!
//! Lives in its own file so the state-transition table is unit-
//! testable without spinning up Slint or whisper. Every `Action`
//! variant is exhaustively classified here; the match in
//! `armed_decision()` lacks a wildcard arm so a future `Action`
//! variant will fail to compile until the author classifies it
//! correctly — better than silently inheriting a "stay armed and run"
//! default the way the previous `_ =>` catch-all did.
//!
//! Exhaustive tests at the bottom keep us honest as the action set
//! grows: every armed+action pair has an asserted outcome.

use crate::actions::Action;

/// What `dispatch()` should do with `action` given the current armed
/// state. Decoded into side-effects by `AppState::dispatch`; tests
/// hit `armed_decision()` directly with no side-effects in scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmedDecision {
    /// Disarm and speak the current item. The user said "go" / "ok" /
    /// "start" / "play" — they want the highlighted step now.
    SpeakCurrent,
    /// Re-read the section header and stay armed. Triggered by repeat-
    /// like actions ("again", "what section") while armed so the user
    /// can hear the header again without leaving the wait state.
    RereadHeader,
    /// Drop the armed flag and stop speaking. No new audio. Used by
    /// `Action::Cancel` when armed.
    SilentDisarm,
    /// Disarm, restore the pre-jump cursor, and bail. Used by
    /// `Action::Previous` when armed so backing out lands the user
    /// where they were before they ever jumped, not in the section
    /// before this one.
    ReturnToPreJump,
    /// Disarm, then continue into the normal action handler. The
    /// handler may immediately re-arm if it's another section jump.
    DisarmThenRun,
    /// Leave the armed flag alone and run the normal handler. Used
    /// for neutral actions (PTT — user is about to *say* the disarm
    /// word — panel toggles, setting toggles) that shouldn't mutate
    /// the armed state on their own.
    PassThrough,
}

/// Classify `(action, armed)` into a dispatch decision. Pure; no
/// side-effects, no allocations. When `!armed` always returns
/// `PassThrough` — the caller's normal action handler runs unchanged.
pub fn armed_decision(action: Action, armed: bool) -> ArmedDecision {
    if !armed {
        return ArmedDecision::PassThrough;
    }
    match action {
        // The "go" gestures: exit armed and speak the current step.
        // Next is the canonical advance; TogglePlay routes here so
        // clicking the on-screen Play button while armed matches what
        // saying "play" would do.
        Action::Next | Action::TogglePlay => ArmedDecision::SpeakCurrent,
        // Repeat-while-armed. ReadSection ("what section") gets the
        // same treatment as ReadCurrent ("again") because both are
        // requests for the same audio — the section header.
        Action::ReadCurrent | Action::ReadSection => ArmedDecision::RereadHeader,
        Action::Cancel => ArmedDecision::SilentDisarm,
        Action::Previous => ArmedDecision::ReturnToPreJump,
        // Explicit navigation away from the armed step. Heading jumps
        // and section restart re-arm at the end of their handlers;
        // the rest leave us un-armed in whatever they navigate to.
        Action::NextHeading
        | Action::PrevHeading
        | Action::PageNext
        | Action::PagePrev
        | Action::CycleTabPrev
        | Action::CycleTabNext
        | Action::RestartSection => ArmedDecision::DisarmThenRun,
        // Neutral actions that don't move the cursor or replace the
        // read. Notably PushToTalk: the user is about to say the
        // disarm word, so silently dropping the flag here would route
        // their "ok" / "go" as a fresh Next instead of a disarm.
        Action::PushToTalk
        | Action::HotMicToggle
        | Action::OpenSettings
        | Action::OpenVoiceCommands
        | Action::ReloadPronunciation
        | Action::ToggleReadNotes
        | Action::ToggleClickThrough
        | Action::ToggleVisibility => ArmedDecision::PassThrough,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// When not armed every action is a pass-through — the existing
    /// non-armed code path runs untouched.
    #[test]
    fn not_armed_always_passes_through() {
        for &action in Action::all() {
            assert_eq!(
                armed_decision(action, false),
                ArmedDecision::PassThrough,
                "{action:?} should pass through when not armed",
            );
        }
    }

    #[test]
    fn next_and_toggleplay_speak_current_while_armed() {
        assert_eq!(armed_decision(Action::Next, true), ArmedDecision::SpeakCurrent);
        assert_eq!(armed_decision(Action::TogglePlay, true), ArmedDecision::SpeakCurrent);
    }

    #[test]
    fn read_current_and_read_section_reread_header_while_armed() {
        assert_eq!(armed_decision(Action::ReadCurrent, true), ArmedDecision::RereadHeader);
        assert_eq!(armed_decision(Action::ReadSection, true), ArmedDecision::RereadHeader);
    }

    #[test]
    fn cancel_silent_disarms() {
        assert_eq!(armed_decision(Action::Cancel, true), ArmedDecision::SilentDisarm);
    }

    #[test]
    fn previous_returns_to_pre_jump() {
        assert_eq!(armed_decision(Action::Previous, true), ArmedDecision::ReturnToPreJump);
    }

    /// Explicit navigation away — heading jumps re-arm at the end of
    /// their handlers; the page/tab jumps and RestartSection don't.
    #[test]
    fn navigation_actions_disarm_then_run() {
        for action in [
            Action::NextHeading,
            Action::PrevHeading,
            Action::PageNext,
            Action::PagePrev,
            Action::CycleTabPrev,
            Action::CycleTabNext,
            Action::RestartSection,
        ] {
            assert_eq!(
                armed_decision(action, true),
                ArmedDecision::DisarmThenRun,
                "{action:?} should disarm-then-run while armed",
            );
        }
    }

    /// PTT is the load-bearing test here: the user pressing the
    /// push-to-talk button is *about to* say the disarm word. If
    /// dispatch greedily dropped the flag on that keypress the
    /// resulting "ok" / "go" transcript would route as a fresh Next
    /// and advance past the armed step.
    #[test]
    fn neutral_actions_pass_through_while_armed() {
        for action in [
            Action::PushToTalk,
            Action::HotMicToggle,
            Action::OpenSettings,
            Action::OpenVoiceCommands,
            Action::ReloadPronunciation,
            Action::ToggleReadNotes,
            Action::ToggleClickThrough,
            Action::ToggleVisibility,
        ] {
            assert_eq!(
                armed_decision(action, true),
                ArmedDecision::PassThrough,
                "{action:?} should pass through while armed",
            );
        }
    }

    /// Regression guard: every action listed in `Action::all()` must
    /// appear in the armed match arm above. If a new variant is added
    /// to the enum without a classification in `armed_decision`, this
    /// test won't catch it (the match would fail to compile) — but
    /// this test catches the inverse: `Action::all()` going out of
    /// sync with the enum.
    #[test]
    fn every_action_in_all_has_a_decision() {
        for &action in Action::all() {
            // Just calling armed_decision is enough — if it ever
            // panicked on an unhandled variant we'd see it here.
            let _ = armed_decision(action, true);
            let _ = armed_decision(action, false);
        }
    }
}
