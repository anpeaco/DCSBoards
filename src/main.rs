// During development we keep a console attached so stderr (tracing logs, panics)
// is visible. Re-enable the windows subsystem at M9 packaging:
// #![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod config;
mod settings;
mod tabs;
mod tts;

use anyhow::Result;
use config::{config_path, AppConfig};
use settings::{settings_path, Settings};
use slint::{ComponentHandle, SharedString, VecModel};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;
use tabs::{first_navigable, Cursor, Item, LoadedPage, TabRegistry};
use tts::{spoken_for, PronunciationConfig, TtsEngine};

// Win32 overlay flags (WS_EX_LAYERED for opacity, WS_EX_TRANSPARENT for click-through)
// will be added in M7 polish via raw HWND access through the `windows` crate.

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
        in-out property <bool> auto-read: false;
        in-out property <bool> auto-advance: false;
        in-out property <float> advance-delay: 1.5;
        in-out property <bool> read-notes: false;
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
        callback reload-pronunciation();

        title: "DCS Kneeboard";
        width: 600px;
        height: 900px;
        background: #000;
        no-frame: true;
        always-on-top: true;
        forward-focus: focus;

        focus := FocusScope {
            x: 0; y: 0; width: 0; height: 0;
            key-pressed(event) => {
                if (event.text == " ") { root.next-clicked(); return accept; }
                if (event.text == Key.Backspace) { root.prev-clicked(); return accept; }
                if (event.text == "r" || event.text == "R") { root.read-clicked(); return accept; }
                if (event.text == "h") { root.next-heading-clicked(); return accept; }
                if (event.text == "H") { root.prev-heading-clicked(); return accept; }
                if (event.text == Key.PageDown) { root.page-next-clicked(); return accept; }
                if (event.text == Key.PageUp) { root.page-prev-clicked(); return accept; }
                if (event.text == Key.F5) { root.reload-pronunciation(); return accept; }
                if (event.text == Key.Escape) {
                    if (root.settings-open) { root.settings-open = false; return accept; }
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
        Rectangle {
            x: root.hl-x;
            y: root.hl-y;
            width: root.hl-w;
            height: root.hl-h;
            border-color: #ffcc33;
            border-width: 2px;
            background: #ffcc3322;
            border-radius: 2px;
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

            // Auto-unpins ~1.2s after a tab cycle so the strip flashes for context.
            Timer {
                interval: 1.2s;
                running: root.strip-pinned;
                triggered() => {
                    root.strip-pinned = false;
                }
            }

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
        // reads cleanly over any kneeboard page.
        if root.settings-open: Rectangle {
            x: 30px;
            y: 40px;
            width: 540px;
            height: 800px;
            background: #2a2a32;
            border-color: #555;
            border-width: 1px;
            border-radius: 8px;

            VerticalLayout {
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

                Rectangle { height: 8px; }
                Rectangle { height: 1px; background: #444; }

                Text { text: "KEYBOARD BINDINGS"; color: #c0c0c0; font-size: 12px; font-weight: 500; }
                Text {
                    text: "Space      Next item\nBackspace  Previous item\nR          Read / Stop\nH          Next heading\nShift + H  Previous heading\nPage Down  Next page\nPage Up    Previous page\nF5         Reload pronunciation.toml\nEsc        Close this panel";
                    color: #f0f0f0;
                    font-size: 13px;
                }
                Text {
                    text: "HOTAS bindings land in M4. Rebindable keys come with capture-mode UI.";
                    color: #a0a0a0;
                    font-size: 12px;
                    wrap: word-wrap;
                }

                Rectangle { vertical-stretch: 1; }

                HorizontalLayout {
                    alignment: end;
                    Button {
                        text: "Close";
                        clicked => { root.settings-open = false; }
                    }
                }
            }
        }
    }
}

const DISPLAY_W: f32 = 600.0;
const DISPLAY_H: f32 = 900.0;

fn init_tts() -> Result<Box<dyn TtsEngine>> {
    #[cfg(windows)]
    {
        Ok(Box::new(tts::WinRtTts::new()?))
    }
    #[cfg(not(windows))]
    {
        Ok(Box::new(tts::NoopTts::new()?))
    }
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
        for p in (cur.page + 1)..pages.len() {
            if let Some((idx, _)) = pages[p]
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

    win.set_page_image(page.image.clone());
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
    settings: Rc<RefCell<Settings>>,
    win: slint::Weak<MainWindow>,
    advance_timer: Rc<slint::Timer>,
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
        let mut s = self.settings.borrow_mut();
        s.current_aircraft = Some(aircraft);
        let _ = s.save(&settings_path());
    }

    fn next(&self) { self.nav(|p, c| step_cursor(p, c, 1)); }
    fn prev(&self) { self.nav(|p, c| step_cursor(p, c, -1)); }
    fn page_next(&self) { self.nav(|p, c| jump_page(p, c, 1)); }
    fn page_prev(&self) { self.nav(|p, c| jump_page(p, c, -1)); }
    fn next_heading(&self) { self.nav(|p, c| step_to_heading(p, c, 1)); }
    fn prev_heading(&self) { self.nav(|p, c| step_to_heading(p, c, -1)); }

    /// Read button: toggle. Stop if currently playing; otherwise start.
    fn read(&self) {
        if self.is_playing() {
            self.stop_speaking();
        } else {
            self.start_speaking();
        }
    }
}

fn main() -> Result<()> {
    let app_config = AppConfig::load_or_default(&config_path());
    let settings = Rc::new(RefCell::new(Settings::load_or_default(&settings_path())));

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

    let tts: Rc<RefCell<Option<Box<dyn TtsEngine>>>> = Rc::new(RefCell::new(
        init_tts().map_err(|e| eprintln!("TTS init failed: {e:?}")).ok(),
    ));

    let win = MainWindow::new()?;
    {
        let s = settings.borrow();
        win.set_auto_read(s.auto_read_on_next);
        win.set_auto_advance(s.auto_advance);
        win.set_advance_delay(s.advance_delay_sec);
        win.set_read_notes(s.read_notes);
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

    let state = AppState {
        tabs: tabs_rc.clone(),
        tts: tts.clone(),
        pronunciation: pronunciation.clone(),
        settings: settings.clone(),
        win: win.as_weak(),
        advance_timer: Rc::new(slint::Timer::default()),
    };
    state.apply();

    {
        let s = state.clone();
        win.on_next_clicked(move || s.next());
    }
    {
        let s = state.clone();
        win.on_prev_clicked(move || s.prev());
    }
    {
        let s = state.clone();
        win.on_page_next_clicked(move || s.page_next());
    }
    {
        let s = state.clone();
        win.on_page_prev_clicked(move || s.page_prev());
    }
    {
        let s = state.clone();
        win.on_next_heading_clicked(move || s.next_heading());
    }
    {
        let s = state.clone();
        win.on_prev_heading_clicked(move || s.prev_heading());
    }
    {
        let s = state.clone();
        win.on_read_clicked(move || s.read());
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

    // Persist settings whenever the panel widgets change them.
    {
        let win_weak = win.as_weak();
        let settings = settings.clone();
        win.on_settings_changed(move || {
            let Some(win) = win_weak.upgrade() else { return };
            let mut s = settings.borrow_mut();
            s.auto_read_on_next = win.get_auto_read();
            s.auto_advance = win.get_auto_advance();
            s.advance_delay_sec = win.get_advance_delay();
            s.read_notes = win.get_read_notes();
            if let Err(e) = s.save(&settings_path()) {
                eprintln!("[settings] save failed: {e:?}");
            }
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
                let mut s = settings.borrow_mut();
                s.window_x = Some(pos.x);
                s.window_y = Some(pos.y);
                let _ = s.save(&settings_path());
            }
            let _ = slint::quit_event_loop();
        });
    }

    {
        let pronunciation = pronunciation.clone();
        win.on_reload_pronunciation(move || {
            let fresh = PronunciationConfig::load_or_default(&PathBuf::from("pronunciation.toml"));
            *pronunciation.borrow_mut() = fresh;
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
                    let mut s = settings.borrow_mut();
                    s.window_x = Some(pos.x);
                    s.window_y = Some(pos.y);
                    if let Err(e) = s.save(&settings_path()) {
                        eprintln!("[settings] position save failed: {e:?}");
                    }
                },
            );
        });
    }

    win.run()?;
    Ok(())
}
