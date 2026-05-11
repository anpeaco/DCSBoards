# DCS Kneeboard — Project Specification

## 1. Overview

A Windows desktop application that runs alongside DCS World and provides a HUD-style overlay kneeboard. The user advances through aircraft checklists using voice commands (push-to-talk) and the app reads checklist items aloud. Checklist content is authored as markdown, consistent with an existing HTML-checklist-generation tool the user already maintains.

The app is local-first: all speech recognition and synthesis run on-device. No network is required at runtime.

## 2. Goals

- Sit reliably over DCS in borderless windowed mode, always on top.
- Voice-driven checklist navigation: next, previous, repeat, jump to a named checklist.
- Spoken readback of checklist items, with phonetically-tunable pronunciation for aviation jargon.
- Reuse the user's existing markdown checklist authoring workflow.
- Clean engine abstractions so STT and TTS backends can be swapped.
- Single-binary distribution plus a small set of asset files (models, voices, checklists).

## 3. Non-goals (initial release)

- macOS / Linux support. Windows only.
- Cloud STT/TTS.
- Multiplayer features, shared state, telemetry.
- Editing checklists in-app — authoring happens externally in markdown.
- Touchscreen / tablet UI as the primary target (a future second-monitor mode is plausible).
- Modifying DCS itself. The app is strictly an external companion.

## 4. Target environment

- Windows 10 (22H2+) and Windows 11. WebView2 not required.
- Typical user hardware: gaming desktop, modern multi-core CPU, discrete GPU saturated by DCS. STT must run acceptably on CPU.
- DCS World running in borderless windowed mode. Exclusive fullscreen is out of scope.
- Audio: user wears a headset; mic input is noisy (engine, comms). Push-to-talk is required, not optional.

## 5. Tech stack

| Concern              | Choice                                              |
|----------------------|-----------------------------------------------------|
| Language             | Rust (stable)                                       |
| UI                   | Slint                                               |
| Window / overlay     | `windows` crate (Win32) for always-on-top, layered, optional click-through |
| Audio capture        | `cpal` + `ringbuf`                                  |
| STT                  | `whisper-rs` (whisper.cpp bindings), small/base.en  |
| TTS (initial)        | WinRT `Windows.Media.SpeechSynthesis` via `windows` crate |
| TTS (future)         | Piper (subprocess or ONNX), Kokoro (ONNX) — swappable behind trait |
| Keyboard hotkeys     | `global-hotkey`                                     |
| Gamepad / HOTAS      | `gilrs` (DirectInput + XInput + SDL mappings)       |
| Raw HID              | `hidapi` (for button boxes, MFD panels, custom devices) |
| Resampling           | `rubato`                                            |
| Config / data        | TOML via `serde` + `toml`                           |
| Markdown parsing     | `pulldown-cmark`                                    |
| Async                | `tokio` (rt-multi-thread)                           |
| Logging              | `tracing`, `tracing-subscriber`                     |
| Errors               | `anyhow` at app boundaries, `thiserror` for library modules |

## 6. High-level architecture

```
+---------------------------------------------------------------+
|                          Slint UI                              |
|       (checklist view, mic indicator, voice picker)            |
+----------------------------↑----------------------------------+
                             |  bindings (callbacks, properties)
+----------------------------↓----------------------------------+
|                     ChecklistController                        |
|  - holds current aircraft, current list, current item index   |
|  - routes voice commands to actions                            |
|  - asks TTS to read items                                      |
+------↑----------------↑-----------------↑----------------↑----+
       |                |                 |                |
+------+-----+   +------+-----+   +-------+------+  +------+-----+
| HotkeyMgr  |   | AudioCapture|  | SttEngine    |  | TtsEngine  |
| (PTT)      |   |  (cpal)     |  | trait        |  | trait      |
+------------+   +-------------+  +--------------+  +------------+
                                  | WhisperImpl  |  | WinRtImpl  |
                                  +--------------+  +------------+
+---------------------------------------------------------------+
|                      ChecklistStore                            |
|  - loads markdown files, parses to internal model              |
|  - watches the checklists/ directory for changes               |
+---------------------------------------------------------------+
+---------------------------------------------------------------+
|                  OverlayWindow (Win32 glue)                    |
|  - always-on-top, layered, optional click-through              |
+---------------------------------------------------------------+
            (optional, later)
+---------------------------------------------------------------+
|                   DcsExportListener                            |
|  - UDP socket receiving data from Export.lua                   |
|  - publishes aircraft type + flight phase hints                |
+---------------------------------------------------------------+
```

All inter-module communication uses owned messages over `tokio::sync::mpsc` channels where async is involved, and direct method calls where it isn't. The UI thread never blocks on STT or TTS work.

## 7. Modules

### 7.1 `overlay`
- `configure_overlay(handle, click_through)`: sets `WS_EX_TOPMOST | WS_EX_LAYERED | WS_EX_TOOLWINDOW`, plus `WS_EX_TRANSPARENT` when click-through is enabled.
- Toggle click-through via a global hotkey (default: `Ctrl+Alt+K`).
- Saves and restores window position to config.

### 7.2 `audio::capture`
- Opens default input device at native sample rate via `cpal`.
- Writes f32 mono samples to a `ringbuf` producer.
- Provides `start_recording()` / `stop_recording()` that snapshot the ring buffer between calls and resample to 16 kHz mono for Whisper.

### 7.3 `audio::stt`
- Trait:
  ```rust
  pub trait SttEngine: Send {
      fn transcribe(&mut self, samples_16k_mono: &[f32]) -> Result<String>;
      fn name(&self) -> &'static str;
  }
  ```
- `WhisperStt` implementation wraps `whisper-rs`. Loads model once, reuses state per call. English-only, single-segment mode.

### 7.4 `audio::tts`
- Trait (already designed):
  ```rust
  pub trait TtsEngine: Send {
      fn speak(&mut self, text: &str, interrupt: bool) -> Result<()>;
      fn stop(&mut self) -> Result<()>;
      fn is_speaking(&self) -> bool;
      fn set_rate(&mut self, rate: f32) -> Result<()>;
      fn set_volume(&mut self, volume: f32) -> Result<()>;
      fn voices(&self) -> Result<Vec<VoiceInfo>>;
      fn set_voice(&mut self, voice_id: &str) -> Result<()>;
      fn name(&self) -> &'static str;
  }
  ```
- Initial implementation: `WinRtTts` using `Windows.Media.SpeechSynthesis` + `MediaPlayer` with `AudioCategory::Speech`.
- Future implementations: `PiperTts`, `KokoroTts`. Selection via config enum `TtsBackend`.

### 7.5 `input`

Action-to-trigger binding system. Replaces a hard-coded PTT hotkey with a flexible map of `Action → Trigger`, where triggers can come from keyboard, gamepad/HOTAS, or raw HID devices.

#### Sources

```rust
pub trait InputSource: Send {
    /// Drain any pending events since the last call.
    fn poll(&mut self) -> Vec<InputEvent>;
    fn name(&self) -> &'static str;
}
```

Initial implementations:

- `KeyboardSource` — wraps `global-hotkey`. Used for combos like `Ctrl+Alt+K`.
- `GamepadSource` — wraps `gilrs`. Covers all DirectInput / XInput devices: HOTAS sticks, throttles, rudder pedals, gamepads, and any button box that presents as a game controller. This includes **FreeJoy-based devices** (STM32 DIY boards), which expose themselves as standard HID gamepads with up to 128 buttons, 8 axes, and 4 hats. FreeJoy encoders typically report as momentary button presses (one button for CW, another for CCW), which makes them natural triggers for `Next` / `Previous`.
- `HidSource` — wraps `hidapi`. Used for devices that *don't* present as game controllers (e.g. some MFD panels, Stream Decks, custom Arduino-based boards). Configured by `vendor_id` + `product_id` + a small report-decoding rule per device.

The `InputManager` owns a `Vec<Box<dyn InputSource>>`, polls them on a timer (~5 ms), and dispatches matched events to action handlers.

#### Triggers and actions

```rust
pub enum Trigger {
    Keyboard { combo: String },                          // "RightCtrl", "Ctrl+Alt+K"
    GamepadButton { device_id: DeviceId, button: u32 },
    GamepadAxis {                                        // axis past threshold = "pressed"
        device_id: DeviceId,
        axis: u32,
        threshold: f32,
        direction: AxisDirection,                        // Positive | Negative
    },
    GamepadHat { device_id: DeviceId, hat: u32, direction: HatDirection },
    HidButton {
        vendor_id: u16,
        product_id: u16,
        usage_page: u16,
        usage: u16,
        bit_offset: u32,                                 // bit in the input report
    },
}

pub struct DeviceId {
    pub guid: String,         // gilrs GUID — stable across reconnects
    pub display_name: String, // human-readable, for UI
}

pub enum Action {
    PushToTalk,               // edge-sensitive: needs Press AND Release
    Next,
    Previous,
    Repeat,
    ReadCurrent,
    ToggleClickThrough,
    ToggleVisibility,
    LoadList(String),         // bound to a button per list, optional
    Cancel,
}

pub enum Edge { Press, Release, Hold }

pub struct Binding {
    pub action: Action,
    pub trigger: Trigger,
    pub edge: Edge,           // most actions: Press. PTT: handled as both edges automatically.
}
```

#### Behavior

- **PTT is special.** A binding with `action = PushToTalk` is treated as edge-sensitive: press starts recording, release stops and submits to STT. The router knows this implicitly; users only bind one trigger.
- **Press-only by default** for everything else. "Next" fires once per press; the user can't accidentally spam it by holding a button.
- **Multiple bindings per action are allowed.** A user can bind PTT to both a HOTAS button *and* a keyboard key as a backup.
- **No modifier combos on gamepad buttons in v1.** Just plain button presses. (Shift-state / chord support is doable later — a `modifiers: Vec<Trigger>` field on `Binding` — but it's a bunch of UX complexity and most HOTAS users handle this in vendor software anyway.)

#### Capture mode (for the bind-UI dialog)

`InputManager::start_capture(timeout)` puts the manager in a mode where it returns the *first* non-trivial event observed instead of dispatching. The settings UI uses this for the "Press the button you want to bind…" flow. Includes a small filter to ignore noisy axes (only triggers axis bindings if the axis moves more than ~70% of its range from rest).

Capture-mode is the **only** binding UX in v1 — no button-grid pickers. This matters for devices like FreeJoy boards that can expose 64+ buttons with no standard labels; the user just presses what they want.

#### Device display

Some devices (notably multiple FreeJoy boards on the same machine) share a display name. The bind UI disambiguates by:

- Always showing the device's `display_name` from `gilrs`.
- If two connected devices share a name, append ` (#2)`, ` (#3)`, etc. based on connection order.
- Persisting the **GUID**, not the display name or order, so bindings survive reconnect / reboot regardless of which order the OS enumerates devices.

For buttons without standard gamepad labels (the vast majority of FreeJoy / button-box buttons), the UI shows `Button N` using the raw button code from `gilrs`. No attempt is made to map to A/B/X/Y semantics for non-gamepad devices.

#### Hot-plug

- `gilrs` emits `Connected` / `Disconnected` events; the manager tracks them.
- Bindings reference devices by GUID, not by connection order, so unplugging and replugging a stick keeps bindings working.
- If a bound device is absent, its bindings are simply inactive until it returns. A small UI indicator shows which bound devices are currently disconnected.

#### Raw HID configuration

Generic HID is open-ended, so v1 supports a curated approach: device profiles describe how to interpret a device's input reports.

```toml
# devices/winwing_mfd.toml  (example, shipped or user-authored)
[device]
vendor_id  = 0x4098
product_id = 0xBE60
display_name = "Winwing MFD"

[[buttons]]
name = "OSB-1"
report_id = 1
byte_offset = 2
bit_offset  = 0

[[buttons]]
name = "OSB-2"
report_id = 1
byte_offset = 2
bit_offset  = 1
# ...
```

The `HidSource` reads input reports and emits `HidButton` events using these maps. v1 ships profiles for a small set of popular devices; users can author more. This is an escape hatch — most users will never need it because `gilrs` already covers their hardware.

#### Default bindings

| Action               | Default trigger              |
|----------------------|------------------------------|
| PushToTalk           | Keyboard `RightCtrl`         |
| Next                 | (unbound — user binds)       |
| Previous             | (unbound)                    |
| Repeat               | (unbound)                    |
| ReadCurrent          | (unbound)                    |
| ToggleClickThrough   | Keyboard `Ctrl+Alt+K`        |
| ToggleVisibility     | Keyboard `Ctrl+Alt+H`        |

The user is expected to bind PTT and nav actions to their HOTAS in settings.

### 7.6 `checklist`
- `model`: `Aircraft`, `Checklist`, `ChecklistItem` (see §8).
- `loader`: parses markdown files into the model (see §9).
- `store`: in-memory index of all loaded checklists, watches the directory via `notify` for hot-reload.
- `controller`: navigation state (current aircraft, list, index), command dispatch.

### 7.7 `voice_router`
- Maps transcribed text to commands using case-insensitive fuzzy contains-matching plus a small synonym table.
- Commands (initial set):
  - `Next` — "next", "next item", "continue", "check"
  - `Previous` — "back", "previous", "go back"
  - `Repeat` — "repeat", "again", "say again"
  - `ReadCurrent` — "read", "read it", "what's next"
  - `LoadList(name)` — "startup", "taxi", "takeoff", "approach", "landing", "shutdown", "emergency"
  - `Cancel` — "cancel", "stop", "quiet"
- Unmatched transcripts are logged and shown briefly in the UI for debugging.

### 7.8 `dcs::export` (optional, deferred)
- UDP listener on `127.0.0.1:<port>` receiving JSON or key=value frames from a user-installed `Export.lua`.
- Publishes `AircraftDetected(name)` and `FlightPhaseHint(phase)` to the controller.
- Strictly optional: the app must work fully without this.

## 8. Data model

```rust
pub struct Aircraft {
    pub id: String,           // "f16c"
    pub display_name: String, // "F-16C Viper"
    pub checklists: Vec<Checklist>,
}

pub struct Checklist {
    pub id: String,           // "startup"
    pub display_name: String, // "Engine Start"
    pub items: Vec<ChecklistItem>,
}

pub struct ChecklistItem {
    pub text: String,             // shown on screen
    pub spoken: Option<String>,   // alternative pronunciation for TTS
    pub note: Option<String>,     // optional secondary line (smaller font)
    pub group: Option<String>,    // optional section heading the item belongs to
}
```

## 9. Markdown checklist format

**This section is a placeholder.** The user maintains an existing markdown-to-HTML checklist generator. The new app will consume the same markdown so authoring stays in one place.

Open questions to resolve once the existing format is provided:

- File layout: one file per checklist, or one file per aircraft with multiple sections?
- Front-matter format: YAML, TOML, or convention-based filenames?
- How are sections / groups delimited (headings, horizontal rules)?
- How is each checklist item represented (list items, table rows, definition lists)?
- Is there an existing convention for a "spoken" override per item?
- Are there any extensions used (callouts, admonitions, images) we need to render or skip?

Until that's settled, the loader will be designed around a small `MarkdownChecklistParser` trait so the concrete parsing rules can be filled in without disturbing the rest of the app:

```rust
pub trait MarkdownChecklistParser {
    fn parse_aircraft(&self, root: &Path) -> Result<Vec<Aircraft>>;
}
```

Once the format is finalized, this section will be expanded to document:

- Directory and file conventions.
- Front-matter fields and required vs. optional keys.
- Mapping rules from markdown constructs → `ChecklistItem` fields.
- How `spoken` overrides are authored (e.g. inline `{spoken="..."}` attribute, footnote-style, or separate YAML key).
- Validation rules and error messages.

## 10. Configuration

Single `config.toml` in `%APPDATA%/dcs-kneeboard/`:

```toml
[window]
x = 100
y = 100
width = 420
height = 600
always_on_top = true
click_through = false
opacity = 0.85

[bindings]
# Each binding: action = "...", and one of the trigger forms below.
# Multiple bindings per action are allowed.

[[bindings.entries]]
action = "PushToTalk"
kind   = "keyboard"
combo  = "RightCtrl"

[[bindings.entries]]
action = "PushToTalk"
kind   = "gamepad_button"
device_guid = "030000004f04000087b6000000000000"  # example: TM Warthog stick
device_name = "Thrustmaster Warthog Joystick"     # informational only
button = 5

[[bindings.entries]]
action = "Next"
kind   = "gamepad_button"
device_guid = "030000004f04000087b6000000000000"
device_name = "Thrustmaster Warthog Joystick"
button = 6

[[bindings.entries]]
action = "ToggleClickThrough"
kind   = "keyboard"
combo  = "Ctrl+Alt+K"

[hid_devices]
# Optional HID profile files for devices that don't appear as gamepads.
profile_paths = ["devices/winwing_mfd.toml"]

[stt]
backend = "whisper"
model_path = "models/ggml-base.en.bin"
language = "en"

[tts]
backend = "winrt"   # "winrt" | "piper" | "kokoro"
voice_id = ""        # empty = auto-pick neural en-* if available
rate = 1.1
volume = 0.9

[checklists]
path = "checklists"  # relative to app dir, or absolute
watch = true

[dcs_export]
enabled = false
listen_addr = "127.0.0.1:34380"
```

## 11. UX flows

### 11.1 First run
1. App starts, loads config (creates defaults if absent).
2. Scans `checklists/` and loads what it finds.
3. Shows the first aircraft's first checklist's first item.
4. Window is always-on-top, semi-transparent, positioned per saved coords.

### 11.2 Reading a checklist
1. User presses PTT, says "read".
2. STT transcribes → router emits `ReadCurrent`.
3. Controller asks TTS to speak the current item (using `spoken` if set).
4. User completes the action, presses PTT, says "next".
5. Current item marked complete, index advances, optional auto-read of next item (configurable).

### 11.3 Jumping between checklists
1. User says "taxi" → router emits `LoadList("taxi")`.
2. Controller loads that list for the current aircraft, resets index to 0.

### 11.4 Aircraft switch
- Manual via UI dropdown.
- Or, if `dcs_export` is enabled, automatic when an `AircraftDetected` event arrives.

## 12. Error handling and resilience

- Missing model file: app starts, UI shows a clear "Whisper model not found at <path>" banner; checklist nav still works via UI buttons.
- TTS init failure: log, disable TTS calls, UI shows speaker-muted icon. App remains usable.
- Audio device disappears (e.g. headset unplugged): catch `cpal` errors, surface a banner, attempt to reopen on next PTT.
- Markdown parse errors: per-file, with file path + line in the log; the offending file is skipped, others load.
- Hotkey registration conflict: surface to the user, fall back to in-UI buttons.

## 13. Performance budget

- Idle CPU: under 1% on a modern desktop.
- STT latency for a 2-second utterance: under 500 ms on CPU with `base.en`. Acceptable up to 1 s for a kneeboard.
- TTS time-to-first-audio: under 300 ms with WinRT.
- Memory: under 400 MB resident with Whisper model loaded.
- Frame rate: Slint window at 60 FPS when visible, throttled when no input.

## 14. Distribution

- Single `dcs-kneeboard.exe`.
- Sidecar folder structure next to the exe:
  ```
  dcs-kneeboard.exe
  models/
    ggml-base.en.bin
  checklists/
    f16c/
    a10c/
    ...
  config.toml   (generated on first run in %APPDATA%)
  ```
- Installer is out of scope for v1; ship a zip.
- Code signing: nice-to-have; document SmartScreen behavior in the README.

## 15. Build & dev workflow

- `cargo run` for dev. Slint hot-reload via `slint-build` is sufficient for iterating on `.slint` files.
- Lints: `cargo clippy -- -D warnings` in CI.
- Format: `cargo fmt`.
- Tests:
  - Unit tests for the voice router (string → command).
  - Unit tests for the markdown parser once the format is locked in.
  - Integration test for `ChecklistController` state transitions (no UI required — pure model).
- CI: GitHub Actions, Windows runner, build + test + clippy.

## 16. Milestones

1. **M1 — Skeleton.** Slint window, always-on-top + layered flags, hardcoded checklist, navigation via on-screen buttons.
2. **M2 — TTS.** `TtsEngine` trait + WinRT impl. "Read" button works.
3. **M3 — STT + PTT.** `SttEngine` trait + Whisper impl, `cpal` capture, push-to-talk hotkey, voice router. Logs transcripts.
4. **M4 — Markdown loader.** Once the format spec is in, parse real checklists from disk. Hot-reload via `notify`.
5. **M5 — Config + persistence.** `config.toml`, window position, voice/rate selection, hotkey rebinding.
6. **M6 — Polish.** Error banners, opacity slider, click-through toggle, aircraft picker.
7. **M7 — DCS export (optional).** Lua export script + UDP listener, auto aircraft detection.
8. **M8 — Packaging.** Zip artifact, README, sample checklists, model download script.

## 17. Open questions / decisions to revisit

- Markdown checklist format details (§9).
- Default PTT binding — `RightCtrl` collides with some DCS defaults; revisit once tested.
- Should completed items auto-uncomplete on checklist reload, or stay sticky per session?
- Auto-read next item after `Next`: on by default, or opt-in?
- Output audio device selection — single default, or allow separate device for TTS so it can go to a different headset?
- Localization — assumed English-only for now; the trait surface doesn't preclude it but UI strings are not externalized in v1.
