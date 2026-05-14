//! Input bindings: trigger → action lookup.
//!
//! Triggers are stable, serialisable descriptors of a physical input event:
//! a keyboard combo, a gamepad button, or a raw HID button. Each trigger
//! resolves to at most one `Action`; an action may have multiple triggers
//! (a HOTAS button + a keyboard fallback, for example).
//!
//! M4 step 1 wires up the keyboard variant only. Gamepad / HID variants are
//! declared so settings.toml entries written by later milestones round-trip,
//! and so the capture-mode UI has somewhere to put them.

use crate::actions::Action;
use serde::{Deserialize, Serialize};
use std::fmt;

pub mod gamepad;

/// A physical input event, edge-resolved. Only `PushToTalk` cares about the
/// release edge today, but the framing keeps that door open without bolting
/// it on later.
#[derive(Debug, Clone)]
pub enum InputEvent {
    Press(Trigger),
    Release(Trigger),
}

bitflags::bitflags! {
    /// Modifier flags. Stored on `Trigger::Keyboard` as a bitset so combos
    /// like Ctrl+Alt+K survive a TOML round-trip and so the capture-mode UI
    /// can render them as text ("Ctrl+Alt+K").
    #[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
    #[serde(transparent)]
    pub struct Mods: u8 {
        const CTRL  = 0b0001;
        const SHIFT = 0b0010;
        const ALT   = 0b0100;
        const META  = 0b1000;
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Trigger {
    /// Keyboard combo. `key` is the canonical key name (single-letter keys
    /// stored lowercase; named keys use Slint's `Key.X` constants verbatim:
    /// "Space", "Backspace", "Escape", "F5", "PageUp", "PageDown").
    Keyboard {
        key: String,
        #[serde(default)]
        mods: Mods,
    },
    /// Gamepad / HOTAS button via `gilrs`. `guid` is the stable per-device
    /// id (UUID-derived) so bindings survive reconnects and pad-order shuffle.
    /// `code` is the gilrs `Code` Display form — varies by platform (Linux
    /// uses `BTN_*` / `KEY_*`, Windows uses XInput button names) but is stable
    /// for a given device across runs.
    Gamepad { guid: String, code: String },
    /// Raw HID button via `hidapi`. For devices `gilrs` can't see
    /// (some FreeJoy modes). Wired post-M4.
    Hid { vid: u16, pid: u16, usage: u32 },
}

impl Trigger {
    /// Human-readable form for the settings UI.
    pub fn display(&self) -> String {
        match self {
            Trigger::Keyboard { key, mods } => {
                let mut s = String::new();
                if mods.contains(Mods::CTRL) { s.push_str("Ctrl+"); }
                if mods.contains(Mods::ALT) { s.push_str("Alt+"); }
                if mods.contains(Mods::SHIFT) { s.push_str("Shift+"); }
                if mods.contains(Mods::META) { s.push_str("Win+"); }
                s.push_str(&display_key(key));
                s
            }
            Trigger::Gamepad { guid, code } => match gamepad::device_name(guid) {
                Some(name) => format!("{name} {code}"),
                None => {
                    let short: String = guid.chars().take(6).collect();
                    format!("Pad[{short}…] {code}")
                }
            },
            Trigger::Hid { vid, pid, usage } => {
                format!("HID {vid:04x}:{pid:04x}/{usage}")
            }
        }
    }
}

impl fmt::Display for Trigger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.display())
    }
}

fn display_key(k: &str) -> String {
    match k {
        " " | "Space" => "Space".into(),
        "Backspace" => "Backspace".into(),
        "Escape" => "Esc".into(),
        "PageUp" => "PageUp".into(),
        "PageDown" => "PageDown".into(),
        other if other.len() == 1 => other.to_ascii_uppercase(),
        other => other.to_string(),
    }
}

/// One binding row. Plural triggers per action is the common case so each
/// row carries one trigger; the table is just a flat `Vec<Binding>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Binding {
    pub action: Action,
    pub trigger: Trigger,
}

/// Full binding table, persisted under `[[bindings]]` in settings.toml.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Bindings(pub Vec<Binding>);

impl Bindings {
    /// Built-in defaults matching the legacy hardcoded FocusScope keymap.
    pub fn defaults() -> Self {
        use Action::*;
        let kb = |key: &str, action: Action| Binding {
            action,
            trigger: Trigger::Keyboard { key: key.into(), mods: Mods::empty() },
        };
        let kbm = |key: &str, mods: Mods, action: Action| Binding {
            action,
            trigger: Trigger::Keyboard { key: key.into(), mods },
        };
        Self(vec![
            kb("Space", Next),
            kb("Backspace", Previous),
            kb("r", TogglePlay),
            kb("h", NextHeading),
            kbm("h", Mods::SHIFT, PrevHeading),
            kb("PageDown", PageNext),
            kb("PageUp", PagePrev),
            kb("F5", ReloadPronunciation),
            kb("Escape", Cancel),
            // Reasonable global-hotkey defaults that don't collide with DCS:
            kbm("k", Mods::CTRL | Mods::ALT, ToggleClickThrough),
            kbm("v", Mods::CTRL | Mods::ALT, ToggleVisibility),
            // PushToTalk has no default — user binds to HOTAS via capture mode.
        ])
    }

    /// First action bound to a given trigger, if any.
    pub fn action_for(&self, trigger: &Trigger) -> Option<Action> {
        self.0.iter().find(|b| &b.trigger == trigger).map(|b| b.action)
    }

    /// All triggers currently mapped to `action`.
    pub fn triggers_for(&self, action: Action) -> Vec<&Trigger> {
        self.0.iter().filter(|b| b.action == action).map(|b| &b.trigger).collect()
    }

    /// Replace every trigger bound to `action` with `triggers`. Used by the
    /// capture-mode UI after the user binds a fresh key.
    pub fn set_triggers(&mut self, action: Action, triggers: Vec<Trigger>) {
        self.0.retain(|b| b.action != action);
        for t in triggers {
            self.0.push(Binding { action, trigger: t });
        }
    }

    /// Remove every binding that points at `trigger` (regardless of action).
    /// Used when the user binds a key that was already in use — last writer
    /// wins, matching the conflict resolution in SPEC §7.5.
    pub fn unbind_trigger(&mut self, trigger: &Trigger) {
        self.0.retain(|b| &b.trigger != trigger);
    }
}

/// Convert a Slint key event payload + modifier flags into a `Trigger`.
/// Returns `None` for unrecognized payloads (modifier-only events, etc).
pub fn key_event_to_trigger(text: &str, mods: Mods) -> Option<Trigger> {
    // Slint sends the literal character for typeable keys (so SHIFT+h arrives
    // as "H"). Normalise to lowercase so capital/lowercase letters land on
    // the same key with SHIFT distinguishing them.
    let key = if text.len() == 1 && text.chars().next().unwrap().is_ascii_alphabetic() {
        text.to_ascii_lowercase()
    } else {
        text.to_string()
    };
    if key.is_empty() {
        return None;
    }
    Some(Trigger::Keyboard { key, mods })
}
