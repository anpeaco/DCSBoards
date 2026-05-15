// During development we keep a console attached so stderr (tracing logs, panics)
// is visible. Re-enable the windows subsystem at M9 packaging:
// #![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod actions;
mod audio;
mod config;
mod controller;
mod input;
#[cfg(windows)]
mod overlay;
mod settings;
// `stt` compiles unconditionally so `SttCommand` and the `SttEngine`
// trait can be named from struct fields + channels even without the
// `whisper-stt` feature (#27). Only the whisper-rs-backed impl
// (`WhisperStt`, `find_default_model`) is feature-gated.
mod stt;
mod query_aliases;
mod tabs;
mod tts;
mod voice_router;
mod watcher;

use actions::Action;
use anyhow::Result;
use audio::AudioCapture;
use config::{config_path, AppConfig};
use input::{key_event_to_trigger, InputEvent, Mods};
use settings::{settings_path, Settings};
use slint::{ComponentHandle, SharedString, VecModel};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;
use query_aliases::QueryAliases;
use tabs::{first_navigable, Cursor, Item, LoadedPage, TabRegistry};
use tts::{spoken_for, PronunciationConfig, TtsEngine};

// Win32 overlay flags (WS_EX_LAYERED for opacity, WS_EX_TRANSPARENT for click-through)
// will be added in M7 polish via raw HWND access through the `windows` crate.

/// Reject offscreen "hidden" positions so ToggleVisibility doesn't persist
/// (-30000, -30000) into settings.toml and lose the user's real window spot.
fn safe_position_filter(pos: slint::PhysicalPosition) -> Option<slint::PhysicalPosition> {
    if pos.x < -20000 || pos.y < -20000 {
        None
    } else {
        Some(pos)
    }
}

fn is_step(item: &Item) -> bool {
    item.kind == "step"
}

fn is_heading(item: &Item) -> bool {
    matches!(
        item.kind.as_str(),
        "section-header" | "branch-header" | "notes-heading"
    )
}

fn is_note(item: &Item) -> bool {
    matches!(
        item.kind.as_str(),
        "note-info" | "note-check" | "note-optional" | "note-caution" | "note-warning" | "note-radio"
    )
}

/// Lucide path-d for a small set of named icons used in tab cells. Falls back
/// to a horizontal dash for unknown names.
fn lucide_path(name: &str) -> &'static str {
    match name {
        "clipboard-list" => "M 16 4 h 2 a 2 2 0 0 1 2 2 v 14 a 2 2 0 0 1 -2 2 H 6 a 2 2 0 0 1 -2 -2 V 6 a 2 2 0 0 1 2 -2 h 2 M 9 2 h 6 a 1 1 0 0 1 1 1 v 2 a 1 1 0 0 1 -1 1 H 9 a 1 1 0 0 1 -1 -1 V 3 a 1 1 0 0 1 1 -1 z M 12 11 h 4 M 12 16 h 4 M 8 11 L 8 11.01 M 8 16 L 8 16.01",
        "plane" => "M 17.8 19.2 L 16 11 l 3.5 -3.5 C 21 6 21.5 4 21 3 c -1 -.5 -3 0 -4.5 1.5 L 13 8 L 4.8 6.2 c -.5 -.1 -.9 .1 -1.1 .5 l -.3 .5 c -.2 .5 -.1 1 .3 1.3 L 9 12 l -2 3 L 4 15 l -1 1 3 2 2 3 1 -1 v -3 l 3 -2 3.5 5.3 c .3 .4 .8 .5 1.3 .3 l .5 -.2 c .4 -.3 .6 -.7 .5 -1.2 z",
        "map" => "M 5 15.5 V 4.618 a 1 1 0 0 1 1.447 -.894 l 4 2 V 19 l -4 -2 a 1 1 0 0 1 -1.447 -.894 z M 9 3.236 v 16 M 15 5.764 v 16 M 14 5.553 a 2 2 0 0 0 1 0 L 18.382 3.553 a 1 1 0 0 1 1.45 .894 V 16.382 a 2 2 0 0 1 -1.106 1.79 L 14 20.447",
        "image" => "M 4 4 h 16 v 16 H 4 z M 9 11 a 2 2 0 1 1 -4 0 a 2 2 0 1 1 4 0 z M 20 17 l -5 -5 L 4 21",
        "list-checks" => "M 11 5 h 10 M 11 12 h 10 M 11 19 h 10 M 5 5 l 1 1 2 -2 M 5 12 l 1 1 2 -2 M 5 19 l 1 1 2 -2",
        "file-text" => "M 14 2 H 6 a 2 2 0 0 0 -2 2 v 16 a 2 2 0 0 0 2 2 h 12 a 2 2 0 0 0 2 -2 V 8 z M 14 2 v 6 h 6 M 16 13 H 8 M 16 17 H 8 M 10 9 H 8",
        "book" => "M 4 19.5 v -15 A 2.5 2.5 0 0 1 6.5 2 H 19 v 18 H 6.5 A 2.5 2.5 0 0 0 4 22.5 z M 8 6 h 8 M 8 10 h 8",
        "file" => "M 14.5 2 H 6 a 2 2 0 0 0 -2 2 v 16 a 2 2 0 0 0 2 2 h 12 a 2 2 0 0 0 2 -2 V 7.5 z M 14 2 v 6 h 6",
        _ => "M 5 12 L 19 12",
    }
}

slint::slint! {
    import { Button, CheckBox, Slider } from "std-widgets.slint";

    export struct TabInfo {
        id: string,
        label: string,
        icon-d: string,
    }

    export struct AircraftInfo {
        id: string,
        label: string,
    }

    export struct BindingRow {
        // Index into `Action::all()`. Round-trips back to Rust on click.
        action-id: int,
        label: string,
        trigger-text: string,   // "Space", "Shift+H", "(unbound)"
        capturing: bool,
    }

    // One row in the voice-commands help panel. `phrases` is a comma-joined
    // list pre-rendered in Rust so Slint doesn't need to walk a [string].
    // When `is-header` is true the row renders as a section divider —
    // larger label, no background, used to group commands / queries /
    // aliases visually.
    export struct VoiceCommandRow {
        action-label: string,
        phrases: string,
        is-header: bool,
    }

    // Hover aggregator for the tab strip. Each tab cell's TouchArea increments
    // this counter on hover-enter and decrements on hover-leave, so the strip
    // visibility expression can OR `count > 0` with the left-trigger and stay
    // visible while the cursor sits on any cell.
    global HoverState {
        in-out property <int> tab-cell-hover: 0;
    }

    // Lucide-style icon. Path commands are the actual lucide SVG data
    // (24×24 viewbox). For stroked icons (chevrons, x, settings) set
    // `filled: false`; for solid icons (play, pause, dots) set `filled: true`.
    component LucideIcon inherits Path {
        in property <string> path-d;
        in property <brush> tint: #bbb;
        in property <bool> filled: false;

        viewbox-width: 24;
        viewbox-height: 24;
        commands: root.path-d;
        stroke: root.filled ? transparent : root.tint;
        fill: root.filled ? root.tint : transparent;
        stroke-width: 2px;
    }

    // Tiny dark-themed checkbox so we can pick our own text colour. The
    // std-widgets CheckBox uses the active theme's foreground which is dark
    // gray on our dark panel — illegible — and Slint 1.13 doesn't expose a
    // direct `text-color` override.
    component DarkCheckBox inherits Rectangle {
        in property <string> text;
        in-out property <bool> checked: false;
        callback toggled;

        height: 22px;
        background: transparent;

        HorizontalLayout {
            spacing: 10px;
            alignment: start;
            padding: 0;

            VerticalLayout {
                alignment: center;
                width: 18px;
                Rectangle {
                    width: 18px;
                    height: 18px;
                    background: root.checked ? #ffcc33 : #1a1a1e;
                    border-color: root.checked ? #e6b820 : #888;
                    border-width: 1.5px;
                    border-radius: 3px;
                    if root.checked: Path {
                        x: 2px; y: 2px;
                        width: 14px;
                        height: 14px;
                        viewbox-width: 24;
                        viewbox-height: 24;
                        commands: "M 20 6 L 9 17 L 4 12";
                        stroke: #1a1a1e;
                        stroke-width: 3px;
                        fill: transparent;
                    }
                }
            }

            Text {
                text: root.text;
                color: #f0f0f0;
                font-size: 13px;
                vertical-alignment: center;
            }
        }

        TouchArea {
            clicked => {
                root.checked = !root.checked;
                root.toggled();
            }
        }
    }

    // Hover-highlighted icon cell. Exposes `hovered` so the parent bar can OR
    // it into its own visibility expression — otherwise the cell's TouchArea
    // steals hover from any bar-level trigger and the bar fades while you're
    // mid-click.
    component IconCell inherits Rectangle {
        in property <string> icon-d;
        in property <bool> icon-filled: false;
        in property <length> cell-w: 22px;
        in property <length> cell-h: 22px;
        in property <length> icon-pad: 4px;
        in property <brush> hover-bg: #2a2a2a;
        in property <brush> icon-tint: #bbb;
        // Defaults to the base tint so callers that don't opt in look unchanged.
        in property <brush> hover-tint: root.icon-tint;
        out property <bool> hovered: touch.has-hover;
        callback clicked;

        width: root.cell-w;
        height: root.cell-h;
        background: touch.has-hover ? root.hover-bg : transparent;

        LucideIcon {
            x: root.icon-pad;
            y: root.icon-pad;
            width: parent.width - 2 * root.icon-pad;
            height: parent.height - 2 * root.icon-pad;
            path-d: root.icon-d;
            tint: touch.has-hover ? root.hover-tint : root.icon-tint;
            filled: root.icon-filled;
        }

        touch := TouchArea {
            clicked => { root.clicked(); }
        }
    }

    export component MainWindow inherits Window {
        in property <image> page-image;
        in property <length> hl-x;
        in property <length> hl-y;
        in property <length> hl-w;
        in property <length> hl-h;
        in property <string> page-title;
        in property <string> item-group;
        in property <string> item-text;
        in property <string> item-counter;

        // Settings bound from Rust (in-out so the panel's widgets can mutate).
        in-out property <bool> settings-open: false;
        in-out property <bool> voice-commands-open: false;
        in property <[VoiceCommandRow]> voice-commands;
        in-out property <bool> auto-read: false;
        in-out property <bool> auto-advance: false;
        in-out property <float> advance-delay: 1.5;
        in-out property <bool> read-notes: false;
        in-out property <bool> hot-reload: false;
        in-out property <bool> mute-mic-during-speech: true;
        in-out property <bool> click-through: false;
        // Window opacity 0.3..=1.0. Applied via Win32 SetLayeredWindowAttributes
        // on the Rust side; this property is just the slider's bound value.
        in-out property <float> window-opacity: 1.0;
        in-out property <bool> is-playing: false;

        // Tabs + aircraft state pushed from Rust.
        in property <[TabInfo]> tabs;
        in property <int> active-tab: 0;
        in property <[AircraftInfo]> aircraft-list;
        in-out property <string> current-aircraft;
        in-out property <bool> strip-pinned: false;
        callback tab-clicked(int);
        callback tab-cycle-prev-clicked();
        callback tab-cycle-next-clicked();
        callback aircraft-clicked(string);

        // Bindings table — pushed from Rust whenever it changes.
        in property <[BindingRow]> bindings;
        callback binding-edit-clicked(int);   // start capture for action-id
        callback binding-clear-clicked(int);  // unbind action-id
        // Test/probe mode: while on, presses flash the matched row instead
        // of firing. `binding-flash-id` is the most-recent action; the
        // background animation auto-fades it back to neutral.
        in-out property <bool> binding-test-mode: false;
        in-out property <int> binding-flash-id: -1;
        callback binding-test-mode-toggled(bool);

        // Audio device list + current selection. mic-hot drives the small
        // top-right indicator the user sees while PTT is held.
        in property <[string]> audio-inputs;
        in property <string> audio-input-selected;
        in property <bool> mic-hot: false;
        in-out property <bool> mic-pulse: false;
        // Issue #17B: armed-after-section-jump state. The cursor is sitting
        // on the first step waiting for start/go/ok before reading. Drives
        // both a pill ("Armed — say go") and a pulsing highlight so the
        // user can tell at a glance that the controller is parked.
        in property <bool> armed: false;
        in-out property <bool> armed-pulse: false;
        callback audio-input-clicked(string);

        // TTS engine + voice selection. The voice list is the basenames of
        // .onnx files found in models/piper/voices/; clicking sends the full
        // path back to Rust (we pre-compute the mapping so Slint doesn't
        // need PathBuf).
        in property <string> tts-engine-selected;
        in property <[string]> piper-voices;
        in property <string> piper-voice-selected;
        in-out property <float> tts-rate: 1.0;
        in-out property <float> tts-volume: 1.0;
        callback tts-engine-clicked(string);
        callback piper-voice-clicked(string);
        callback tts-test-clicked();

        // Generic message pill (issue #17D). One Rectangle, fed by a
        // single content struct on the Rust side; sources compete by
        // priority (sticky armed-state messages beat transient
        // transcripts beat status toasts). Rust pushes whichever wins
        // into these properties; Slint just renders.
        //
        // - `pill-pulse` and `pill-icon-d` are mutually exclusive: the
        //   pulse dot occupies the icon slot when on.
        // - `pill-border-color` defaults to transparent so non-armed
        //   messages stay borderless; the armed source supplies amber.
        in property <string> pill-text;
        in property <bool> pill-visible: false;
        in property <string> pill-icon-d;
        in property <brush> pill-icon-tint: #ffcc33;
        in property <brush> pill-border-color: transparent;
        in property <bool> pill-pulse: false;
        // Transient-message lifetime. Kept the on-disk name so existing
        // settings.toml files round-trip unchanged even though the pill
        // is no longer transcript-specific.
        in-out property <float> transcript-pill-seconds: 5.0;

        callback next-clicked();
        callback prev-clicked();
        callback read-clicked();
        callback page-next-clicked();
        callback page-prev-clicked();
        callback next-heading-clicked();
        callback prev-heading-clicked();
        callback close-clicked();
        callback drag-by(length, length);
        callback settings-changed();
        callback settings-opened();   // fired when the panel transitions to open
        callback panels-changed();    // fired when settings/voice-commands open or close
        callback reload-pronunciation();
        // Keyboard entry-points. Rust resolves the trigger via the Bindings
        // table and returns true when it consumed the event so the FocusScope
        // can accept/reject. Separate press/release so PushToTalk has edges.
        callback handle-key(string, bool, bool, bool, bool) -> bool;
        callback handle-key-up(string, bool, bool, bool, bool) -> bool;

        // Translate Slint's KeyEvent payload into a stable name. Letters and
        // digits arrive as themselves; named keys come through as Private-Use-
        // Area chars (Key.Backspace = "\u{0008}", etc.) which round-trip
        // poorly through TOML — emit a readable name instead.
        pure function canon-key(t: string) -> string {
            if (t == " ") { return "Space"; }
            if (t == Key.Backspace) { return "Backspace"; }
            if (t == Key.Escape) { return "Escape"; }
            if (t == Key.PageDown) { return "PageDown"; }
            if (t == Key.PageUp) { return "PageUp"; }
            if (t == Key.Tab) { return "Tab"; }
            if (t == Key.Return) { return "Return"; }
            if (t == Key.F1) { return "F1"; }
            if (t == Key.F2) { return "F2"; }
            if (t == Key.F3) { return "F3"; }
            if (t == Key.F4) { return "F4"; }
            if (t == Key.F5) { return "F5"; }
            if (t == Key.F6) { return "F6"; }
            if (t == Key.F7) { return "F7"; }
            if (t == Key.F8) { return "F8"; }
            if (t == Key.F9) { return "F9"; }
            if (t == Key.F10) { return "F10"; }
            if (t == Key.F11) { return "F11"; }
            if (t == Key.F12) { return "F12"; }
            if (t == Key.UpArrow) { return "Up"; }
            if (t == Key.DownArrow) { return "Down"; }
            if (t == Key.LeftArrow) { return "Left"; }
            if (t == Key.RightArrow) { return "Right"; }
            if (t == Key.Home) { return "Home"; }
            if (t == Key.End) { return "End"; }
            if (t == Key.Insert) { return "Insert"; }
            if (t == Key.Delete) { return "Delete"; }
            return t;
        }

        title: "DCS Kneeboard";
        width: 600px;
        height: 900px;
        background: #000;
        no-frame: true;
        always-on-top: true;
        forward-focus: focus;

        // Side-effects driven by the panel's open/close transitions.
        // Opening silences in-flight speech; closing clears probe/test mode
        // so the next session starts in normal dispatch.
        changed settings-open => {
            if (root.settings-open) {
                root.voice-commands-open = false;
                root.settings-opened();
            } else {
                root.binding-test-mode = false;
                root.binding-flash-id = -1;
            }
            root.panels-changed();
        }
        changed voice-commands-open => {
            if (root.voice-commands-open) {
                root.settings-open = false;
                root.settings-opened();
            }
            root.panels-changed();
        }

        focus := FocusScope {
            x: 0; y: 0; width: 0; height: 0;
            key-pressed(event) => {
                if (root.handle-key(
                    canon-key(event.text),
                    event.modifiers.control,
                    event.modifiers.shift,
                    event.modifiers.alt,
                    event.modifiers.meta,
                )) {
                    return accept;
                }
                return reject;
            }
            key-released(event) => {
                if (root.handle-key-up(
                    canon-key(event.text),
                    event.modifiers.control,
                    event.modifiers.shift,
                    event.modifiers.alt,
                    event.modifiers.meta,
                )) {
                    return accept;
                }
                return reject;
            }
        }

        // Page image fills the whole window. The 1358:2037 PNG aspect (≈0.667)
        // matches 600:900 exactly so image-fit:contain has no letterbox.
        Image {
            x: 0px; y: 0px;
            width: parent.width;
            height: parent.height;
            source: root.page-image;
            image-fit: contain;
        }

        // Current-item highlight. Coords are pre-scaled in Rust to display space.
        // While armed (issue #17B) the highlight pulses between two alpha
        // levels on a 750 ms cycle — visible from across the room without
        // being motion-distracting like a faster strobe would be.
        Rectangle {
            x: root.hl-x;
            y: root.hl-y;
            width: root.hl-w;
            height: root.hl-h;
            border-color: root.armed ? #ffaa33 : #ffcc33;
            border-width: 2px;
            background: root.armed
                ? (root.armed-pulse ? #ffaa3355 : #ffaa331a)
                : #ffcc3322;
            border-radius: 2px;
            animate background { duration: 750ms; easing: ease-in-out; }
            animate border-color { duration: 300ms; easing: ease-out; }

            Timer {
                interval: 750ms;
                running: root.armed;
                triggered() => { root.armed-pulse = !root.armed-pulse; }
            }
        }

        // Click-through indicator. Just a small 22×22 chip top-right (same
        // height as the title bar) so it stays out of the way. Title bar
        // can't appear while click-through is on (hovers don't reach us),
        // so there's no overlap concern.
        if root.click-through && !root.settings-open && !root.voice-commands-open: Rectangle {
            x: parent.width - 24px;
            y: 2px;
            width: 22px;
            height: 22px;
            background: #ffcc33;
            border-radius: 11px;

            LucideIcon {
                x: 4px; y: 4px;
                width: 14px;
                height: 14px;
                // lucide "mouse-pointer-click"
                path-d: "M 9 9 l 5 12 l 1.774 -5.226 L 21 14 L 9 9 z M 16.071 16.071 l 4.243 4.243 M 7.188 2.239 l .777 2.898 M 5.136 7.965 l -2.898 -.777 M 13.95 4.05 l -2.122 2.122 M 5.05 12.95 l -2.122 2.122";
                tint: #1a1a1e;
            }
        }

        // Last-transcript pill. Right edge aligns with the mic indicator so
        // the mic chip sits at the right "cap" of one continuous two-tone
        // pill (dark left half = transcript, yellow right half = mic).
        // Same 14px radius + 28px height as the mic so the rounding matches.
        // Generic message pill (issue #17D). Renders whichever
        // `PillMessage` the Rust side has resolved to the highest
        // priority — armed state, transient transcripts, status
        // toasts, future "Loading model…" / error banners — all share
        // this one Rectangle.
        pill := Rectangle {
            x: root.width - 8px - self.width;
            y: 32px;
            width: 320px;
            height: 28px;
            background: rgba(26, 26, 30, 0.94);
            border-color: root.pill-border-color;
            border-width: 1px;
            border-radius: 14px;
            opacity: root.pill-visible ? 1.0 : 0.0;
            animate opacity { duration: 400ms; easing: ease-in-out; }
            animate border-color { duration: 300ms; easing: ease-out; }

            // Optional pulse dot. Mutually exclusive with the icon —
            // sticky "alive" sources (armed state) use the pulse;
            // transient sources (transcripts) use a lucide icon.
            if root.pill-pulse: Rectangle {
                x: 12px;
                y: (parent.height - self.height) / 2;
                width: 8px;
                height: 8px;
                border-radius: 4px;
                background: root.pill-icon-tint;
                opacity: root.armed-pulse ? 1.0 : 0.4;
                animate opacity { duration: 750ms; easing: ease-in-out; }
            }

            if !root.pill-pulse && root.pill-icon-d != "": LucideIcon {
                x: 10px;
                y: 6px;
                width: 16px;
                height: 16px;
                path-d: root.pill-icon-d;
                tint: root.pill-icon-tint;
            }

            Text {
                // Indent past the pulse dot / icon, else fall back to
                // a 14 px left margin.
                x: root.pill-pulse
                    ? 28px
                    : (root.pill-icon-d != "" ? 32px : 14px);
                y: 0px;
                // Leave ~36px on the right so text doesn't slide under
                // the mic indicator.
                width: parent.width - self.x - 36px;
                height: parent.height;
                text: root.pill-text;
                color: #f0f0f0;
                font-size: 12px;
                vertical-alignment: center;
                overflow: elide;
            }
        }

        // Mic-hot indicator. Visible only while PTT is held. Positioned below
        // the top-trigger zone so it stays visible when the title bar is hidden.
        // A 600 ms opacity pulse driven by a Timer reads as "live" without
        // being distracting.
        if root.mic-hot: Rectangle {
            x: parent.width - 36px;
            y: 32px;
            width: 28px;
            height: 28px;
            background: #ffcc33;
            border-radius: 14px;
            opacity: root.mic-pulse ? 1.0 : 0.55;
            animate opacity { duration: 600ms; easing: ease-in-out; }
            drop-shadow-blur: 8px;
            drop-shadow-color: #ffcc3380;

            Timer {
                interval: 600ms;
                running: root.mic-hot;
                triggered() => { root.mic-pulse = !root.mic-pulse; }
            }

            LucideIcon {
                x: 5px; y: 5px;
                width: 18px; height: 18px;
                // lucide "mic"
                path-d: "M 12 1 a 3 3 0 0 0 -3 3 v 8 a 3 3 0 0 0 6 0 V 4 a 3 3 0 0 0 -3 -3 z M 19 10 v 2 a 7 7 0 0 1 -14 0 v -2 M 12 19 v 4 M 8 23 h 8";
                tint: #1a1a1e;
            }
        }

        // Left hover trigger — fades the tab strip in when cursor enters the left edge.
        left-trigger := TouchArea {
            x: 0px; y: 0px;
            width: 56px;
            height: parent.height;
        }

        // Floating vertical tab strip. Auto-hides like the top/bottom bars.
        tab-strip := Rectangle {
            x: 0px; y: 0px;
            width: 44px;
            height: parent.height;
            background: rgba(18, 18, 18, 0.92);
            opacity: (left-trigger.has-hover || HoverState.tab-cell-hover > 0 || root.strip-pinned) ? 1.0 : 0.0;
            animate opacity { duration: 180ms; easing: ease; }
            // Note: the auto-unpin timer lives in Rust (AppState::flash_tab_strip)
            // so consecutive cycle presses restart the countdown rather than
            // letting the first one's timer fire mid-flash.

            VerticalLayout {
                // Cells must start below the 60px top-trigger area; otherwise
                // top-trigger (declared later in z-order) steals their hover
                // and the strip fades while you're on the first tab.
                padding-top: 64px;
                padding-bottom: 64px;
                padding-left: 2px;
                padding-right: 2px;
                spacing: 2px;
                alignment: start;

                for tab[idx] in root.tabs: Rectangle {
                    width: 40px;
                    height: 40px;
                    background: idx == root.active-tab
                        ? #2c2c34
                        : (tab-touch.has-hover ? #1f1f23 : transparent);
                    border-radius: 4px;

                    // Active-tab accent bar on the left edge.
                    Rectangle {
                        x: 0px; y: 6px;
                        width: 3px;
                        height: parent.height - 12px;
                        background: idx == root.active-tab ? #ffcc33 : transparent;
                        border-radius: 1.5px;
                    }

                    LucideIcon {
                        x: 10px; y: 10px;
                        width: 20px;
                        height: 20px;
                        path-d: tab.icon-d;
                        tint: idx == root.active-tab
                            ? #ffffff
                            : (tab-touch.has-hover ? #ffcc33 : #aaaaaa);
                    }

                    tab-touch := TouchArea {
                        clicked => { root.tab-clicked(idx); }
                        changed has-hover => {
                            HoverState.tab-cell-hover = HoverState.tab-cell-hover
                                + (self.has-hover ? 1 : -1);
                        }
                    }
                }
            }
        }

        // Top hover trigger — fades the title bar in when cursor enters the top strip.
        top-trigger := TouchArea {
            x: 0px; y: 0px;
            width: parent.width;
            height: 60px;
        }

        // Floating slim toolbar (auto-hide). 22px tall, lucide-style icons.
        // Drag is only via the handle on the far left; rest of the bar is inert.
        // OR-ing each cell's `hovered` into the opacity expression keeps the
        // bar visible while the cursor sits on a button (otherwise the
        // button's TouchArea steals hover from the top-trigger).
        title-bar := Rectangle {
            x: 0px; y: 0px;
            width: parent.width;
            height: 22px;
            background: rgba(18, 18, 18, 0.9);
            opacity: top-trigger.has-hover
                || drag-handle.hovered
                || voice-cell.hovered
                || gear-cell.hovered
                || close-cell.hovered
                ? 1.0 : 0.0;
            animate opacity { duration: 180ms; easing: ease; }

            drag-handle := Rectangle {
                x: 0px; y: 0px;
                width: 22px; height: parent.height;
                background: drag-touch.has-hover ? #2a2a2a : transparent;
                out property <bool> hovered: drag-touch.has-hover;

                LucideIcon {
                    x: 4px; y: 4px;
                    width: parent.width - 8px;
                    height: parent.height - 8px;
                    // lucide "grip-vertical" — six filled dots.
                    path-d: "M 7.8 5 a 1.2 1.2 0 1 0 2.4 0 a 1.2 1.2 0 1 0 -2.4 0 z M 7.8 12 a 1.2 1.2 0 1 0 2.4 0 a 1.2 1.2 0 1 0 -2.4 0 z M 7.8 19 a 1.2 1.2 0 1 0 2.4 0 a 1.2 1.2 0 1 0 -2.4 0 z M 13.8 5 a 1.2 1.2 0 1 0 2.4 0 a 1.2 1.2 0 1 0 -2.4 0 z M 13.8 12 a 1.2 1.2 0 1 0 2.4 0 a 1.2 1.2 0 1 0 -2.4 0 z M 13.8 19 a 1.2 1.2 0 1 0 2.4 0 a 1.2 1.2 0 1 0 -2.4 0 z";
                    tint: #888;
                    filled: true;
                }
                drag-touch := TouchArea {
                    mouse-cursor: MouseCursor.move;
                    moved => {
                        if (self.pressed) {
                            root.drag-by(
                                self.mouse-x - self.pressed-x,
                                self.mouse-y - self.pressed-y,
                            );
                        }
                    }
                }
            }

            voice-cell := IconCell {
                x: parent.width - 66px;
                y: 0px;
                // lucide "mic" with line list-ish overlay — just use mic to keep
                // the title bar tidy. Mic icon was already defined for the hot
                // indicator; reuse the same path.
                icon-d: "M 12 1 a 3 3 0 0 0 -3 3 v 8 a 3 3 0 0 0 6 0 V 4 a 3 3 0 0 0 -3 -3 z M 19 10 v 2 a 7 7 0 0 1 -14 0 v -2 M 12 19 v 4 M 8 23 h 8";
                hover-bg: #333;
                hover-tint: #ffcc33;
                clicked => { root.voice-commands-open = !root.voice-commands-open; }
            }

            gear-cell := IconCell {
                x: parent.width - 44px;
                y: 0px;
                // lucide "settings" — gear outline + central circle.
                icon-d: "M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z M9 12 a 3 3 0 1 0 6 0 a 3 3 0 1 0 -6 0 z";
                hover-bg: #333;
                hover-tint: #ffcc33;
                clicked => { root.settings-open = !root.settings-open; }
            }

            close-cell := IconCell {
                x: parent.width - 22px;
                y: 0px;
                // lucide "x"
                icon-d: "M 18 6 L 6 18 M 6 6 L 18 18";
                hover-bg: #aa3333;
                clicked => { root.close-clicked(); }
            }
        }

        // Bottom hover trigger.
        bottom-trigger := TouchArea {
            x: 0px;
            y: parent.height - 60px;
            width: parent.width;
            height: 60px;
        }

        // Floating debug nav bar (auto-hide). Real navigation is shortcuts +
        // HOTAS maps (M4); the buttons are here for occasional manual control.
        // Slim 22px to match the top bar.
        footer := Rectangle {
            x: 0px;
            y: parent.height - 22px;
            width: parent.width;
            height: 22px;
            background: rgba(18, 18, 18, 0.9);
            opacity: bottom-trigger.has-hover
                || tab-cycle-prev-cell.hovered
                || tab-cycle-next-cell.hovered
                || heading-prev-cell.hovered
                || page-prev-cell.hovered
                || prev-cell.hovered
                || play-cell.hovered
                || next-cell.hovered
                || page-next-cell.hovered
                || heading-next-cell.hovered
                ? 1.0 : 0.0;
            animate opacity { duration: 180ms; easing: ease; }

            // Tab-cycle buttons on the far left, occupying the same 44px width
            // as the tab strip above. Both visible only with >1 tab.
            // Up = previous tab, Down = next tab.
            tab-cycle-prev-cell := IconCell {
                x: 0px;
                y: 0px;
                visible: root.tabs.length > 1;
                cell-w: 22px;
                cell-h: 22px;
                icon-pad: 4px;
                // lucide "chevron-up"
                icon-d: "M 18 15 L 12 9 L 6 15";
                clicked => {
                    root.tab-cycle-prev-clicked();
                    root.strip-pinned = true;
                }
            }
            tab-cycle-next-cell := IconCell {
                x: 22px;
                y: 0px;
                visible: root.tabs.length > 1;
                cell-w: 22px;
                cell-h: 22px;
                icon-pad: 4px;
                // lucide "chevron-down"
                icon-d: "M 6 9 L 12 15 L 18 9";
                clicked => {
                    root.tab-cycle-next-clicked();
                    root.strip-pinned = true;
                }
            }

            HorizontalLayout {
                padding-left: 6px;
                padding-right: 6px;
                padding-top: 0px;
                padding-bottom: 0px;
                spacing: 4px;
                alignment: center;

                page-prev-cell := IconCell {
                    cell-w: 22px;
                    cell-h: 22px;
                    icon-pad: 4px;
                    hover-tint: #ffcc33;
                    // lucide "chevron-first"  ( |<< )
                    icon-d: "M 17 18 L 11 12 L 17 6 M 7 6 L 7 18";
                    clicked => { root.page-prev-clicked(); }
                }
                heading-prev-cell := IconCell {
                    cell-w: 22px;
                    cell-h: 22px;
                    icon-pad: 4px;
                    hover-tint: #ffcc33;
                    // lucide "chevrons-left"  ( << )
                    icon-d: "M 11 17 L 6 12 L 11 7 M 18 17 L 13 12 L 18 7";
                    clicked => { root.prev-heading-clicked(); }
                }
                prev-cell := IconCell {
                    cell-w: 22px;
                    cell-h: 22px;
                    icon-pad: 4px;
                    hover-tint: #ffcc33;
                    // lucide "chevron-left"
                    icon-d: "M 15 18 L 9 12 L 15 6";
                    clicked => { root.prev-clicked(); }
                }
                play-cell := IconCell {
                    cell-w: 26px;
                    cell-h: 22px;
                    icon-pad: 5px;
                    hover-tint: #ffcc33;
                    // lucide "pause" when playing, "play" when not
                    icon-d: root.is-playing
                        ? "M 6 4 L 10 4 L 10 20 L 6 20 Z M 14 4 L 18 4 L 18 20 L 14 20 Z"
                        : "M 7 4 L 19 12 L 7 20 Z";
                    icon-filled: true;
                    clicked => { root.read-clicked(); }
                }
                next-cell := IconCell {
                    cell-w: 22px;
                    cell-h: 22px;
                    icon-pad: 4px;
                    hover-tint: #ffcc33;
                    // lucide "chevron-right"
                    icon-d: "M 9 18 L 15 12 L 9 6";
                    clicked => { root.next-clicked(); }
                }
                heading-next-cell := IconCell {
                    cell-w: 22px;
                    cell-h: 22px;
                    icon-pad: 4px;
                    hover-tint: #ffcc33;
                    // lucide "chevrons-right"  ( >> )
                    icon-d: "M 6 17 L 11 12 L 6 7 M 13 17 L 18 12 L 13 7";
                    clicked => { root.next-heading-clicked(); }
                }
                page-next-cell := IconCell {
                    cell-w: 22px;
                    cell-h: 22px;
                    icon-pad: 4px;
                    hover-tint: #ffcc33;
                    // lucide "chevron-last"  ( >>| )
                    icon-d: "M 7 18 L 13 12 L 7 6 M 17 6 L 17 18";
                    clicked => { root.page-next-clicked(); }
                }
            }
        }

        // Settings panel — bumped contrast (lighter bg, brighter text) so it
        // reads cleanly over any kneeboard page. Whole content is inside a
        // Flickable so it scrolls when the bindings list grows past the
        // viewport.
        if root.settings-open: Rectangle {
            x: 30px;
            y: 40px;
            width: 540px;
            height: 800px;
            background: #2a2a32;
            border-color: #555;
            border-width: 1px;
            border-radius: 8px;
            clip: true;

            settings-scroll := Flickable {
                x: 0px; y: 0px;
                width: parent.width;
                height: parent.height;
                viewport-height: settings-content.preferred-height;

                settings-content := VerticalLayout {
                    padding: 20px;
                    spacing: 14px;

                Text {
                    text: "Settings";
                    color: #ffffff;
                    font-size: 22px;
                    font-weight: 600;
                }

                Rectangle { height: 1px; background: #444; }

                Text { text: "AIRCRAFT"; color: #c0c0c0; font-size: 12px; font-weight: 500; }
                HorizontalLayout {
                    spacing: 6px;
                    alignment: start;
                    for entry[idx] in root.aircraft-list: Rectangle {
                        width: 110px;
                        height: 28px;
                        background: entry.id == root.current-aircraft
                            ? #ffcc33
                            : (aircraft-touch.has-hover ? #2e2e34 : #1e1e22);
                        border-color: entry.id == root.current-aircraft ? #e6b820 : #555;
                        border-width: 1px;
                        border-radius: 4px;
                        Text {
                            text: entry.label;
                            color: entry.id == root.current-aircraft ? #1a1a1e : #d8d8d8;
                            font-weight: entry.id == root.current-aircraft ? 600 : 400;
                            font-size: 12px;
                            horizontal-alignment: center;
                            vertical-alignment: center;
                            width: parent.width;
                            height: parent.height;
                            overflow: elide;
                        }
                        aircraft-touch := TouchArea {
                            clicked => { root.aircraft-clicked(entry.id); }
                        }
                    }
                }

                Rectangle { height: 8px; }
                Rectangle { height: 1px; background: #444; }

                Text { text: "BEHAVIOR"; color: #c0c0c0; font-size: 12px; font-weight: 500; }

                DarkCheckBox {
                    text: "Auto-read item after Next / Prev";
                    checked: root.auto-read;
                    toggled => {
                        root.auto-read = self.checked;
                        root.settings-changed();
                    }
                }
                DarkCheckBox {
                    text: "Auto-advance to next item after speaking";
                    checked: root.auto-advance;
                    toggled => {
                        root.auto-advance = self.checked;
                        root.settings-changed();
                    }
                }
                DarkCheckBox {
                    text: "Read supporting notes after each step";
                    checked: root.read-notes;
                    toggled => {
                        root.read-notes = self.checked;
                        root.settings-changed();
                    }
                }
                DarkCheckBox {
                    text: "Hot-reload pages from disk when files change";
                    checked: root.hot-reload;
                    toggled => {
                        root.hot-reload = self.checked;
                        root.settings-changed();
                    }
                }
                DarkCheckBox {
                    text: "Mute hot mic during speech";
                    checked: root.mute-mic-during-speech;
                    toggled => {
                        root.mute-mic-during-speech = self.checked;
                        root.settings-changed();
                    }
                }
                DarkCheckBox {
                    text: "Click-through (pass clicks to apps behind — bind toggle to HOTAS!)";
                    checked: root.click-through;
                    toggled => {
                        root.click-through = self.checked;
                        root.settings-changed();
                    }
                }
                HorizontalLayout {
                    spacing: 10px;
                    Text {
                        text: "Pause between items:";
                        color: #f0f0f0;
                        vertical-alignment: center;
                        font-size: 14px;
                    }
                    delay-slider := Slider {
                        minimum: 0.0;
                        maximum: 8.0;
                        value: root.advance-delay;
                        changed value => {
                            root.advance-delay = self.value;
                            root.settings-changed();
                        }
                    }
                    Text {
                        text: round(root.advance-delay * 10) / 10 + " s";
                        color: #f0f0f0;
                        vertical-alignment: center;
                        horizontal-alignment: right;
                        font-size: 14px;
                        font-weight: 500;
                        width: 56px;
                    }
                }
                HorizontalLayout {
                    spacing: 10px;
                    Text {
                        text: "Transcript pill duration:";
                        color: #f0f0f0;
                        vertical-alignment: center;
                        font-size: 14px;
                    }
                    pill-slider := Slider {
                        minimum: 0.0;
                        maximum: 20.0;
                        value: root.transcript-pill-seconds;
                        changed value => {
                            root.transcript-pill-seconds = self.value;
                            root.settings-changed();
                        }
                    }
                    Text {
                        text: round(root.transcript-pill-seconds * 10) / 10 + " s";
                        color: #f0f0f0;
                        vertical-alignment: center;
                        horizontal-alignment: right;
                        font-size: 14px;
                        font-weight: 500;
                        width: 56px;
                    }
                }
                HorizontalLayout {
                    spacing: 10px;
                    Text {
                        text: "Window opacity:";
                        color: #f0f0f0;
                        vertical-alignment: center;
                        font-size: 14px;
                    }
                    opacity-slider := Slider {
                        minimum: 0.3;
                        maximum: 1.0;
                        value: root.window-opacity;
                        changed value => {
                            root.window-opacity = self.value;
                            root.settings-changed();
                        }
                    }
                    Text {
                        text: round(root.window-opacity * 100) + " %";
                        color: #f0f0f0;
                        vertical-alignment: center;
                        horizontal-alignment: right;
                        font-size: 14px;
                        font-weight: 500;
                        width: 56px;
                    }
                }

                Rectangle { height: 8px; }
                Rectangle { height: 1px; background: #444; }

                Text { text: "AUDIO INPUT"; color: #c0c0c0; font-size: 12px; font-weight: 500; }
                Text {
                    text: "Microphone used for push-to-talk. The mic icon pulses while capturing.";
                    color: #a0a0a0;
                    font-size: 11px;
                    wrap: word-wrap;
                }
                VerticalLayout {
                    spacing: 2px;
                    for name[idx] in root.audio-inputs: Rectangle {
                        height: 26px;
                        background: name == root.audio-input-selected
                            ? #4a3a16
                            : (audio-touch.has-hover ? #1f1f23 : #1a1a1e);
                        border-color: name == root.audio-input-selected ? #ffcc33 : #333;
                        border-width: 1px;
                        border-radius: 3px;

                        HorizontalLayout {
                            padding-left: 8px;
                            padding-right: 8px;
                            spacing: 6px;
                            alignment: stretch;

                            // Small mic dot on the active row so the choice
                            // is recognisable at a glance.
                            Rectangle {
                                width: 8px;
                                background: name == root.audio-input-selected ? #ffcc33 : transparent;
                                border-radius: 4px;
                            }
                            Text {
                                text: name;
                                color: #f0f0f0;
                                font-size: 12px;
                                font-weight: name == root.audio-input-selected ? 600 : 400;
                                vertical-alignment: center;
                                horizontal-stretch: 1;
                                overflow: elide;
                            }
                        }

                        audio-touch := TouchArea {
                            clicked => { root.audio-input-clicked(name); }
                        }
                    }
                }

                Rectangle { height: 8px; }
                Rectangle { height: 1px; background: #444; }

                Text { text: "TEXT-TO-SPEECH"; color: #c0c0c0; font-size: 12px; font-weight: 500; }
                Text {
                    text: "WinRT uses your installed Windows voices (instant, OK quality). Piper is open-source neural TTS — drop a voice into models/piper/voices/ and pick it below. See models/piper/README.md.";
                    color: #a0a0a0;
                    font-size: 11px;
                    wrap: word-wrap;
                }
                HorizontalLayout {
                    spacing: 6px;
                    alignment: start;
                    Rectangle {
                        width: 120px;
                        height: 28px;
                        background: root.tts-engine-selected == "winrt"
                            ? #ffcc33
                            : (winrt-touch.has-hover ? #2e2e34 : #1e1e22);
                        border-color: root.tts-engine-selected == "winrt" ? #e6b820 : #555;
                        border-width: 1px;
                        border-radius: 4px;
                        Text {
                            text: "WinRT (system)";
                            color: root.tts-engine-selected == "winrt" ? #1a1a1e : #d8d8d8;
                            font-weight: root.tts-engine-selected == "winrt" ? 600 : 400;
                            font-size: 12px;
                            horizontal-alignment: center;
                            vertical-alignment: center;
                            width: parent.width;
                            height: parent.height;
                        }
                        winrt-touch := TouchArea {
                            clicked => { root.tts-engine-clicked("winrt"); }
                        }
                    }
                    Rectangle {
                        width: 120px;
                        height: 28px;
                        background: root.tts-engine-selected == "piper"
                            ? #ffcc33
                            : (piper-touch.has-hover ? #2e2e34 : #1e1e22);
                        border-color: root.tts-engine-selected == "piper" ? #e6b820 : #555;
                        border-width: 1px;
                        border-radius: 4px;
                        Text {
                            text: "Piper (neural)";
                            color: root.tts-engine-selected == "piper" ? #1a1a1e : #d8d8d8;
                            font-weight: root.tts-engine-selected == "piper" ? 600 : 400;
                            font-size: 12px;
                            horizontal-alignment: center;
                            vertical-alignment: center;
                            width: parent.width;
                            height: parent.height;
                        }
                        piper-touch := TouchArea {
                            clicked => { root.tts-engine-clicked("piper"); }
                        }
                    }
                    Rectangle {
                        width: 70px;
                        height: 28px;
                        background: test-touch.has-hover ? #2e2e34 : #1e1e22;
                        border-color: #555;
                        border-width: 1px;
                        border-radius: 4px;
                        Text {
                            text: "Test";
                            color: #d8d8d8;
                            font-size: 12px;
                            horizontal-alignment: center;
                            vertical-alignment: center;
                            width: parent.width;
                            height: parent.height;
                        }
                        test-touch := TouchArea {
                            clicked => { root.tts-test-clicked(); }
                        }
                    }
                }
                HorizontalLayout {
                    spacing: 10px;
                    Text {
                        text: "Speed:";
                        color: #f0f0f0;
                        vertical-alignment: center;
                        font-size: 14px;
                        width: 70px;
                    }
                    rate-slider := Slider {
                        minimum: 0.5;
                        maximum: 2.0;
                        value: root.tts-rate;
                        changed value => {
                            root.tts-rate = self.value;
                            root.settings-changed();
                        }
                    }
                    Text {
                        text: round(root.tts-rate * 100) / 100 + "x";
                        color: #f0f0f0;
                        vertical-alignment: center;
                        horizontal-alignment: right;
                        font-size: 14px;
                        font-weight: 500;
                        width: 56px;
                    }
                }
                HorizontalLayout {
                    spacing: 10px;
                    Text {
                        text: "Volume:";
                        color: #f0f0f0;
                        vertical-alignment: center;
                        font-size: 14px;
                        width: 70px;
                    }
                    vol-slider := Slider {
                        minimum: 0.0;
                        maximum: 1.0;
                        value: root.tts-volume;
                        changed value => {
                            root.tts-volume = self.value;
                            root.settings-changed();
                        }
                    }
                    Text {
                        text: round(root.tts-volume * 100) + "%";
                        color: #f0f0f0;
                        vertical-alignment: center;
                        horizontal-alignment: right;
                        font-size: 14px;
                        font-weight: 500;
                        width: 56px;
                    }
                }
                if root.tts-engine-selected == "piper": VerticalLayout {
                    spacing: 2px;
                    Text {
                        text: root.piper-voices.length > 0
                            ? "Available voices:"
                            : "No voices in models/piper/voices/ — see README.";
                        color: #a0a0a0;
                        font-size: 11px;
                    }
                    for voice[idx] in root.piper-voices: Rectangle {
                        height: 26px;
                        background: voice == root.piper-voice-selected
                            ? #4a3a16
                            : (pv-touch.has-hover ? #1f1f23 : #1a1a1e);
                        border-color: voice == root.piper-voice-selected ? #ffcc33 : #333;
                        border-width: 1px;
                        border-radius: 3px;

                        HorizontalLayout {
                            padding-left: 8px;
                            padding-right: 8px;
                            spacing: 6px;
                            alignment: stretch;
                            Rectangle {
                                width: 8px;
                                background: voice == root.piper-voice-selected ? #ffcc33 : transparent;
                                border-radius: 4px;
                            }
                            Text {
                                text: voice;
                                color: #f0f0f0;
                                font-size: 12px;
                                font-weight: voice == root.piper-voice-selected ? 600 : 400;
                                vertical-alignment: center;
                                horizontal-stretch: 1;
                                overflow: elide;
                            }
                        }
                        pv-touch := TouchArea {
                            clicked => { root.piper-voice-clicked(voice); }
                        }
                    }
                }

                Rectangle { height: 8px; }
                Rectangle { height: 1px; background: #444; }

                Text { text: "BINDINGS"; color: #c0c0c0; font-size: 12px; font-weight: 500; }
                Text {
                    text: "Click a row to bind a new key. Press Esc to cancel. Clear removes the binding.";
                    color: #a0a0a0;
                    font-size: 11px;
                    wrap: word-wrap;
                }
                DarkCheckBox {
                    text: "Test mode — flash matched row, don't fire actions";
                    checked: root.binding-test-mode;
                    toggled => {
                        root.binding-test-mode = self.checked;
                        root.binding-test-mode-toggled(self.checked);
                    }
                }

                // List of every action + its current binding. Outer panel
                // Flickable handles scrolling, so this is a plain layout.
                VerticalLayout {
                    spacing: 2px;
                    for row[idx] in root.bindings: Rectangle {
                            property <bool> flashing: root.binding-flash-id == row.action-id;
                            height: 26px;
                            background: row.capturing
                                ? #4a3a16
                                : (flashing
                                    ? #6e5a1a
                                    : (row-touch.has-hover ? #1f1f23 : #1a1a1e));
                            border-color: row.capturing
                                ? #ffcc33
                                : (flashing ? #ffcc33 : #333);
                            border-width: 1px;
                            border-radius: 3px;
                            animate background { duration: 500ms; easing: ease-out; }
                            animate border-color { duration: 500ms; easing: ease-out; }

                            // Row-wide TouchArea declared FIRST so the icon
                            // TouchAreas below sit on top and consume their
                            // own clicks. The whole row remains clickable
                            // for convenience.
                            row-touch := TouchArea {
                                clicked => { root.binding-edit-clicked(row.action-id); }
                            }

                            HorizontalLayout {
                                padding-left: 8px;
                                padding-right: 4px;
                                spacing: 6px;
                                alignment: stretch;

                                Text {
                                    text: row.label;
                                    color: #f0f0f0;
                                    font-size: 12px;
                                    vertical-alignment: center;
                                    horizontal-stretch: 1;
                                    overflow: elide;
                                }

                                Text {
                                    text: row.capturing ? "Press a key…" : row.trigger-text;
                                    color: row.capturing
                                        ? #ffcc33
                                        : (row.trigger-text == "(unbound)" ? #777 : #d0d0d0);
                                    font-size: 12px;
                                    font-weight: row.capturing ? 600 : 400;
                                    vertical-alignment: center;
                                    horizontal-alignment: right;
                                    width: 140px;
                                }

                                edit-cell := IconCell {
                                    cell-w: 22px;
                                    cell-h: 22px;
                                    icon-pad: 5px;
                                    visible: !row.capturing;
                                    // lucide "pencil"
                                    icon-d: "M 12 20 h 9 M 16.5 3.5 a 2.121 2.121 0 0 1 3 3 L 7 19 L 3 20 L 4 16 L 16.5 3.5 z";
                                    hover-bg: #2a2a2a;
                                    hover-tint: #ffcc33;
                                    clicked => { root.binding-edit-clicked(row.action-id); }
                                }
                                clear-cell := IconCell {
                                    cell-w: 22px;
                                    cell-h: 22px;
                                    icon-pad: 5px;
                                    visible: row.trigger-text != "(unbound)" && !row.capturing;
                                    // lucide "x" — removes all triggers for this action
                                    icon-d: "M 18 6 L 6 18 M 6 6 L 18 18";
                                    hover-bg: #553333;
                                    hover-tint: #ff7777;
                                    clicked => { root.binding-clear-clicked(row.action-id); }
                                }
                            }
                        }
                    }

                    HorizontalLayout {
                        alignment: end;
                        Button {
                            text: "Close";
                            clicked => { root.settings-open = false; }
                        }
                    }
                }       // settings-content VerticalLayout
            }           // outer Flickable

            // Custom scrollbar overlay so users see there's more below the
            // fold. Hidden when content fits. Track is the right edge of the
            // panel; thumb is a translucent yellow chip sized proportionally
            // to the visible fraction and positioned by viewport-y.
            if settings-scroll.viewport-height > settings-scroll.height: Rectangle {
                x: parent.width - 8px;
                y: 6px;
                width: 4px;
                height: parent.height - 12px;
                background: rgba(255, 255, 255, 0.06);
                border-radius: 2px;

                Rectangle {
                    x: 0px;
                    // viewport-y is negative as content scrolls up.
                    y: -settings-scroll.viewport-y
                        / (settings-scroll.viewport-height - settings-scroll.height)
                        * (parent.height - self.height);
                    width: parent.width;
                    height: max(
                        24px,
                        parent.height * settings-scroll.height / settings-scroll.viewport-height
                    );
                    background: rgba(255, 204, 51, 0.55);
                    border-radius: 2px;
                }
            }
        }               // settings-panel Rectangle

        // Voice-commands help panel — same style as Settings, read-only.
        if root.voice-commands-open: Rectangle {
            x: 30px;
            y: 40px;
            width: 540px;
            height: 800px;
            background: #2a2a32;
            border-color: #555;
            border-width: 1px;
            border-radius: 8px;
            clip: true;

            voice-scroll := Flickable {
                x: 0px; y: 0px;
                width: parent.width;
                height: parent.height;
                viewport-height: voice-content.preferred-height;

                voice-content := VerticalLayout {
                    padding: 20px;
                    spacing: 10px;

                    Text {
                        text: "Voice commands";
                        color: #ffffff;
                        font-size: 22px;
                        font-weight: 600;
                    }
                    Rectangle { height: 1px; background: #444; }
                    Text {
                        text: "Hold push-to-talk (or use hot mic) and say any of these. Commands match case-insensitively and tolerate trailing punctuation. Below the commands, free-form queries (\"go to ...\") accept fuzzy targets — section names, weapon designators, tab labels — and rewrite phonetic spellings via query_aliases.toml.";
                        color: #a0a0a0;
                        font-size: 11px;
                        wrap: word-wrap;
                    }
                    Rectangle { height: 4px; }

                    for row[idx] in root.voice-commands: Rectangle {
                        // Header rows: taller, transparent background, no
                        // border, light grey uppercase title with a bottom
                        // divider so the section it introduces reads as one
                        // visual group.
                        height: row.is-header ? 36px : 44px;
                        background: row.is-header ? transparent : #1a1a1e;
                        border-color: row.is-header ? transparent : #333;
                        border-width: 1px;
                        border-radius: 3px;

                        if row.is-header: VerticalLayout {
                            padding-left: 2px;
                            padding-top: 14px;
                            padding-bottom: 2px;
                            spacing: 0px;
                            Text {
                                text: row.action-label;
                                color: #e6b820;
                                font-size: 13px;
                                font-weight: 700;
                            }
                            Rectangle {
                                height: 1px;
                                background: #444;
                            }
                        }

                        if !row.is-header: VerticalLayout {
                            padding-left: 10px;
                            padding-right: 10px;
                            padding-top: 4px;
                            padding-bottom: 4px;
                            spacing: 2px;
                            Text {
                                text: row.action-label;
                                color: #ffcc33;
                                font-size: 12px;
                                font-weight: 600;
                            }
                            Text {
                                text: row.phrases;
                                color: #d0d0d0;
                                font-size: 11px;
                                overflow: elide;
                            }
                        }
                    }

                    Rectangle { height: 8px; }
                    HorizontalLayout {
                        alignment: end;
                        Button {
                            text: "Close";
                            clicked => { root.voice-commands-open = false; }
                        }
                    }
                }
            }

            if voice-scroll.viewport-height > voice-scroll.height: Rectangle {
                x: parent.width - 8px;
                y: 6px;
                width: 4px;
                height: parent.height - 12px;
                background: rgba(255, 255, 255, 0.06);
                border-radius: 2px;

                Rectangle {
                    x: 0px;
                    y: -voice-scroll.viewport-y
                        / (voice-scroll.viewport-height - voice-scroll.height)
                        * (parent.height - self.height);
                    width: parent.width;
                    height: max(
                        24px,
                        parent.height * voice-scroll.height / voice-scroll.viewport-height
                    );
                    background: rgba(255, 204, 51, 0.55);
                    border-radius: 2px;
                }
            }
        }
    }                   // MainWindow
}                       // slint! macro

const DISPLAY_W: f32 = 600.0;
const DISPLAY_H: f32 = 900.0;

/// Build the configured TTS engine. Returns the engine plus an
/// optional warning describing a non-fatal fallback (e.g. piper was
/// selected but its exe / voice was missing so we used WinRT). The
/// warning is surfaced via the critical pill so the user can see why
/// the engine they chose isn't actually running.
fn init_tts(settings: &Settings) -> Result<(Box<dyn TtsEngine>, Option<String>)> {
    #[cfg(windows)]
    {
        if settings.tts_engine == "piper" {
            let piper_exe = piper_exe_path();
            let voice = settings.tts_piper_voice.as_ref().map(PathBuf::from);
            if let Some(voice) = voice {
                match tts::PiperTts::new(piper_exe.clone(), voice.clone()) {
                    Ok(p) => return Ok((Box::new(p), None)),
                    Err(e) => {
                        let msg = format!("Piper init failed — using WinRT ({e})");
                        eprintln!("[tts] {msg}");
                        return Ok((Box::new(tts::WinRtTts::new()?), Some(msg)));
                    }
                }
            } else {
                let msg = "Piper selected but no voice configured — using WinRT".to_string();
                eprintln!("[tts] {msg}");
                return Ok((Box::new(tts::WinRtTts::new()?), Some(msg)));
            }
        }
        Ok((Box::new(tts::WinRtTts::new()?), None))
    }
    #[cfg(not(windows))]
    {
        let _ = settings;
        Ok((Box::new(tts::NoopTts::new()?), None))
    }
}

#[cfg(windows)]
fn piper_exe_path() -> PathBuf {
    PathBuf::from("models/piper/piper.exe")
}

/// Scan `models/piper/voices/` for `.onnx` files. Each one paired with a
/// `.onnx.json` counts as a usable voice.
fn list_piper_voices() -> Vec<PathBuf> {
    let dir = PathBuf::from("models/piper/voices");
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return out;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().is_some_and(|ext| ext == "onnx") {
            let cfg = p.with_extension("onnx.json");
            if cfg.exists() {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

fn last_navigable(items: &[Item]) -> usize {
    items
        .iter()
        .rposition(|i| i.navigable)
        .unwrap_or(items.len().saturating_sub(1))
}

fn next_navigable_in_page(items: &[Item], from: usize, dir: i32) -> usize {
    let n = items.len();
    if n == 0 {
        return 0;
    }
    let mut i = from as i32;
    loop {
        i += dir;
        if i < 0 || i as usize >= n {
            return from;
        }
        if items[i as usize].navigable {
            return i as usize;
        }
    }
}

fn step_cursor(pages: &[LoadedPage], cur: Cursor, dir: i32) -> Cursor {
    let items = &pages[cur.page].manifest.items;
    let next_in_page = next_navigable_in_page(items, cur.item, dir);
    if next_in_page != cur.item {
        return Cursor { page: cur.page, item: next_in_page };
    }
    if dir > 0 && cur.page + 1 < pages.len() {
        let np = cur.page + 1;
        Cursor { page: np, item: first_navigable(&pages[np].manifest.items) }
    } else if dir < 0 && cur.page > 0 {
        let pp = cur.page - 1;
        Cursor { page: pp, item: last_navigable(&pages[pp].manifest.items) }
    } else {
        cur
    }
}

/// Walk to the next/previous heading. Crosses pages like step_cursor does.
/// Compute the armed cursor placement for a freshly-jumped section.
/// Returns `(first_nav_idx, heading_idx)` where `first_nav_idx` is the
/// item the cursor should land on (the first navigable item of the
/// section) and `heading_idx` is the section header to be spoken — None
/// if the section has no preceding heading on this page (page-title
/// fallback handled by the caller).
///
/// `cursor_item` may point at either the heading itself (when the
/// caller used `step_to_heading`) or at any item inside the section
/// (when the caller used a NavTarget from a query resolver). Both paths
/// produce the same result. Issue #17.
fn place_armed_cursor(items: &[tabs::Item], cursor_item: usize) -> (usize, Option<usize>) {
    if items.is_empty() {
        return (0, None);
    }
    let item_idx = cursor_item.min(items.len() - 1);
    if is_heading(&items[item_idx]) {
        let first_nav = items
            .iter()
            .enumerate()
            .skip(item_idx + 1)
            .find(|(_, it)| it.navigable)
            .map(|(idx, _)| idx)
            .unwrap_or(item_idx);
        (first_nav, Some(item_idx))
    } else {
        let heading = items[..=item_idx].iter().rposition(is_heading);
        (item_idx, heading)
    }
}

fn step_to_heading(pages: &[LoadedPage], cur: Cursor, dir: i32) -> Cursor {
    let items = &pages[cur.page].manifest.items;
    let n = items.len() as i32;
    let mut i = cur.item as i32;
    loop {
        i += dir;
        if i < 0 || i >= n {
            break;
        }
        if is_heading(&items[i as usize]) {
            return Cursor { page: cur.page, item: i as usize };
        }
    }
    if dir > 0 {
        for (p, page) in pages.iter().enumerate().skip(cur.page + 1) {
            if let Some((idx, _)) = page
                .manifest
                .items
                .iter()
                .enumerate()
                .find(|(_, it)| is_heading(it))
            {
                return Cursor { page: p, item: idx };
            }
        }
    } else if cur.page > 0 {
        for p in (0..cur.page).rev() {
            if let Some((idx, _)) = pages[p]
                .manifest
                .items
                .iter()
                .enumerate()
                .rev()
                .find(|(_, it)| is_heading(it))
            {
                return Cursor { page: p, item: idx };
            }
        }
    }
    cur
}

fn jump_page(pages: &[LoadedPage], cur: Cursor, dir: i32) -> Cursor {
    let new_page = match dir {
        d if d > 0 => (cur.page + 1).min(pages.len().saturating_sub(1)),
        d if d < 0 => cur.page.saturating_sub(1),
        _ => cur.page,
    };
    Cursor { page: new_page, item: first_navigable(&pages[new_page].manifest.items) }
}

fn apply_cursor(win: &MainWindow, tab: &tabs::Tab) {
    if tab.pages.is_empty() {
        win.set_page_image(slint::Image::default());
        win.set_page_title(SharedString::from(tab.label.clone()));
        win.set_item_group(SharedString::default());
        win.set_item_text(SharedString::from(
            tab.load_error
                .clone()
                .unwrap_or_else(|| format!("({} is empty)", tab.label)),
        ));
        win.set_item_counter(SharedString::default());
        win.set_hl_x(0.0);
        win.set_hl_y(0.0);
        win.set_hl_w(0.0);
        win.set_hl_h(0.0);
        return;
    }

    let cur = tab.cursor;
    let page_idx = cur.page.min(tab.pages.len() - 1);
    let page = &tab.pages[page_idx];
    let item_idx = cur.item.min(page.manifest.items.len().saturating_sub(1));
    let item = page.manifest.items.get(item_idx);

    win.set_page_image(page.image());

    // Evict every other decoded page so dormant pixel buffers don't pile up.
    // The next nav decodes its target on demand; load time is single-digit ms
    // for typical 1358×2037 PNGs which is fine.
    for (i, p) in tab.pages.iter().enumerate() {
        if i != page_idx {
            p.evict_image();
        }
    }
    win.set_page_title(SharedString::from(format!(
        "{} · P{}/{}",
        page.manifest.title,
        page_idx + 1,
        tab.pages.len()
    )));

    match item {
        Some(item) if is_step(item) || is_heading(item) || is_note(item) => {
            let scale_x = DISPLAY_W / page.manifest.image_size[0] as f32;
            let scale_y = DISPLAY_H / page.manifest.image_size[1] as f32;
            let [bx, by, bw, bh] = item.bbox;
            win.set_hl_x(bx * scale_x);
            win.set_hl_y(by * scale_y);
            win.set_hl_w(bw * scale_x);
            win.set_hl_h(bh * scale_y);
        }
        _ => {
            // Image-folder pages and other whole-page items: hide highlight.
            win.set_hl_x(0.0);
            win.set_hl_y(0.0);
            win.set_hl_w(0.0);
            win.set_hl_h(0.0);
        }
    }

    win.set_item_group(SharedString::from(item.map(|i| i.group.as_str()).unwrap_or("")));
    win.set_item_text(SharedString::from(item.map(|i| i.text.as_str()).unwrap_or("")));
    win.set_item_counter(SharedString::from(format!(
        "{}/{}",
        item_idx + 1,
        page.manifest.items.len()
    )));
}

/// Estimate speech duration in milliseconds so auto-advance can schedule the
/// next item without subscribing to the WinRT `MediaEnded` event (which would
/// require cross-thread marshalling). Tuned conservatively: 130 wpm with a
/// 500 ms floor so short phrases get a beat of room.
fn estimate_speech_ms(text: &str) -> u32 {
    let word_count = text.split_whitespace().count().max(1) as f32;
    let ms = (word_count / 130.0) * 60_000.0;
    (ms.max(500.0) * 1.15) as u32
}

#[derive(Clone)]
struct AppState {
    tabs: Rc<RefCell<TabRegistry>>,
    tts: Rc<RefCell<Option<Box<dyn TtsEngine>>>>,
    pronunciation: Rc<RefCell<PronunciationConfig>>,
    /// Voice-query rewrites (e.g. "mark" → "mk"). Applied before fuzzy
    /// matching in section/tab resolvers. Hot-reloaded by F5 alongside
    /// pronunciation.toml.
    aliases: Rc<RefCell<QueryAliases>>,
    settings: Rc<RefCell<Settings>>,
    win: slint::Weak<MainWindow>,
    advance_timer: Rc<slint::Timer>,
    /// `Some(action)` while the user is in capture mode for that action.
    /// Set by `binding-edit-clicked`, consumed (or cleared on Esc) by the
    /// next handle-key invocation.
    capture: Rc<RefCell<Option<Action>>>,
    /// Long-lived cpal input stream. `None` if no mic was available at boot.
    /// `RefCell` so it can be torn down + rebuilt when the user picks a
    /// different input device in settings.
    audio: Rc<RefCell<Option<AudioCapture>>>,
    /// Single-shot timer that expires the transient pill message after
    /// `transcript_pill_seconds` so the pill fades out (or falls back
    /// to whatever sticky source is active — issue #17D).
    transcript_timer: Rc<slint::Timer>,
    /// Sticky pill source (issue #17D). When `Some`, beats any
    /// transient message — the armed-state cue lives here so a
    /// freshly-arrived STT transcript can't displace it. Cleared by
    /// `set_armed_state(false)`.
    pill_sticky: Rc<RefCell<Option<PillMessage>>>,
    /// Transient pill source. Expires via `transcript_timer`. Toasts,
    /// STT transcripts, "(no speech)" messages — anything that should
    /// fade after a few seconds rather than holding indefinitely.
    pill_transient: Rc<RefCell<Option<PillMessage>>>,
    /// Critical-error pill source. Beats both sticky and transient so
    /// a silent-failure message (whisper model missing, audio device
    /// gone, piper fallback) is impossible to miss. Latest critical
    /// replaces the previous; the queue is single-slot deliberately —
    /// concurrent startup failures are rare enough that latest-wins +
    /// full detail in the log is the right trade.
    pill_critical: Rc<RefCell<Option<PillMessage>>>,
    /// Single-shot timer that expires the critical pill. Held longer
    /// than the transcript timer (15 s) so the user has time to read.
    critical_timer: Rc<slint::Timer>,
    /// PCM sender into the whisper worker thread. `None` if no model loaded.
    stt_tx: Rc<Option<std::sync::mpsc::Sender<stt::SttCommand>>>,
    /// Issue #15: STT tuning — vocabulary (for whisper `initial_prompt`),
    /// post-STT corrections, and fuzzy-match threshold. Loaded from
    /// `config.toml` at startup; the vocabulary is rebuilt and pushed
    /// to the worker on every aircraft switch.
    stt_config: Rc<config::SttConfig>,
    /// Single-shot timer that clears the bindings test-mode flash highlight.
    binding_flash_timer: Rc<slint::Timer>,
    /// True when the window has been moved offscreen by ToggleVisibility.
    /// `saved_pos` holds the position to restore on the next toggle.
    window_hidden: Rc<std::cell::Cell<bool>>,
    saved_pos: Rc<RefCell<Option<slint::PhysicalPosition>>>,
    /// Auto-unpin timer for the left tab strip flash. Restarted on every
    /// cycle so rapid presses keep the strip visible for the full window.
    strip_pin_timer: Rc<slint::Timer>,
    /// Filesystem watcher for hot-reload. Active only when `settings.hot_reload`
    /// is true; target follows the active tab's source path.
    watcher: Rc<RefCell<watcher::Watcher>>,
    watcher_tx: std::sync::mpsc::Sender<PathBuf>,
    /// True while a HotMicToggle press has started capture and is waiting
    /// for a second press to stop. Distinct from PushToTalk's edge model.
    mic_locked: Rc<std::cell::Cell<bool>>,
    /// Polls the audio buffer every 200 ms while hot mic is latched,
    /// shipping detected utterances to STT as they complete.
    hotmic_timer: Rc<slint::Timer>,
    /// Issue #17: the cursor sits on the first navigable item of a
    /// freshly-jumped section, the section header has been spoken, and
    /// we're waiting for an explicit start/go/ok/Next before reading the
    /// step. Linear navigation never sets this — only NextHeading,
    /// PrevHeading, and NavigateToSection. Cleared by Next, Cancel, any
    /// action that isn't repeat-while-armed, or another section jump
    /// (which immediately re-arms).
    armed: Rc<std::cell::Cell<bool>>,
    /// Snapshot of (tab_idx, cursor) captured at the start of each
    /// section jump so `Previous` while armed can return the user to
    /// where they were before the jump rather than stepping into the
    /// previous section's last item. Chained jumps overwrite — Previous
    /// unwinds one jump at a time. None when no jump is pending.
    pre_jump_cursor: Rc<RefCell<Option<(usize, Cursor)>>>,
}

/// One message worth showing in the pill — issue #17D's generic
/// replacement for the old transcript+armed-only chrome. Sources push
/// these into `AppState::pill_sticky` (armed state) or
/// `pill_transient` (toasts, transcripts); `apply_pill()` renders
/// whichever wins.
#[derive(Debug, Clone)]
struct PillMessage {
    text: String,
    /// Lucide `path-d` string, drawn left of the text. Empty string
    /// suppresses the icon — used when `pulse` is on (the pulse dot
    /// occupies the same slot) or for plain status toasts.
    icon_d: &'static str,
    /// Tint applied to both the icon and the pulse dot.
    icon_tint: slint::Color,
    /// Border colour around the pill. `transparent` for the default
    /// chrome-less look used by transient messages.
    border_color: slint::Color,
    /// When true, a pulse dot replaces the icon (sticky "alive"
    /// signal — currently only the armed state).
    pulse: bool,
}

impl PillMessage {
    /// Stock cue for the armed-after-section-jump wait (issue #17B).
    /// Sticky source — cleared explicitly when the user disarms.
    fn armed_cue() -> Self {
        Self {
            text: "Armed — say go".to_string(),
            icon_d: "",
            icon_tint: slint::Color::from_rgb_u8(0xff, 0xaa, 0x33),
            border_color: slint::Color::from_rgb_u8(0xff, 0xaa, 0x33),
            pulse: true,
        }
    }

    /// Voice-command-recognised cue: lucide check, yellow tint, no
    /// border. Same visual as the pre-#17D `show_transcript_match`.
    fn transcript_match(text: String) -> Self {
        Self {
            text,
            icon_d: "M 20 6 L 9 17 L 4 12",
            icon_tint: slint::Color::from_rgb_u8(0xff, 0xcc, 0x33),
            border_color: slint::Color::from_argb_u8(0, 0, 0, 0),
            pulse: false,
        }
    }

    /// Voice-command-unmatched cue: lucide x, red tint.
    fn transcript_unmatched(text: String) -> Self {
        Self {
            text,
            icon_d: "M 18 6 L 6 18 M 6 6 L 18 18",
            icon_tint: slint::Color::from_rgb_u8(0xff, 0x77, 0x77),
            border_color: slint::Color::from_argb_u8(0, 0, 0, 0),
            pulse: false,
        }
    }

    /// Plain status toast: no icon, no border, default tint. Used for
    /// "(no speech)", "Click-through ON/OFF", "Transcribing 1.2s…",
    /// etc.
    fn status(text: String) -> Self {
        Self {
            text,
            icon_d: "",
            icon_tint: slint::Color::from_rgb_u8(0xff, 0xcc, 0x33),
            border_color: slint::Color::from_argb_u8(0, 0, 0, 0),
            pulse: false,
        }
    }

    /// Lowest-priority "we hear you" cue, shown only while PTT is
    /// held AND no other pill source has content. Same pulse dot as
    /// the armed cue but yellow tint + no border so it doesn't look
    /// like an armed-state lookalike. Disappears the instant PTT
    /// releases (the transient "Transcribing X.Xs..." takes over).
    fn listening() -> Self {
        Self {
            text: "Listening…".to_string(),
            icon_d: "",
            icon_tint: slint::Color::from_rgb_u8(0xff, 0xcc, 0x33),
            border_color: slint::Color::from_argb_u8(0, 0, 0, 0),
            pulse: true,
        }
    }

    /// Critical-error cue: lucide warning triangle, red tint, red
    /// border. Used for silent-failure scenarios that previously only
    /// hit the log (whisper model missing, audio device gone, piper
    /// fallback when piper was selected). Distinct from
    /// `transcript_unmatched`'s red `x` by both icon and border so the
    /// two don't get confused.
    fn critical(text: String) -> Self {
        Self {
            text,
            icon_d: "M 10.29 3.86 L 1.82 18 a 2 2 0 0 0 1.71 3 h 16.94 a 2 2 0 0 0 1.71 -3 L 13.71 3.86 a 2 2 0 0 0 -3.42 0 z M 12 9 v 4 M 12 17 h 0.01",
            icon_tint: slint::Color::from_rgb_u8(0xff, 0x77, 0x77),
            border_color: slint::Color::from_rgb_u8(0xff, 0x55, 0x55),
            pulse: false,
        }
    }
}

impl AppState {
    fn apply(&self) {
        let Some(win) = self.win.upgrade() else { return };
        let tabs = self.tabs.borrow();
        if let Some(tab) = tabs.active_tab() {
            apply_cursor(&win, tab);
        }
    }

    fn speak_current(&self, include_page_header: bool) -> u32 {
        let tabs = self.tabs.borrow();
        let Some(tab) = tabs.active_tab() else { return 0; };
        if tab.pages.is_empty() { return 0; }
        let cur = tab.cursor;
        let page_idx = cur.page.min(tab.pages.len() - 1);
        let page = &tab.pages[page_idx];
        if page.manifest.items.is_empty() { return 0; }
        let item_idx = cur.item.min(page.manifest.items.len() - 1);
        let item = &page.manifest.items[item_idx];

        let pron = self.pronunciation.borrow();
        let mut to_say = String::new();

        // When we've just swapped to a new page, announce the page title and
        // the most-recent preceding heading so the listener has context before
        // the first checklist item.
        if include_page_header {
            let page_title = spoken_for(&page.manifest.title, None, &pron);
            if !page_title.is_empty() {
                to_say.push_str(&page_title);
                to_say.push_str(". ");
            }
            if let Some(heading) = page.manifest.items[..=item_idx]
                .iter()
                .rev()
                .find(|i| is_heading(i))
            {
                let h = spoken_for(&heading.text, heading.spoken.as_deref(), &pron);
                if !h.is_empty() {
                    to_say.push_str(&h);
                    to_say.push_str(". ");
                }
            }
        }

        to_say.push_str(&spoken_for(&item.text, item.spoken.as_deref(), &pron));

        if self.settings.borrow().read_notes && is_step(item) {
            for following in &page.manifest.items[(item_idx + 1).min(page.manifest.items.len())..] {
                if is_note(following) {
                    let nt = spoken_for(&following.text, following.spoken.as_deref(), &pron);
                    to_say.push_str(". ");
                    to_say.push_str(&nt);
                } else {
                    break;
                }
            }
        }
        drop(pron);
        drop(tabs);

        let est = estimate_speech_ms(&to_say);
        eprintln!("[tts] speaking ({est} ms est): {to_say}");
        if let Some(engine) = self.tts.borrow_mut().as_mut() {
            if let Err(e) = engine.speak(&to_say, true) {
                eprintln!("TTS speak failed: {e:?}");
            }
        }
        est
    }

    fn set_playing(&self, playing: bool) {
        if let Some(win) = self.win.upgrade() {
            win.set_is_playing(playing);
        }
    }

    fn is_playing(&self) -> bool {
        self.win.upgrade().map(|w| w.get_is_playing()).unwrap_or(false)
    }

    /// Cancel any in-flight speech + pending auto-advance; flag as not playing.
    fn stop_speaking(&self) {
        self.advance_timer.stop();
        if let Some(engine) = self.tts.borrow_mut().as_mut() {
            let _ = engine.stop();
        }
        self.set_playing(false);
    }

    /// Start speaking the current item; schedule a post-speech tick that
    /// either advances (if auto_advance) or clears the playing flag.
    fn start_speaking(&self) {
        self.start_speaking_with_header(false);
    }

    /// Variant that prepends the page title + section heading. Used when nav
    /// crosses a page boundary so the listener gets context before the item.
    fn start_speaking_with_header(&self, include_page_header: bool) {
        let est = self.speak_current(include_page_header);
        if est == 0 {
            return;
        }
        self.set_playing(true);
        self.schedule_post_speech(est);
    }

    fn schedule_post_speech(&self, speech_ms: u32) {
        let delay_ms = if self.settings.borrow().auto_advance {
            (self.settings.borrow().advance_delay_sec * 1000.0) as u32
        } else {
            0
        };
        let total_ms = (speech_ms + delay_ms) as u64;
        let me = self.clone();
        self.advance_timer.start(
            slint::TimerMode::SingleShot,
            Duration::from_millis(total_ms),
            move || me.post_speech_tick(),
        );
    }

    /// Fired ~when speech finishes. Auto-advances if enabled; otherwise just
    /// clears the playing flag so the button label flips back to "Read".
    fn post_speech_tick(&self) {
        // While armed (issue #17) auto-advance is suppressed — the user
        // must say start/go/ok before any reading resumes. The arm path
        // itself doesn't schedule this tick; this guard catches any
        // other path that does (e.g. ReadSection while armed scheduled
        // a tick before we re-enter, though arm_after_section_jump now
        // takes that case).
        if self.armed.get() {
            self.set_playing(false);
            return;
        }
        if self.settings.borrow().auto_advance {
            let (advanced, page_changed) = {
                let mut tabs = self.tabs.borrow_mut();
                let Some(tab) = tabs.active_tab_mut() else { return };
                let old_page = tab.cursor.page;
                let new = step_cursor(&tab.pages, tab.cursor, 1);
                if new == tab.cursor {
                    (false, false)
                } else {
                    tab.cursor = new;
                    (true, new.page != old_page)
                }
            };
            if !advanced {
                self.set_playing(false);
                return;
            }
            self.apply();
            self.start_speaking_with_header(page_changed);
        } else {
            self.set_playing(false);
        }
    }

    fn nav(&self, step: impl FnOnce(&[LoadedPage], Cursor) -> Cursor) {
        self.stop_speaking();
        let page_changed = {
            let mut tabs = self.tabs.borrow_mut();
            let Some(tab) = tabs.active_tab_mut() else { return };
            let old_page = tab.cursor.page;
            tab.cursor = step(&tab.pages, tab.cursor);
            tab.cursor.page != old_page
        };
        self.apply();
        if self.settings.borrow().auto_read_on_next {
            self.start_speaking_with_header(page_changed);
        }
    }

    fn cycle_tab(&self, dir: i32) {
        let target = {
            let tabs = self.tabs.borrow();
            let n = tabs.tabs.len();
            if n <= 1 {
                return;
            }
            let cur = tabs.active as i32;
            ((cur + dir).rem_euclid(n as i32)) as usize
        };
        self.switch_tab(target);
        // Reveal the strip briefly so the user can see which tab they're on
        // — matches the chevron-button affordance.
        self.flash_tab_strip();
    }

    fn switch_tab(&self, idx: usize) {
        self.stop_speaking();
        let last_tab_id = {
            let mut tabs = self.tabs.borrow_mut();
            tabs.set_active(idx);
            let id = tabs.active_tab().map(|t| t.id.clone());
            if let Some(win) = self.win.upgrade() {
                win.set_active_tab(tabs.active as i32);
            }
            id
        };
        self.apply();
        self.refresh_watcher();
        if let Some(id) = last_tab_id {
            let mut s = self.settings.borrow_mut();
            s.last_tab = Some(id);
            let _ = s.save(&settings_path());
        }
    }

    fn set_aircraft(&self, aircraft: String) {
        self.stop_speaking();
        {
            let mut tabs = self.tabs.borrow_mut();
            tabs.set_aircraft(aircraft.clone());
            if let Some(win) = self.win.upgrade() {
                win.set_current_aircraft(SharedString::from(aircraft.as_str()));
            }
        }
        self.apply();
        self.refresh_watcher();
        self.push_stt_initial_prompt(&aircraft);
        {
            let mut s = self.settings.borrow_mut();
            s.current_aircraft = Some(aircraft);
            let _ = s.save(&settings_path());
        }
    }

    /// Compose the Whisper initial-prompt string for `aircraft` from
    /// `stt_config` and ship it to the STT worker thread. Cheap to call
    /// repeatedly — the worker just swaps a Mutex<String> contents.
    /// Issue #15.
    fn push_stt_initial_prompt(&self, aircraft: &str) {
        let Some(tx) = self.stt_tx.as_ref() else { return };
        let prompt = self.stt_config.build_initial_prompt(aircraft);
        if let Err(e) = tx.send(stt::SttCommand::SetInitialPrompt(prompt)) {
            eprintln!("[stt] could not push initial_prompt: {e:?}");
        }
    }

    fn next(&self) { self.nav(|p, c| step_cursor(p, c, 1)); }
    fn prev(&self) { self.nav(|p, c| step_cursor(p, c, -1)); }
    fn page_next(&self) { self.nav(|p, c| jump_page(p, c, 1)); }
    fn page_prev(&self) { self.nav(|p, c| jump_page(p, c, -1)); }
    fn next_heading(&self) {
        // Section jumps arm rather than auto-read (issue #17): position
        // the cursor on the heading, then arm_after_section_jump moves
        // it forward to the first navigable item and reads only the
        // header. Linear Next continues to flow as before. The pre-jump
        // snapshot lets Previous-while-armed return us here.
        self.save_pre_jump_cursor();
        self.nav_silent(|p, c| step_to_heading(p, c, 1));
        self.arm_after_section_jump();
    }
    fn prev_heading(&self) {
        self.save_pre_jump_cursor();
        self.nav_silent(|p, c| step_to_heading(p, c, -1));
        self.arm_after_section_jump();
    }

    /// Jump to an absolute page index. `n` is 1-based as spoken by the user
    /// ("page three" → 3); we clamp to the available range so out-of-bounds
    /// utterances land on the nearest valid page rather than refusing.
    fn goto_page(&self, n: u32) {
        self.nav(|pages, _cur| {
            if pages.is_empty() {
                return Cursor { page: 0, item: 0 };
            }
            let target = (n as usize).saturating_sub(1).min(pages.len() - 1);
            Cursor {
                page: target,
                item: tabs::first_navigable(&pages[target].manifest.items),
            }
        });
    }

    /// Jump to an exact (tab, page, item) coordinate produced by a query
    /// resolver, then arm — section jumps don't auto-read in issue #17.
    /// Switches tab first if needed.
    fn goto_target_armed(&self, target: tabs::NavTarget) {
        // Capture pre-jump position BEFORE the tab switch so Previous-
        // while-armed can return the user to the right place if the
        // jump crossed tabs.
        self.save_pre_jump_cursor();
        let active = self.tabs.borrow().active;
        if target.tab_idx != active {
            self.switch_tab(target.tab_idx);
        }
        self.nav_silent(|pages, _cur| {
            if pages.is_empty() {
                return Cursor { page: 0, item: 0 };
            }
            let page = target.page_idx.min(pages.len() - 1);
            let items = &pages[page].manifest.items;
            let item = if items.is_empty() {
                0
            } else {
                target.item_idx.min(items.len() - 1)
            };
            Cursor { page, item }
        });
        self.arm_after_section_jump();
    }

    /// Route a structured query to the right handler. Phase 2 only the
    /// NavigateToPage arm is live; later phases (section / tab / list /
    /// pick) plug into the same dispatcher.
    fn handle_query(&self, q: voice_router::QueryIntent) {
        use voice_router::QueryIntent;
        // Most queries leave the armed state behind — they're explicit
        // jumps to somewhere unrelated. NavigateToSection is the
        // exception and re-arms at the end of its own handler.
        self.set_armed_state(false);
        match q {
            QueryIntent::NavigateToPage(n) => self.goto_page(n),
            QueryIntent::NavigateToSection(target) => {
                // Apply phonetic alias rewrites ("mark" → "mk", "agem" →
                // "agm") so the fuzzy matcher sees canonical terminology.
                let rewritten = self.aliases.borrow().rewrite(&target);
                if rewritten != target {
                    eprintln!("[voice] section alias: \"{target}\" → \"{rewritten}\"");
                }
                let nm = self.tabs.borrow().resolve_section_query(&rewritten);
                match nm {
                    Some(nm) => {
                        eprintln!(
                            "[voice] section \"{}\" → \"{}\" (score {:.2}, {} alternates)",
                            rewritten,
                            nm.label,
                            nm.score,
                            nm.alternates.len()
                        );
                        for alt in nm.alternates.iter().take(2) {
                            eprintln!("[voice]   alt: \"{}\" (score {:.2})", alt.label, alt.score);
                        }
                        self.goto_target_armed(nm.target);
                    }
                    None => {
                        eprintln!("[voice] section query \"{rewritten}\" — no match");
                    }
                }
            }
            QueryIntent::NavigateToTab(target) => {
                let rewritten = self.aliases.borrow().rewrite(&target);
                if rewritten != target {
                    eprintln!("[voice] tab alias: \"{target}\" → \"{rewritten}\"");
                }
                let nm = self.tabs.borrow().resolve_tab_query(&rewritten);
                match nm {
                    Some(nm) => {
                        eprintln!(
                            "[voice] tab \"{}\" → \"{}\" (score {:.2}, {} alternates)",
                            rewritten, nm.label, nm.score, nm.alternates.len()
                        );
                        for alt in nm.alternates.iter().take(2) {
                            eprintln!("[voice]   alt: \"{}\" (score {:.2})", alt.label, alt.score);
                        }
                        self.switch_tab(nm.target.tab_idx);
                    }
                    None => {
                        eprintln!("[voice] tab query \"{rewritten}\" — no match");
                    }
                }
            }
            QueryIntent::ListSections => self.speak_section_list(),
            QueryIntent::PickResult(_) => {
                eprintln!("[voice] pick result — no results panel open, ignoring");
            }
        }
    }

    /// Read button: toggle. Stop if currently playing; otherwise start.
    fn read(&self) {
        if self.is_playing() {
            self.stop_speaking();
        } else {
            self.start_speaking();
        }
    }

    /// Sync the filesystem watcher with the current settings + active tab.
    /// Off → no watcher. On → watch the active tab's source path. Called on
    /// startup, tab switch, aircraft change, and toggle flip.
    fn refresh_watcher(&self) {
        let want_path: Option<PathBuf> = if self.settings.borrow().hot_reload {
            self.tabs
                .borrow()
                .active_tab()
                .and_then(|t| t.source_path.clone())
        } else {
            None
        };
        if let Err(e) = self
            .watcher
            .borrow_mut()
            .watch(want_path, self.watcher_tx.clone())
        {
            eprintln!("[watch] setup failed: {e:?}");
        }
    }

    /// Reload the active tab from disk + refresh the UI. Cursor is preserved
    /// when the same item still exists in the new manifest.
    fn reload_active_tab(&self) {
        let had_source = self.tabs.borrow_mut().reload_active();
        if had_source {
            self.apply();
        }
    }

    fn reload_pronunciation(&self) {
        let fresh = PronunciationConfig::load_or_default(&PathBuf::from("pronunciation.toml"));
        *self.pronunciation.borrow_mut() = fresh;
        // F5 reloads all the text-config files so the user can iterate on
        // both TTS pronunciation and voice-query aliases in one keystroke.
        let fresh_aliases = QueryAliases::load_or_default(&PathBuf::from("query_aliases.toml"));
        *self.aliases.borrow_mut() = fresh_aliases;
    }

    /// Move the cursor to the first navigable item right after the nearest
    /// preceding heading on the current page, then start reading with header
    /// context so the section name gets announced first. If no heading
    /// precedes the cursor, restarts the page from item 0.
    fn restart_section(&self) {
        // Explicit "from the top" — user wants to hear the section, not be
        // armed and waiting. Disarm before reading.
        self.set_armed_state(false);
        self.stop_speaking();
        {
            let mut tabs = self.tabs.borrow_mut();
            let Some(tab) = tabs.active_tab_mut() else { return };
            if tab.pages.is_empty() { return; }
            let page_idx = tab.cursor.page.min(tab.pages.len() - 1);
            let page = &tab.pages[page_idx];
            if page.manifest.items.is_empty() { return; }
            let item_idx = tab.cursor.item.min(page.manifest.items.len() - 1);

            let heading_idx = page.manifest.items[..=item_idx]
                .iter()
                .rposition(is_heading);

            let new_item = match heading_idx {
                Some(h) => page
                    .manifest
                    .items
                    .iter()
                    .enumerate()
                    .skip(h + 1)
                    .find(|(_, i)| i.navigable)
                    .map(|(idx, _)| idx)
                    .unwrap_or(h),
                None => first_navigable(&page.manifest.items),
            };
            tab.cursor = Cursor { page: page_idx, item: new_item };
        }
        self.apply();
        // include_page_header=true so the section name is re-announced.
        self.start_speaking_with_header(true);
    }

    /// Snapshot (active tab, cursor) so a subsequent `Previous` while
    /// armed can return the user to where they were before the jump.
    /// Called by the three section-jump entry points (NextHeading,
    /// PrevHeading, NavigateToSection) right before they move the cursor.
    fn save_pre_jump_cursor(&self) {
        let tabs = self.tabs.borrow();
        let active = tabs.active;
        let Some(tab) = tabs.active_tab() else { return };
        *self.pre_jump_cursor.borrow_mut() = Some((active, tab.cursor));
    }

    /// Restore the cursor (and tab) saved by `save_pre_jump_cursor`,
    /// consuming the snapshot. Returns true if a snapshot was restored.
    /// Used by `Previous` while armed (issue #17 follow-up).
    fn return_to_pre_jump(&self) -> bool {
        let Some((tab_idx, cursor)) = self.pre_jump_cursor.borrow_mut().take() else {
            return false;
        };
        self.stop_speaking();
        let active = self.tabs.borrow().active;
        if active != tab_idx {
            self.switch_tab(tab_idx);
        }
        {
            let mut tabs = self.tabs.borrow_mut();
            let Some(tab) = tabs.active_tab_mut() else { return false };
            // Clamp defensively — the page/item layout may have changed
            // under us via hot-reload while the user was armed.
            let page = cursor.page.min(tab.pages.len().saturating_sub(1));
            let item = if tab.pages.is_empty() {
                0
            } else {
                cursor.item.min(tab.pages[page].manifest.items.len().saturating_sub(1))
            };
            tab.cursor = Cursor { page, item };
        }
        self.apply();
        true
    }

    /// Mirror the armed-state flag into Slint and push/clear the
    /// sticky pill message. Every call site that touches
    /// `self.armed` goes through here so the in-memory flag, the
    /// highlight rect's amber pulse, and the pill content can't
    /// drift apart. Issue #17B; pill plumbing reworked in #17D.
    fn set_armed_state(&self, armed: bool) {
        self.armed.set(armed);
        if let Some(win) = self.win.upgrade() {
            win.set_armed(armed);
        }
        *self.pill_sticky.borrow_mut() = if armed {
            Some(PillMessage::armed_cue())
        } else {
            None
        };
        self.apply_pill();
    }

    /// Position the cursor without speaking. Sibling of `nav()` — used by
    /// section-jump paths that hand off to `arm_after_section_jump`
    /// rather than auto-reading. Issue #17.
    fn nav_silent(&self, step: impl FnOnce(&[LoadedPage], Cursor) -> Cursor) {
        self.stop_speaking();
        {
            let mut tabs = self.tabs.borrow_mut();
            let Some(tab) = tabs.active_tab_mut() else { return };
            tab.cursor = step(&tab.pages, tab.cursor);
        }
        self.apply();
    }

    /// Enter the armed state after a section jump (issue #17): move the
    /// cursor to the first navigable item of the section, speak only the
    /// section header, and wait. The first step is highlighted but not
    /// spoken; the user must say start / go / ok (or press Next) to
    /// advance.
    ///
    /// Also serves the "repeat while armed" path — calling this when
    /// already armed re-reads the header and keeps the same cursor
    /// (idempotent on cursor placement).
    fn arm_after_section_jump(&self) {
        self.stop_speaking();
        let heading_text = {
            let mut tabs = self.tabs.borrow_mut();
            let Some(tab) = tabs.active_tab_mut() else { return };
            if tab.pages.is_empty() {
                return;
            }
            let page_idx = tab.cursor.page.min(tab.pages.len() - 1);
            let page = &tab.pages[page_idx];
            if page.manifest.items.is_empty() {
                return;
            }
            let (first_nav, heading_idx) =
                place_armed_cursor(&page.manifest.items, tab.cursor.item);
            tab.cursor = Cursor { page: page_idx, item: first_nav };

            let pron = self.pronunciation.borrow();
            match heading_idx {
                Some(h) => {
                    let it = &page.manifest.items[h];
                    spoken_for(&it.text, it.spoken.as_deref(), &pron)
                }
                None => spoken_for(&page.manifest.title, None, &pron),
            }
        };

        self.apply();
        self.set_armed_state(true);

        if heading_text.is_empty() {
            return;
        }
        eprintln!("[tts] arm: {heading_text}");
        if let Some(engine) = self.tts.borrow_mut().as_mut() {
            if let Err(e) = engine.speak(&heading_text, true) {
                eprintln!("TTS speak failed: {e:?}");
            }
        }
        // Deliberately no set_playing(true) and no schedule_post_speech —
        // the armed header is one-shot, not part of the play cadence.
        // Setting "playing" would leave the on-screen Play button stuck
        // in "stop" mode forever (no post_speech_tick clears it back).
        // The user controls when reading resumes via start/go/ok/Next.
    }

    /// Speak the nearest preceding heading so the user can hear which
    /// section they're currently in. Falls back to the page title if no
    /// heading precedes the current item.
    /// Read the active tab's distinct section headers aloud, in document
    /// order. Triggered by voice "what sections are in this tab". Skips
    /// duplicates (same header text on multiple pages) so the list stays
    /// short. Applies pronunciation overrides through the standard
    /// spoken_for path.
    fn speak_section_list(&self) {
        let (to_say, est) = {
            let tabs = self.tabs.borrow();
            let Some(tab) = tabs.active_tab() else { return };
            if tab.pages.is_empty() {
                return;
            }
            let pron = self.pronunciation.borrow();
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            let mut parts: Vec<String> = Vec::new();
            for page in &tab.pages {
                for item in &page.manifest.items {
                    if item.kind != "section-header" {
                        continue;
                    }
                    let key = item.text.to_lowercase();
                    if !seen.insert(key) {
                        continue;
                    }
                    parts.push(spoken_for(&item.text, item.spoken.as_deref(), &pron));
                }
            }
            if parts.is_empty() {
                return;
            }
            // Semicolons buy us a natural pause from most TTS engines.
            let text = format!("Sections: {}.", parts.join("; "));
            let est = estimate_speech_ms(&text);
            (text, est)
        };
        if to_say.is_empty() {
            return;
        }
        self.stop_speaking();
        eprintln!("[tts] section list: {to_say}");
        if let Some(engine) = self.tts.borrow_mut().as_mut() {
            if let Err(e) = engine.speak(&to_say, true) {
                eprintln!("TTS speak failed: {e:?}");
            }
        }
        self.set_playing(true);
        self.schedule_post_speech(est);
    }

    fn speak_section(&self) {
        let (to_say, est) = {
            let tabs = self.tabs.borrow();
            let Some(tab) = tabs.active_tab() else { return };
            if tab.pages.is_empty() { return; }
            let cur = tab.cursor;
            let page_idx = cur.page.min(tab.pages.len() - 1);
            let page = &tab.pages[page_idx];
            if page.manifest.items.is_empty() { return; }
            let item_idx = cur.item.min(page.manifest.items.len() - 1);

            let pron = self.pronunciation.borrow();
            let heading = page.manifest.items[..=item_idx]
                .iter()
                .rev()
                .find(|i| is_heading(i));
            let text = match heading {
                Some(h) => spoken_for(&h.text, h.spoken.as_deref(), &pron),
                None => spoken_for(&page.manifest.title, None, &pron),
            };
            let est = estimate_speech_ms(&text);
            (text, est)
        };
        if to_say.is_empty() { return; }

        self.stop_speaking();
        eprintln!("[tts] section: {to_say}");
        if let Some(engine) = self.tts.borrow_mut().as_mut() {
            if let Err(e) = engine.speak(&to_say, true) {
                eprintln!("TTS speak failed: {e:?}");
            }
        }
        self.set_playing(true);
        self.schedule_post_speech(est);
    }

    /// Push the current bindings table into the Slint model. Call after any
    /// edit (capture commit, clear, or capture-start so the row highlights).
    fn refresh_bindings_ui(&self) {
        let Some(win) = self.win.upgrade() else { return };
        let s = self.settings.borrow();
        let capturing = *self.capture.borrow();
        let rows: Vec<BindingRow> = Action::all()
            .iter()
            .enumerate()
            .map(|(i, &action)| {
                let triggers = s.bindings.triggers_for(action);
                let trigger_text = match triggers.as_slice() {
                    [] => "(unbound)".to_string(),
                    [t] => t.display(),
                    many => format!("{} (+{})", many[0].display(), many.len() - 1),
                };
                BindingRow {
                    action_id: i as i32,
                    label: SharedString::from(action.label()),
                    trigger_text: SharedString::from(trigger_text),
                    capturing: capturing == Some(action),
                }
            })
            .collect();
        win.set_bindings(slint::ModelRc::new(VecModel::from(rows)));
    }

    /// Begin capturing a key for `action`. The next keypress (other than Esc)
    /// is recorded as the action's sole trigger.
    fn start_binding_capture(&self, action: Action) {
        *self.capture.borrow_mut() = Some(action);
        self.refresh_bindings_ui();
    }

    fn cancel_binding_capture(&self) {
        *self.capture.borrow_mut() = None;
        self.refresh_bindings_ui();
    }

    fn clear_binding(&self, action: Action) {
        {
            let mut s = self.settings.borrow_mut();
            s.bindings.set_triggers(action, vec![]);
            let _ = s.save(&settings_path());
        }
        self.refresh_bindings_ui();
    }

    /// Commit a captured trigger to the bindings table. Replaces any prior
    /// triggers for that action, and removes any other action that previously
    /// owned this trigger (last-writer-wins per SPEC §7.5).
    fn commit_capture(&self, action: Action, trigger: input::Trigger) {
        {
            let mut s = self.settings.borrow_mut();
            s.bindings.unbind_trigger(&trigger);
            s.bindings.set_triggers(action, vec![trigger]);
            let _ = s.save(&settings_path());
        }
        *self.capture.borrow_mut() = None;
        self.refresh_bindings_ui();
    }

    /// Single entry point for keyboard + gamepad input. Press edges go
    /// through capture-or-dispatch; release edges only act for PushToTalk.
    fn handle_event(&self, event: InputEvent) -> bool {
        match event {
            InputEvent::Press(t) => self.handle_press(t),
            InputEvent::Release(t) => self.handle_release(t),
            InputEvent::DevicesChanged => {
                self.refresh_bindings_ui();
                false
            }
        }
    }

    fn handle_press(&self, trigger: input::Trigger) -> bool {
        // Copy the captured action out so the Ref drops before we re-borrow
        // capture/settings inside commit_capture or cancel_binding_capture
        // (Rust extends scrutinee temporaries across the entire `if let` body).
        let capturing = *self.capture.borrow();
        if let Some(action) = capturing {
            // Esc on the keyboard cancels capture instead of binding to Esc.
            if matches!(&trigger, input::Trigger::Keyboard { key, .. } if key == "Escape") {
                self.cancel_binding_capture();
            } else {
                self.commit_capture(action, trigger);
            }
            return true;
        }
        let action = self.settings.borrow().bindings.action_for(&trigger);
        match action {
            Some(action) => {
                // In probe/test mode, flash the row instead of firing so the
                // user can verify what a button is bound to without leaving
                // the settings panel.
                if self
                    .win
                    .upgrade()
                    .map(|w| w.get_binding_test_mode())
                    .unwrap_or(false)
                {
                    eprintln!("[probe] {} → {}", trigger.display(), action.label());
                    self.flash_binding(action);
                } else {
                    self.dispatch(action);
                }
                true
            }
            None => {
                if self
                    .win
                    .upgrade()
                    .map(|w| w.get_binding_test_mode())
                    .unwrap_or(false)
                {
                    eprintln!("[probe] {} → (unbound)", trigger.display());
                }
                false
            }
        }
    }

    /// Only PushToTalk acts on release. The release handler stops audio
    /// capture and (M4.3) hands the buffer to STT. Right now it logs the
    /// duration and sample count so we can verify capture is alive.
    fn handle_release(&self, trigger: input::Trigger) -> bool {
        if self.capture.borrow().is_some() {
            return false; // ignore releases during binding capture
        }
        let action = self.settings.borrow().bindings.action_for(&trigger);
        if action != Some(Action::PushToTalk) {
            return false;
        }
        // Ignore PTT release if hot mic is latched — that mode is press-to-
        // toggle and should not be cut short by a transient PTT release.
        if self.mic_locked.get() {
            return false;
        }
        self.submit_captured_audio();
        true
    }

    /// Send a chunk of captured PCM to STT (or log if no STT loaded).
    /// Shared by PTT release, HotMicToggle stop, and the VAD chunker.
    fn submit_pcm(&self, pcm: Vec<f32>) {
        if pcm.is_empty() {
            return;
        }
        let secs = pcm.len() as f32 / audio::TARGET_RATE as f32;
        eprintln!("[mic] submit {} samples ({:.2} s)", pcm.len(), secs);
        match self.stt_tx.as_ref() {
            Some(tx) => {
                self.show_transcript(format!("Transcribing {:.1}s...", secs));
                if let Err(e) = tx.send(stt::SttCommand::Pcm(pcm)) {
                    eprintln!("[stt] submit failed: {e:?}");
                }
            }
            None => {
                self.show_transcript(format!("Captured {:.1}s — no STT model loaded", secs));
            }
        }
    }

    /// Begin polling the audio buffer for complete utterances while hot mic
    /// is on. The 200 ms cadence is a balance between detection latency and
    /// CPU cost; the actual chunking decision happens inside the audio
    /// module's VAD.
    fn start_hotmic_polling(&self) {
        let me = self.clone();
        self.hotmic_timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(200),
            move || {
                if !me.mic_locked.get() {
                    return;
                }
                // While TTS is playing, optionally drop any audio that came
                // in — without that, speaker bleed from the synth would land
                // in the buffer and get transcribed when we resume polling.
                // Headphone users can disable this so speech captured during
                // TTS still gets processed when the readout ends.
                if me.is_playing() && me.settings.borrow().mute_mic_during_speech {
                    if let Some(audio) = me.audio.borrow().as_ref() {
                        audio.discard_pending();
                    }
                    return;
                }
                let chunk = me
                    .audio
                    .borrow()
                    .as_ref()
                    .and_then(|a| a.try_take_utterance().ok().flatten());
                if let Some(pcm) = chunk {
                    me.submit_pcm(pcm);
                }
            },
        );
    }

    /// Stop capture and ship the (whole) recorded PCM to STT. Shared by
    /// PTT release and the HotMicToggle stop path — the latter relies on
    /// this to flush whatever the VAD chunker hasn't already drained.
    fn submit_captured_audio(&self) {
        let pcm = match self.audio.borrow().as_ref() {
            Some(audio) => match audio.stop() {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("[mic] stop error: {e:?}");
                    Vec::new()
                }
            },
            None => Vec::new(),
        };
        self.submit_pcm(pcm);
        self.set_mic_hot(false);
    }

    fn set_mic_hot(&self, hot: bool) {
        if let Some(win) = self.win.upgrade() {
            win.set_mic_hot(hot);
        }
        // Re-render the pill so the lowest-priority listening cue
        // appears/disappears in lockstep with PTT.
        self.apply_pill();
    }

    /// Pop a transcript into the pill (no icon) and schedule its fade.
    /// 0 s pill duration disables it entirely.
    /// Plain status toast — no icon, no border. Used for messages
    /// like "(no speech)", "Click-through ON".
    fn show_transcript(&self, text: String) {
        self.push_transient_pill(PillMessage::status(text));
    }

    /// Voice-command-recognised toast: lucide check on the left.
    fn show_transcript_match(&self, text: String) {
        self.push_transient_pill(PillMessage::transcript_match(text));
    }

    /// Voice-command-unmatched toast: red lucide x on the left.
    fn show_transcript_unmatched(&self, text: String) {
        self.push_transient_pill(PillMessage::transcript_unmatched(text));
    }

    /// Surface a critical-error message in the pill. Beats both
    /// sticky and transient sources so silent-failure scenarios stop
    /// being silent. Held for 15 s then drops back to whatever
    /// sticky/transient source is active (usually nothing). Always
    /// also logged via `eprintln!` so the message survives in the
    /// console after the pill fades.
    fn show_critical(&self, text: String) {
        if text.is_empty() {
            return;
        }
        eprintln!("[critical] {text}");
        *self.pill_critical.borrow_mut() = Some(PillMessage::critical(text));
        self.apply_pill();
        let me = self.clone();
        self.critical_timer.start(
            slint::TimerMode::SingleShot,
            Duration::from_secs(15),
            move || {
                *me.pill_critical.borrow_mut() = None;
                me.apply_pill();
            },
        );
    }

    /// Schedule a transient pill message. Beaten by any sticky
    /// (armed-state) message currently active — the sticky message
    /// stays on-screen, the transient is suppressed entirely rather
    /// than queued. That keeps the priority semantics simple: only
    /// the most recent transient is ever remembered, only the active
    /// sticky is ever shown.
    ///
    /// Empty text is a no-op so callers can `show_transcript(t)`
    /// without an empty-string guard.
    fn push_transient_pill(&self, msg: PillMessage) {
        let dur = self.settings.borrow().transcript_pill_seconds;
        if dur <= 0.0 || msg.text.is_empty() {
            return;
        }
        *self.pill_transient.borrow_mut() = Some(msg);
        self.apply_pill();
        let me = self.clone();
        self.transcript_timer.start(
            slint::TimerMode::SingleShot,
            Duration::from_secs_f32(dur),
            move || {
                *me.pill_transient.borrow_mut() = None;
                me.apply_pill();
            },
        );
    }

    /// Compose the effective pill message from the four sources and
    /// push it into Slint. Priority: critical (silent-failure
    /// banners) > sticky (armed-state cue) > transient (transcripts,
    /// status toasts) > listening (PTT-held cue, only when nothing
    /// else has content). Called every time any source changes or
    /// expires, and from `set_mic_hot` so the listening cue tracks
    /// the PTT edge.
    fn apply_pill(&self) {
        let Some(win) = self.win.upgrade() else { return };
        let effective = self
            .pill_critical
            .borrow()
            .clone()
            .or_else(|| self.pill_sticky.borrow().clone())
            .or_else(|| self.pill_transient.borrow().clone())
            .or_else(|| {
                if win.get_mic_hot() {
                    Some(PillMessage::listening())
                } else {
                    None
                }
            });
        match effective {
            Some(msg) => {
                win.set_pill_text(SharedString::from(msg.text.as_str()));
                win.set_pill_icon_d(SharedString::from(msg.icon_d));
                win.set_pill_icon_tint(slint::Brush::SolidColor(msg.icon_tint));
                win.set_pill_border_color(slint::Brush::SolidColor(msg.border_color));
                win.set_pill_pulse(msg.pulse);
                win.set_pill_visible(true);
            }
            None => {
                win.set_pill_visible(false);
            }
        }
    }

    /// Briefly pin the left tab strip so a button-driven tab change is
    /// visible. Re-arms the unpin timer on every call so consecutive cycles
    /// extend the visible window rather than firing back-to-back.
    fn flash_tab_strip(&self) {
        if let Some(win) = self.win.upgrade() {
            win.set_strip_pinned(true);
        }
        let me = self.clone();
        self.strip_pin_timer.start(
            slint::TimerMode::SingleShot,
            Duration::from_millis(1200),
            move || {
                if let Some(win) = me.win.upgrade() {
                    win.set_strip_pinned(false);
                }
            },
        );
    }

    /// Pop the binding row corresponding to `action` into the yellow flash
    /// state, then schedule it to fade back. The Slint `animate background`
    /// on the row does the visual blend.
    fn flash_binding(&self, action: Action) {
        let id = Action::all()
            .iter()
            .position(|&a| a == action)
            .map(|i| i as i32)
            .unwrap_or(-1);
        if let Some(win) = self.win.upgrade() {
            win.set_binding_flash_id(id);
        }
        let me = self.clone();
        self.binding_flash_timer.start(
            slint::TimerMode::SingleShot,
            Duration::from_millis(700),
            move || {
                if let Some(win) = me.win.upgrade() {
                    win.set_binding_flash_id(-1);
                }
            },
        );
    }

    /// Tear down the current capture stream and open a new one on `name`
    /// (None = system default). Persists the choice to settings.toml.
    fn set_audio_input(&self, name: Option<String>) {
        // Drop the old stream before opening the new one so cpal can release
        // the device handle on platforms that exclusive-lock.
        *self.audio.borrow_mut() = None;
        let opened = match name.as_deref() {
            Some(n) => audio::open_named(n),
            None => audio::open_default(),
        };
        match opened {
            Ok(cap) => {
                eprintln!("[audio] switched to \"{}\"", cap.input_name());
                *self.audio.borrow_mut() = Some(cap);
            }
            Err(e) => eprintln!("[audio] switch failed: {e:?}"),
        }
        {
            let mut s = self.settings.borrow_mut();
            s.audio_input = name;
            let _ = s.save(&settings_path());
        }
        self.refresh_audio_ui();
    }

    /// Push the current TTS engine + Piper voice list into the Slint model.
    fn refresh_tts_ui(&self) {
        let Some(win) = self.win.upgrade() else { return };
        let s = self.settings.borrow();
        win.set_tts_engine_selected(SharedString::from(s.tts_engine.as_str()));

        let voices = list_piper_voices();
        let names: Vec<SharedString> = voices
            .iter()
            .map(|p| SharedString::from(
                p.file_name().and_then(|n| n.to_str()).unwrap_or(""),
            ))
            .collect();
        win.set_piper_voices(slint::ModelRc::new(VecModel::from(names)));

        let selected = s
            .tts_piper_voice
            .as_ref()
            .map(|full| {
                std::path::Path::new(full)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string()
            })
            .unwrap_or_default();
        win.set_piper_voice_selected(SharedString::from(selected.as_str()));
    }

    /// Rebuild the TTS engine after the user changes the selected engine or
    /// Piper voice. Stops in-flight speech, drops the old engine, then tries
    /// to build the new one. Falls back to WinRT on failure.
    fn rebuild_tts(&self) {
        self.stop_speaking();
        *self.tts.borrow_mut() = None;
        let (r, v) = {
            let s = self.settings.borrow();
            (s.tts_rate, s.tts_volume)
        };
        let s = self.settings.borrow();
        match init_tts(&s) {
            Ok((mut engine, warn)) => {
                eprintln!("[tts] engine ready: {}", engine.name());
                engine.set_rate(r);
                engine.set_volume(v);
                drop(s);
                *self.tts.borrow_mut() = Some(engine);
                if let Some(msg) = warn {
                    self.show_critical(msg);
                }
            }
            Err(e) => {
                eprintln!("[tts] rebuild failed: {e:?}");
                drop(s);
                self.show_critical(format!("TTS engine init failed: {e}"));
            }
        }
    }

    fn set_tts_engine(&self, name: String) {
        {
            let mut s = self.settings.borrow_mut();
            if s.tts_engine == name {
                return;
            }
            s.tts_engine = name;
            let _ = s.save(&settings_path());
        }
        self.refresh_tts_ui();
        self.rebuild_tts();
    }

    fn set_piper_voice(&self, voice_basename: String) {
        let full = PathBuf::from("models/piper/voices").join(&voice_basename);
        let full_str = full.to_string_lossy().to_string();
        {
            let mut s = self.settings.borrow_mut();
            if s.tts_piper_voice.as_deref() == Some(full_str.as_str()) {
                return;
            }
            s.tts_piper_voice = Some(full_str);
            // Picking a voice also implies switching the engine.
            if s.tts_engine != "piper" {
                s.tts_engine = "piper".into();
            }
            let _ = s.save(&settings_path());
        }
        self.refresh_tts_ui();
        self.rebuild_tts();
    }

    fn test_tts(&self) {
        // Build a pronounceable phrase — using engine.name() verbatim makes
        // WinRT spell out "winrt" badly.
        let label: &str = match self.tts.borrow().as_ref().map(|e| e.name()) {
            Some("piper") => "Piper voice ready.",
            Some("winrt") => "Windows speech engine ready.",
            Some(_) => "Speech engine ready.",
            None => "No speech engine loaded.",
        };
        let phrase = label.to_string();
        eprintln!("[tts] test: {phrase}");
        if let Some(engine) = self.tts.borrow_mut().as_mut() {
            if let Err(e) = engine.speak(&phrase, true) {
                eprintln!("[tts] test speak failed: {e:?}");
            }
        }
    }

    /// Push the current device list + selection into the Slint model.
    fn refresh_audio_ui(&self) {
        let Some(win) = self.win.upgrade() else { return };
        let devices = audio::enumerate_inputs();
        let selected = self
            .settings
            .borrow()
            .audio_input
            .clone()
            .or_else(|| self.audio.borrow().as_ref().map(|a| a.input_name().to_string()))
            .unwrap_or_default();
        let model: Vec<SharedString> = devices
            .iter()
            .map(|n| SharedString::from(n.as_str()))
            .collect();
        win.set_audio_inputs(slint::ModelRc::new(VecModel::from(model)));
        win.set_audio_input_selected(SharedString::from(selected.as_str()));
    }

    fn toggle_settings(&self) {
        if let Some(win) = self.win.upgrade() {
            win.set_settings_open(!win.get_settings_open());
        }
    }

    fn toggle_voice_commands(&self) {
        if let Some(win) = self.win.upgrade() {
            win.set_voice_commands_open(!win.get_voice_commands_open());
        }
    }

    /// Apply click-through: flip Win32 WS_EX_TRANSPARENT, update the UI
    /// badge, persist, and toast the result. Called from the Action, the
    /// settings checkbox, and at startup when the persisted state is true.
    fn set_click_through(&self, enabled: bool) {
        if let Some(win) = self.win.upgrade() {
            win.set_click_through(enabled);
        }
        #[cfg(windows)]
        {
            overlay::set_click_through(enabled);
        }
        {
            let mut s = self.settings.borrow_mut();
            if s.click_through != enabled {
                s.click_through = enabled;
                let _ = s.save(&settings_path());
            }
        }
        self.show_transcript(
            if enabled { "Click-through ON".to_string() } else { "Click-through OFF".to_string() },
        );
    }

    /// Dispatch a resolved action. The Cancel action is context-sensitive:
    /// closes the settings panel if open, otherwise stops in-flight speech.
    fn dispatch(&self, action: Action) {
        // Armed-state interception (issue #17). Classification lives in
        // `controller::armed_decision` so the state-transition table
        // can be unit-tested without TTS/Slint side-effects. Decisions
        // either short-circuit dispatch outright or drop into the main
        // match below (DisarmThenRun / PassThrough).
        match controller::armed_decision(action, self.armed.get()) {
            controller::ArmedDecision::SpeakCurrent => {
                self.set_armed_state(false);
                self.start_speaking();
                return;
            }
            controller::ArmedDecision::RereadHeader => {
                self.arm_after_section_jump();
                return;
            }
            controller::ArmedDecision::SilentDisarm => {
                self.set_armed_state(false);
                self.stop_speaking();
                return;
            }
            controller::ArmedDecision::ReturnToPreJump => {
                self.set_armed_state(false);
                if !self.return_to_pre_jump() {
                    self.prev();
                }
                return;
            }
            controller::ArmedDecision::DisarmThenRun => {
                self.set_armed_state(false);
                // fall through into the main action match
            }
            controller::ArmedDecision::PassThrough => {
                // fall through unchanged
            }
        }
        match action {
            Action::Next => self.next(),
            Action::Previous => self.prev(),
            Action::TogglePlay => self.read(),
            Action::ReadCurrent => self.start_speaking(),
            Action::ReadSection => self.speak_section(),
            Action::RestartSection => self.restart_section(),
            Action::NextHeading => self.next_heading(),
            Action::PrevHeading => self.prev_heading(),
            Action::PageNext => self.page_next(),
            Action::PagePrev => self.page_prev(),
            Action::CycleTabPrev => self.cycle_tab(-1),
            Action::CycleTabNext => self.cycle_tab(1),
            Action::OpenSettings => self.toggle_settings(),
            Action::OpenVoiceCommands => self.toggle_voice_commands(),
            Action::ReloadPronunciation => self.reload_pronunciation(),
            Action::Cancel => {
                if let Some(win) = self.win.upgrade() {
                    if win.get_voice_commands_open() {
                        win.set_voice_commands_open(false);
                        return;
                    }
                    if win.get_settings_open() {
                        win.set_settings_open(false);
                        return;
                    }
                }
                self.stop_speaking();
            }
            Action::PushToTalk => {
                if self.mic_locked.get() {
                    eprintln!("[ptt] ignored — hot mic toggle is active");
                } else if let Some(audio) = self.audio.borrow().as_ref() {
                    audio.start();
                    eprintln!("[ptt] capturing… (release to submit)");
                    self.set_mic_hot(true);
                } else {
                    eprintln!("[ptt] no audio device — cannot capture");
                    // Surface the dead PTT in the pill so a press at least
                    // gets a visible response. The startup critical pill
                    // covered "no mic" once, but it has long since faded.
                    self.show_transcript("No microphone — voice off".to_string());
                }
            }
            Action::HotMicToggle => {
                if self.mic_locked.get() {
                    // Second press → stop polling, flush any tail audio.
                    self.mic_locked.set(false);
                    self.hotmic_timer.stop();
                    eprintln!("[hotmic] stopping");
                    self.submit_captured_audio();
                } else if let Some(audio) = self.audio.borrow().as_ref() {
                    // First press → start capture, latch, and begin polling
                    // the audio buffer for complete utterances.
                    audio.start();
                    self.mic_locked.set(true);
                    eprintln!("[hotmic] hot mic ON — speak commands, press again to stop");
                    self.set_mic_hot(true);
                    self.start_hotmic_polling();
                } else {
                    eprintln!("[hotmic] no audio device — cannot capture");
                    self.show_transcript("No microphone — voice off".to_string());
                }
            }
            Action::ToggleReadNotes => {
                let new_val = {
                    let mut s = self.settings.borrow_mut();
                    s.read_notes = !s.read_notes;
                    let _ = s.save(&settings_path());
                    s.read_notes
                };
                if let Some(win) = self.win.upgrade() {
                    win.set_read_notes(new_val);
                }
                eprintln!("[ui] read_notes = {new_val}");
                self.show_transcript(
                    if new_val { "More info ON".to_string() } else { "More info OFF".to_string() },
                );
            }
            Action::ToggleVisibility => {
                // Move the window offscreen instead of slint::Window::hide()
                // so the event loop, timers, and gamepad polling stay alive —
                // which means a HOTAS-bound ToggleVisibility press can also
                // bring it back.
                if let Some(win) = self.win.upgrade() {
                    if self.window_hidden.get() {
                        let pos = self
                            .saved_pos
                            .borrow()
                            .unwrap_or_else(|| slint::PhysicalPosition::new(100, 100));
                        win.window().set_position(pos);
                        self.window_hidden.set(false);
                        eprintln!("[ui] show at {:?}", (pos.x, pos.y));
                    } else {
                        let cur = win.window().position();
                        *self.saved_pos.borrow_mut() = Some(cur);
                        // Offscreen far enough that no display reaches it.
                        win.window().set_position(slint::PhysicalPosition::new(-30000, -30000));
                        self.window_hidden.set(true);
                        eprintln!("[ui] hide (offscreen) — press your bound button again to restore");
                    }
                }
            }
            Action::ToggleClickThrough => {
                let now = self
                    .win
                    .upgrade()
                    .map(|w| w.get_click_through())
                    .unwrap_or(false);
                self.set_click_through(!now);
            }
        }
    }
}

fn main() -> Result<()> {
    let app_config = AppConfig::load_or_default(&config_path());
    let settings = Rc::new(RefCell::new(Settings::load_or_default(&settings_path())));

    // Collected during init so silent failures (whisper missing, audio
    // open failure, piper fallback) can be surfaced in the pill once
    // the window exists. Latest message wins; the full log still has
    // every entry.
    let mut pending_critical: Vec<String> = Vec::new();

    // Resolve initial aircraft: settings override, else first config entry, else fallback.
    let aircraft = settings
        .borrow()
        .current_aircraft
        .clone()
        .or_else(|| app_config.aircraft.first().map(|a| a.id.clone()))
        .unwrap_or_else(|| "F-16C_50".to_string());
    eprintln!("[app] aircraft: {aircraft}");

    let mut registry = TabRegistry::new(&app_config, aircraft.clone());

    // Restore last-active tab if it still exists, else first tab.
    let last_tab = settings.borrow().last_tab.clone();
    match last_tab {
        Some(id) if registry.tabs.iter().any(|t| t.id == id) => registry.set_active_by_id(&id),
        _ => {
            if !registry.tabs.is_empty() {
                registry.set_active(0);
            }
        }
    }

    let tabs_rc = Rc::new(RefCell::new(registry));

    let pronunciation = Rc::new(RefCell::new(PronunciationConfig::load_or_default(
        &PathBuf::from("pronunciation.toml"),
    )));
    let aliases = Rc::new(RefCell::new(QueryAliases::load_or_default(
        &PathBuf::from("query_aliases.toml"),
    )));

    let tts: Rc<RefCell<Option<Box<dyn TtsEngine>>>> = Rc::new(RefCell::new(
        match init_tts(&settings.borrow()) {
            Ok((engine, warn)) => {
                if let Some(msg) = warn {
                    pending_critical.push(msg);
                }
                Some(engine)
            }
            Err(e) => {
                eprintln!("TTS init failed: {e:?}");
                pending_critical.push(format!("TTS engine init failed: {e}"));
                None
            }
        },
    ));
    // Apply the persisted rate / volume to the freshly built engine.
    {
        let s = settings.borrow();
        if let Some(engine) = tts.borrow_mut().as_mut() {
            engine.set_rate(s.tts_rate);
            engine.set_volume(s.tts_volume);
        }
    }

    let win = MainWindow::new()?;
    {
        let s = settings.borrow();
        win.set_auto_read(s.auto_read_on_next);
        win.set_auto_advance(s.auto_advance);
        win.set_advance_delay(s.advance_delay_sec);
        win.set_read_notes(s.read_notes);
        win.set_transcript_pill_seconds(s.transcript_pill_seconds);
        win.set_hot_reload(s.hot_reload);
        win.set_mute_mic_during_speech(s.mute_mic_during_speech);
        win.set_click_through(s.click_through);
        win.set_window_opacity(Settings::clamp_window_opacity(s.window_opacity));
        win.set_tts_rate(s.tts_rate);
        win.set_tts_volume(s.tts_volume);
        if let (Some(x), Some(y)) = (s.window_x, s.window_y) {
            win.window().set_position(slint::PhysicalPosition::new(x, y));
        }
    }

    // Push tab + aircraft model state to Slint.
    {
        let reg = tabs_rc.borrow();
        let tab_infos: Vec<TabInfo> = reg
            .tabs
            .iter()
            .map(|t| TabInfo {
                id: SharedString::from(t.id.as_str()),
                label: SharedString::from(t.label.as_str()),
                icon_d: SharedString::from(lucide_path(&t.icon)),
            })
            .collect();
        win.set_tabs(slint::ModelRc::new(VecModel::from(tab_infos)));
        win.set_active_tab(reg.active as i32);

        let aircraft_infos: Vec<AircraftInfo> = reg
            .aircraft_list
            .iter()
            .map(|(id, label)| AircraftInfo {
                id: SharedString::from(id.as_str()),
                label: SharedString::from(label.as_str()),
            })
            .collect();
        win.set_aircraft_list(slint::ModelRc::new(VecModel::from(aircraft_infos)));
        win.set_current_aircraft(SharedString::from(reg.aircraft.as_str()));
    }

    let audio = {
        let preferred = settings.borrow().audio_input.clone();
        let opened = match preferred.as_deref() {
            Some(name) => audio::open_named(name),
            None => audio::open_default(),
        };
        match opened {
            Ok(cap) => {
                eprintln!("[audio] capture ready on \"{}\"", cap.input_name());
                Some(cap)
            }
            Err(e) => {
                eprintln!("[audio] disabled: {e:?}");
                pending_critical.push(format!("Microphone unavailable — voice off ({e})"));
                None
            }
        }
    };

    // STT worker thread: model + inference live on a dedicated thread so a
    // 700 ms whisper run doesn't stall the UI. `stt_tx` is None when no model
    // is on disk (or the whisper-stt feature is off) so the rest of the app
    // keeps working — PTT still captures audio and the pill reports the
    // duration.
    let (stt_tx, stt_rx_main): (
        Option<std::sync::mpsc::Sender<stt::SttCommand>>,
        Option<std::sync::mpsc::Receiver<String>>,
    );
    #[cfg(feature = "whisper-stt")]
    {
        match stt::find_default_model() {
            Some(path) => {
                eprintln!("[stt] model: {}", path.display());
                let (req_tx, req_rx) = std::sync::mpsc::channel::<stt::SttCommand>();
                let (res_tx, res_rx) = std::sync::mpsc::channel::<String>();
                std::thread::Builder::new()
                    .name("whisper".into())
                    .spawn(move || {
                        use stt::{SttCommand, SttEngine};
                        let engine = match stt::WhisperStt::new(&path) {
                            Ok(e) => { eprintln!("[stt] loaded: {}", e.name()); e }
                            Err(e) => { eprintln!("[stt] init failed: {e:?}"); return; }
                        };
                        while let Ok(cmd) = req_rx.recv() {
                            match cmd {
                                SttCommand::Pcm(pcm) => {
                                    let start = std::time::Instant::now();
                                    match stt::SttEngine::transcribe(&engine, &pcm) {
                                        Ok(text) => {
                                            eprintln!("[stt] {:.0} ms: \"{}\"", start.elapsed().as_millis(), text);
                                            let _ = res_tx.send(text);
                                        }
                                        Err(e) => {
                                            eprintln!("[stt] transcribe failed: {e:?}");
                                            let _ = res_tx.send(format!("(STT error: {e})"));
                                        }
                                    }
                                }
                                SttCommand::SetInitialPrompt(prompt) => {
                                    engine.set_initial_prompt(prompt);
                                }
                            }
                        }
                    })?;
                stt_tx = Some(req_tx);
                stt_rx_main = Some(res_rx);
            }
            None => {
                eprintln!("[stt] no model found — disabled");
                pending_critical.push(
                    "Whisper model missing — voice commands off (see models/README.md)".to_string(),
                );
                stt_tx = None;
                stt_rx_main = None;
            }
        }
    }
    #[cfg(not(feature = "whisper-stt"))]
    {
        eprintln!("[stt] whisper-stt feature not enabled — build with --features whisper-stt after installing LLVM");
        stt_tx = None;
        stt_rx_main = None;
    }

    let state = AppState {
        tabs: tabs_rc.clone(),
        tts: tts.clone(),
        pronunciation: pronunciation.clone(),
        aliases: aliases.clone(),
        settings: settings.clone(),
        win: win.as_weak(),
        advance_timer: Rc::new(slint::Timer::default()),
        capture: Rc::new(RefCell::new(None)),
        audio: Rc::new(RefCell::new(audio)),
        transcript_timer: Rc::new(slint::Timer::default()),
        stt_tx: Rc::new(stt_tx),
        binding_flash_timer: Rc::new(slint::Timer::default()),
        window_hidden: Rc::new(std::cell::Cell::new(false)),
        saved_pos: Rc::new(RefCell::new(None)),
        strip_pin_timer: Rc::new(slint::Timer::default()),
        watcher: Rc::new(RefCell::new(watcher::Watcher::new())),
        watcher_tx: {
            let (tx, _rx) = std::sync::mpsc::channel::<PathBuf>();
            tx
        },
        mic_locked: Rc::new(std::cell::Cell::new(false)),
        hotmic_timer: Rc::new(slint::Timer::default()),
        armed: Rc::new(std::cell::Cell::new(false)),
        pre_jump_cursor: Rc::new(RefCell::new(None)),
        stt_config: Rc::new(app_config.stt.clone()),
        pill_sticky: Rc::new(RefCell::new(None)),
        pill_transient: Rc::new(RefCell::new(None)),
        pill_critical: Rc::new(RefCell::new(None)),
        critical_timer: Rc::new(slint::Timer::default()),
    };

    // Real watcher channel: tx goes into the watcher callback, rx is drained
    // on the UI thread to call reload_active_tab.
    let (watch_tx, watch_rx) = std::sync::mpsc::channel::<PathBuf>();
    // Replace the placeholder tx that was needed to satisfy struct init order.
    let mut state = state;
    state.watcher_tx = watch_tx;
    let state = state;
    state.apply();

    // Surface any startup silent-failures captured during init. Latest
    // wins by design (single critical slot); the full list is in the
    // log already. Done after `state.apply()` so the pill timer and
    // window are both ready to render.
    if let Some(msg) = pending_critical.pop() {
        state.show_critical(msg);
    }

    // UI buttons go through dispatch so the armed-state interception
    // (issue #17) lives in one place — clicking on-screen Next while
    // armed must behave identically to saying "go" or pressing the
    // bound key.
    {
        let s = state.clone();
        win.on_next_clicked(move || s.dispatch(Action::Next));
    }
    {
        let s = state.clone();
        win.on_prev_clicked(move || s.dispatch(Action::Previous));
    }
    {
        let s = state.clone();
        win.on_page_next_clicked(move || s.dispatch(Action::PageNext));
    }
    {
        let s = state.clone();
        win.on_page_prev_clicked(move || s.dispatch(Action::PagePrev));
    }
    {
        let s = state.clone();
        win.on_next_heading_clicked(move || s.dispatch(Action::NextHeading));
    }
    {
        let s = state.clone();
        win.on_prev_heading_clicked(move || s.dispatch(Action::PrevHeading));
    }
    {
        let s = state.clone();
        win.on_read_clicked(move || s.dispatch(Action::TogglePlay));
    }
    {
        let s = state.clone();
        win.on_tab_clicked(move |idx| s.switch_tab(idx.max(0) as usize));
    }
    {
        let s = state.clone();
        win.on_tab_cycle_prev_clicked(move || s.cycle_tab(-1));
    }
    {
        let s = state.clone();
        win.on_tab_cycle_next_clicked(move || s.cycle_tab(1));
    }
    {
        let s = state.clone();
        win.on_aircraft_clicked(move |id| s.set_aircraft(id.to_string()));
    }

    // Persist settings whenever the panel widgets change them. Also resyncs
    // the filesystem watcher in case the hot-reload toggle flipped.
    {
        let win_weak = win.as_weak();
        let s = state.clone();
        let settings = settings.clone();
        win.on_settings_changed(move || {
            let Some(win) = win_weak.upgrade() else { return };
            {
                let mut sett = settings.borrow_mut();
                sett.auto_read_on_next = win.get_auto_read();
                sett.auto_advance = win.get_auto_advance();
                sett.advance_delay_sec = win.get_advance_delay();
                sett.read_notes = win.get_read_notes();
                sett.transcript_pill_seconds = win.get_transcript_pill_seconds();
                sett.hot_reload = win.get_hot_reload();
                sett.mute_mic_during_speech = win.get_mute_mic_during_speech();
                // Only update the click-through field; the Win32 apply
                // happens unconditionally below so the new state takes effect.
                sett.click_through = win.get_click_through();
                sett.window_opacity = Settings::clamp_window_opacity(win.get_window_opacity());
                sett.tts_rate = win.get_tts_rate();
                sett.tts_volume = win.get_tts_volume();
                if let Err(e) = sett.save(&settings_path()) {
                    eprintln!("[settings] save failed: {e:?}");
                }
            }
            // Apply Win32 click-through + opacity outside the borrow so the
            // applies don't re-enter settings.save.
            #[cfg(windows)]
            {
                let (want_ct, want_op) = {
                    let sett = s.settings.borrow();
                    (sett.click_through, sett.window_opacity)
                };
                overlay::set_click_through(want_ct);
                overlay::set_opacity(want_op);
            }
            // Push rate / volume into the live engine.
            {
                let (r, v) = {
                    let sett = s.settings.borrow();
                    (sett.tts_rate, sett.tts_volume)
                };
                if let Some(engine) = s.tts.borrow_mut().as_mut() {
                    engine.set_rate(r);
                    engine.set_volume(v);
                }
            }
            s.refresh_watcher();
        });
    }

    // Close also persists the final window position so users don't have to
    // wait for the debounced drag-save to fire before quitting.
    {
        let win_weak = win.as_weak();
        let settings = settings.clone();
        win.on_close_clicked(move || {
            if let Some(win) = win_weak.upgrade() {
                let pos = win.window().position();
                if let Some(pos) = safe_position_filter(pos) {
                    let mut s = settings.borrow_mut();
                    s.window_x = Some(pos.x);
                    s.window_y = Some(pos.y);
                    let _ = s.save(&settings_path());
                }
            }
            let _ = slint::quit_event_loop();
        });
    }

    {
        let s = state.clone();
        win.on_reload_pronunciation(move || s.reload_pronunciation());
    }

    // Keyboard FocusScope routes here. handle_event does the capture-vs-
    // dispatch decision so these stubs just pack the modifiers.
    fn pack_mods(ctrl: bool, shift: bool, alt: bool, meta: bool) -> Mods {
        let mut m = Mods::empty();
        if ctrl { m |= Mods::CTRL; }
        if shift { m |= Mods::SHIFT; }
        if alt { m |= Mods::ALT; }
        if meta { m |= Mods::META; }
        m
    }
    {
        let s = state.clone();
        win.on_handle_key(move |text, ctrl, shift, alt, meta| -> bool {
            let Some(trigger) = key_event_to_trigger(text.as_str(), pack_mods(ctrl, shift, alt, meta)) else {
                return false;
            };
            s.handle_event(InputEvent::Press(trigger))
        });
    }
    {
        let s = state.clone();
        win.on_handle_key_up(move |text, ctrl, shift, alt, meta| -> bool {
            let Some(trigger) = key_event_to_trigger(text.as_str(), pack_mods(ctrl, shift, alt, meta)) else {
                return false;
            };
            s.handle_event(InputEvent::Release(trigger))
        });
    }

    // Gamepad / HOTAS listener — gilrs runs on a worker thread, events arrive
    // on `gamepad_rx`, and a UI-thread Timer drains the channel into
    // handle_event so the capture-or-dispatch path is identical to keyboard.
    // Load persisted device-name cache first so the bindings UI's initial
    // render resolves names for currently-unplugged devices the user has
    // seen on previous runs.
    input::gamepad::load_persisted_names();
    let (gamepad_tx, gamepad_rx) = std::sync::mpsc::channel::<InputEvent>();
    if let Err(e) = input::gamepad::spawn(gamepad_tx) {
        eprintln!("[gamepad] disabled: {e:?}");
    }
    let gamepad_poll_timer: Rc<slint::Timer> = Rc::new(slint::Timer::default());
    {
        let s = state.clone();
        let gamepad_poll_timer = gamepad_poll_timer.clone();
        gamepad_poll_timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(16),
            move || {
                while let Ok(event) = gamepad_rx.try_recv() {
                    s.handle_event(event);
                }
            },
        );
    }

    {
        let s = state.clone();
        win.on_binding_edit_clicked(move |id| {
            if let Some(&action) = Action::all().get(id.max(0) as usize) {
                s.start_binding_capture(action);
            }
        });
    }
    {
        let s = state.clone();
        win.on_binding_clear_clicked(move |id| {
            if let Some(&action) = Action::all().get(id.max(0) as usize) {
                s.clear_binding(action);
            }
        });
    }
    {
        let s = state.clone();
        win.on_settings_opened(move || {
            s.stop_speaking();
        });
    }
    // Panel open/close: temporarily disable click-through so the user can
    // interact with the settings or voice-commands list, without changing
    // the persisted setting. Re-applies the saved state on close.
    {
        let s = state.clone();
        let win_weak = win.as_weak();
        win.on_panels_changed(move || {
            let any_open = win_weak
                .upgrade()
                .map(|w| w.get_settings_open() || w.get_voice_commands_open())
                .unwrap_or(false);
            #[cfg(windows)]
            {
                let target = if any_open {
                    false
                } else {
                    s.settings.borrow().click_through
                };
                overlay::set_click_through(target);
            }
            let _ = (any_open, &s);
        });
    }
    {
        let s = state.clone();
        win.on_binding_test_mode_toggled(move |on| {
            eprintln!("[probe] test mode: {}", if on { "ON" } else { "off" });
            // Clear any current flash highlight when toggling so a leftover
            // flash from a previous press doesn't linger.
            if !on {
                if let Some(win) = s.win.upgrade() {
                    win.set_binding_flash_id(-1);
                }
            }
        });
    }

    // Drain whisper transcripts on the UI thread, dispatch the matched
    // Action (if any), and surface the result in the pill. We show the
    // transcript with the matched action so the user can see why the app
    // moved — invaluable when STT mishears.
    let stt_poll_timer: Rc<slint::Timer> = Rc::new(slint::Timer::default());
    if let Some(rx) = stt_rx_main {
        let s = state.clone();
        let stt_poll_timer = stt_poll_timer.clone();
        stt_poll_timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(50),
            move || {
                while let Ok(text) = rx.try_recv() {
                    let trimmed = text.trim();
                    if trimmed.is_empty() {
                        s.show_transcript("(no speech)".into());
                        continue;
                    }
                    // Issue #15 step 2: apply post-STT corrections before
                    // routing. The voice router never sees the raw mis-
                    // transcription, so a "home" → "HARM" rewrite makes
                    // both the displayed pill and the routed intent
                    // agree with what the user actually said.
                    let corrected_owned =
                        voice_router::apply_corrections(trimmed, &s.stt_config.corrections);
                    let routed = corrected_owned.as_deref().unwrap_or(trimmed);
                    if let Some(rewritten) = corrected_owned.as_deref() {
                        eprintln!("[stt] corrected: \"{trimmed}\" → \"{rewritten}\"");
                    }
                    match voice_router::route_with_fuzzy(routed, s.stt_config.fuzzy_threshold) {
                        voice_router::RoutedIntent::Action(action) => {
                            eprintln!("[voice] \"{trimmed}\" → {action:?}");
                            s.show_transcript_match(format!(
                                "{} ({})",
                                action.label(),
                                trimmed
                            ));
                            s.dispatch(action);
                        }
                        voice_router::RoutedIntent::Query(query) => {
                            let label = match &query {
                                voice_router::QueryIntent::NavigateToPage(n) => {
                                    format!("Go to page {n}")
                                }
                                voice_router::QueryIntent::NavigateToSection(t) => {
                                    format!("Find section \"{t}\"")
                                }
                                voice_router::QueryIntent::NavigateToTab(t) => {
                                    format!("Switch to tab \"{t}\"")
                                }
                                voice_router::QueryIntent::ListSections => {
                                    "List sections".to_string()
                                }
                                voice_router::QueryIntent::PickResult(n) => {
                                    format!("Pick result {n}")
                                }
                            };
                            eprintln!("[voice] \"{trimmed}\" → {label}");
                            s.show_transcript_match(format!("{label} ({trimmed})"));
                            s.handle_query(query);
                        }
                        voice_router::RoutedIntent::Unmatched => {
                            eprintln!("[voice] \"{trimmed}\" → (no match)");
                            s.show_transcript_unmatched(trimmed.to_string());
                        }
                    }
                }
            },
        );
    }

    // Hot-reload drain: surface watcher events on the UI thread.
    let watcher_poll_timer: Rc<slint::Timer> = Rc::new(slint::Timer::default());
    {
        let s = state.clone();
        let watcher_poll_timer = watcher_poll_timer.clone();
        watcher_poll_timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(120),
            move || {
                let mut any = false;
                while let Ok(_path) = watch_rx.try_recv() {
                    any = true;
                }
                if any {
                    eprintln!("[watch] reloading active tab");
                    s.reload_active_tab();
                }
            },
        );
    }

    // Boot the watcher in sync with current settings + active tab.
    state.refresh_watcher();

    // Seed the STT worker's initial_prompt for the boot aircraft so the
    // very first PTT release benefits from vocabulary biasing — without
    // this the first transcript would run with an empty prompt and only
    // subsequent aircraft switches would push one. Issue #15.
    state.push_stt_initial_prompt(&aircraft);

    // Populate the bindings list once at startup so the settings panel opens
    // with the current state.
    state.refresh_bindings_ui();
    state.refresh_audio_ui();
    state.refresh_tts_ui();

    // Build the voice-commands help model once — RULES, query_examples and
    // the alias hint are all static so this never changes during a session.
    // Layout: three section headers each followed by their rows.
    {
        let header = |label: &str| VoiceCommandRow {
            action_label: SharedString::from(label),
            phrases: SharedString::default(),
            is_header: true,
        };
        let command = |label: &str, phrases: String| VoiceCommandRow {
            action_label: SharedString::from(label),
            phrases: SharedString::from(phrases),
            is_header: false,
        };

        let mut rows: Vec<VoiceCommandRow> = Vec::new();

        // Section 1: literal-phrase Action commands.
        rows.push(header("COMMANDS"));
        rows.extend(
            voice_router::all_rules()
                .iter()
                .map(|(phrases, action)| command(action.label(), phrases.join(", "))),
        );

        // Section 2: free-form query intents.
        rows.push(header("VOICE QUERIES (FREE-FORM)"));
        rows.extend(
            voice_router::query_examples()
                .iter()
                .map(|(label, examples)| command(label, examples.join(", "))),
        );

        // Section 3: phonetic-alias system pointer.
        rows.push(header("PHONETIC ALIASES"));
        rows.push(command(
            "Aliases (query_aliases.toml)",
            "Common: Maverick → AGM-65, Sidewinder → AIM-9, Slammer → AIM-120, HARM → AGM-88, Mark 82 → MK-82. Edit query_aliases.toml + press F5.".to_string(),
        ));

        win.set_voice_commands(slint::ModelRc::new(VecModel::from(rows)));
    }

    {
        let s = state.clone();
        win.on_tts_engine_clicked(move |name| s.set_tts_engine(name.to_string()));
    }
    {
        let s = state.clone();
        win.on_piper_voice_clicked(move |voice| s.set_piper_voice(voice.to_string()));
    }
    {
        let s = state.clone();
        win.on_tts_test_clicked(move || s.test_tts());
    }
    {
        let s = state.clone();
        win.on_audio_input_clicked(move |name| {
            // Clicking the already-selected device unselects → falls back to
            // system default. (Matches "click the active tab to deselect"
            // affordance some users expect.)
            let current = s.settings.borrow().audio_input.clone();
            let target = if current.as_deref() == Some(name.as_str()) {
                None
            } else {
                Some(name.to_string())
            };
            s.set_audio_input(target);
        });
    }

    // Drag-move: relocate the window and debounce-save the new position to
    // settings.toml so it survives next launch (and crashes).
    let position_save_timer: Rc<slint::Timer> = Rc::new(slint::Timer::default());
    {
        let win_weak = win.as_weak();
        let settings = settings.clone();
        let position_save_timer = position_save_timer.clone();
        win.on_drag_by(move |dx, dy| {
            let Some(win) = win_weak.upgrade() else { return };
            let scale = win.window().scale_factor();
            let pos = win.window().position();
            let new_pos = slint::PhysicalPosition::new(
                pos.x + (dx * scale) as i32,
                pos.y + (dy * scale) as i32,
            );
            win.window().set_position(new_pos);

            // Debounce 500 ms after last drag tick. Restarting the timer
            // resets the countdown so we only save once the drag settles.
            let win_weak = win_weak.clone();
            let settings = settings.clone();
            position_save_timer.start(
                slint::TimerMode::SingleShot,
                Duration::from_millis(500),
                move || {
                    let Some(win) = win_weak.upgrade() else { return };
                    let pos = win.window().position();
                    if let Some(pos) = safe_position_filter(pos) {
                        let mut s = settings.borrow_mut();
                        s.window_x = Some(pos.x);
                        s.window_y = Some(pos.y);
                        if let Err(e) = s.save(&settings_path()) {
                            eprintln!("[settings] position save failed: {e:?}");
                        }
                    }
                },
            );
        });
    }

    // Apply persisted Win32 overlay attributes on a short delay so the
    // window has actually been shown — FindWindowW needs the HWND to
    // exist. Opacity always applies; click-through only when persisted on.
    #[cfg(windows)]
    {
        let want_ct = settings.borrow().click_through;
        let want_op = Settings::clamp_window_opacity(settings.borrow().window_opacity);
        if want_ct || (want_op - 1.0).abs() > f32::EPSILON {
            let timer = Box::new(slint::Timer::default());
            let timer_ref: &slint::Timer = Box::leak(timer);
            timer_ref.start(
                slint::TimerMode::SingleShot,
                Duration::from_millis(400),
                move || {
                    if want_ct {
                        overlay::set_click_through(true);
                    }
                    if (want_op - 1.0).abs() > f32::EPSILON {
                        overlay::set_opacity(want_op);
                    }
                },
            );
        }
    }

    win.run()?;
    Ok(())
}

#[cfg(test)]
mod armed_cursor_tests {
    //! Unit tests for `place_armed_cursor`, the pure helper that decides
    //! where the cursor lands and which heading gets spoken after a
    //! section jump (issue #17). The rest of the armed state lives on
    //! `AppState` which is mixed with TTS / Slint / settings and would
    //! need extraction before it can be unit-tested cleanly.
    use super::*;

    fn item(kind: &str, text: &str, navigable: bool) -> tabs::Item {
        tabs::Item {
            idx: 0,
            group: String::new(),
            kind: kind.into(),
            text: text.into(),
            spoken: None,
            navigable,
            bbox: [0.0; 4],
        }
    }

    fn heading(text: &str) -> tabs::Item {
        item("section-header", text, false)
    }

    fn step(text: &str) -> tabs::Item {
        item("step", text, true)
    }

    fn note(text: &str) -> tabs::Item {
        item("note-info", text, false)
    }

    #[test]
    fn lands_on_first_nav_after_heading_when_starting_on_heading() {
        let items = vec![
            step("step before"),         // 0
            heading("Maverick"),         // 1
            step("FCR ON"),              // 2
            step("AGM ON"),              // 3
        ];
        let (cursor, heading_idx) = place_armed_cursor(&items, 1);
        assert_eq!(cursor, 2, "cursor should advance to first navigable step");
        assert_eq!(heading_idx, Some(1), "header spoken should be the heading we landed on");
    }

    /// Edge case from the issue: jump target is a non-navigable item
    /// (note). Land on the next navigable item, still pick up the
    /// preceding section header.
    #[test]
    fn lands_on_next_nav_when_starting_on_non_navigable_note() {
        let items = vec![
            heading("Maverick"),         // 0
            note("Optional warning"),    // 1 ← cursor here
            step("FCR ON"),              // 2
        ];
        let (cursor, heading_idx) = place_armed_cursor(&items, 1);
        // Note isn't a heading so we don't shift the cursor forward; the
        // armed cursor sits on the note, and start/go will speak it.
        // The heading lookup walks backward and finds the section header.
        assert_eq!(cursor, 1);
        assert_eq!(heading_idx, Some(0));
    }

    /// Edge case: section header has only one navigable item.
    /// arm_after_section_jump must still place the cursor there.
    #[test]
    fn section_with_only_one_step() {
        let items = vec![
            heading("Bingo"),            // 0
            step("RTB"),                 // 1
        ];
        let (cursor, heading_idx) = place_armed_cursor(&items, 0);
        assert_eq!(cursor, 1);
        assert_eq!(heading_idx, Some(0));
    }

    /// Heading with no following navigable item (trailing notes only).
    /// We fall back to the heading itself as the cursor — the controller
    /// then speaks the header and arms; pressing Next finds nothing to
    /// advance to, which is the same behaviour as today.
    #[test]
    fn heading_with_no_following_navigable_falls_back() {
        let items = vec![
            heading("Notes"),            // 0
            note("Just info"),           // 1
            note("More info"),           // 2
        ];
        let (cursor, heading_idx) = place_armed_cursor(&items, 0);
        assert_eq!(cursor, 0, "no navigable item after heading — stay put");
        assert_eq!(heading_idx, Some(0));
    }

    /// Cursor sits on a navigable item already inside a section (the
    /// shape produced by a NavigateToSection query that returned the
    /// resolved item index). Don't shift; just find the preceding
    /// heading.
    #[test]
    fn cursor_already_inside_section() {
        let items = vec![
            heading("Maverick"),         // 0
            step("FCR ON"),              // 1
            step("AGM ON"),              // 2 ← cursor here
            step("UNCAGE"),              // 3
        ];
        let (cursor, heading_idx) = place_armed_cursor(&items, 2);
        assert_eq!(cursor, 2);
        assert_eq!(heading_idx, Some(0));
    }

    /// Page with no heading at all (rare, but the manifest doesn't
    /// require one). Return None so the caller can fall back to the
    /// page title.
    #[test]
    fn no_heading_on_page() {
        let items = vec![
            step("free-floating step"),  // 0
            step("another"),             // 1
        ];
        let (cursor, heading_idx) = place_armed_cursor(&items, 1);
        assert_eq!(cursor, 1);
        assert_eq!(heading_idx, None);
    }

    /// Empty manifest must not panic.
    #[test]
    fn empty_items_does_not_panic() {
        let items: Vec<tabs::Item> = vec![];
        let (cursor, heading_idx) = place_armed_cursor(&items, 0);
        assert_eq!(cursor, 0);
        assert_eq!(heading_idx, None);
    }
}
