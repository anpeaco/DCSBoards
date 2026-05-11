# DCS Kneeboard (DCSBoards)

## What this project is

A Windows desktop overlay app that runs alongside DCS World and provides a voice-controlled HUD-style kneeboard. The user advances aircraft checklists via push-to-talk voice commands; the app reads items aloud via TTS. All STT/TTS runs on-device — no network at runtime.

Authoritative spec: `SPEC.md`.

## Tech stack

| Concern | Choice |
|---|---|
| Language | Rust (stable) |
| UI | Slint (as per SPEC §5) |
| Overlay (Win32) | `windows` crate — `WS_EX_TOPMOST | WS_EX_LAYERED | WS_EX_TOOLWINDOW`, optional `WS_EX_TRANSPARENT` for click-through |
| Audio capture | `cpal` + `ringbuf`, resampled with `rubato` to 16 kHz mono |
| STT | `whisper-rs` (whisper.cpp), `base.en` or `small.en` |
| TTS | WinRT `Windows.Media.SpeechSynthesis` initially; Piper/Kokoro behind trait later |
| Input | `global-hotkey` (keyboard) + `gilrs` (gamepad/HOTAS, incl. FreeJoy) + `hidapi` (raw HID) |
| Page assets | PNG + sidecar JSON produced by the sibling **kneeboards generator** (see "Visual rendering" below). Overlay does **not** parse markdown directly. |
| JSON | `serde_json` (for the generator's sidecar manifests) |
| Config | TOML via `serde` + `toml` |
| Async | `tokio` (rt-multi-thread) |
| Logging | `tracing` + `tracing-subscriber` |
| Errors | `anyhow` at app boundaries, `thiserror` in library modules |

## High-level architecture

```
Slint UI  ──bindings──►  ChecklistController
   │   (Image: page-NN.png +                │
   │    overlay Rect at current bbox)       │
   │                                        │
   HotkeyMgr / InputManager ────────────────┤
   AudioCapture (cpal) ─────────────────────┤   asks TTS to read,
   SttEngine (trait)  ──────────────────────┤   routes voice→commands
   TtsEngine (trait)  ──────────────────────┤
   PageStore (loads PNG + sidecar JSON;
              notify hot-reload of output dir)
   OverlayWindow (Win32 always-on-top / layered / click-through)
   DcsExportListener (UDP, optional, deferred)
```

All inter-module messaging is owned messages over `tokio::sync::mpsc`. The UI thread never blocks on STT/TTS work.

## Repo layout (planned)

```
DCSBoards/
  SPEC.md                  # authoritative spec
  CLAUDE.md                # this file
  README.md
  Cargo.toml
  build.rs                 # if needed for Slint codegen / Win32 manifest
  src/
    main.rs
    app.rs                 # wiring
    overlay/               # Win32 glue
    audio/
      capture.rs
      stt/                 # trait + WhisperStt
      tts/                 # trait + WinRtTts (+ later PiperTts/KokoroTts)
    input/                 # InputManager + KeyboardSource, GamepadSource, HidSource
    checklist/
      model.rs             # Aircraft, Checklist, ChecklistItem (matches generator's sidecar JSON schema)
      store.rs             # loads PNG + sidecar JSON pairs from the generator's output dir; notify hot-reload
      controller.rs        # navigation state and command dispatch
    voice_router.rs        # transcript → Action
    config.rs              # TOML schema + defaults + load/save
    dcs_export.rs          # optional UDP listener (deferred)
  ui/
    main.slint               # top-level window: chrome + page Image + highlight Rect
    page_view.slint          # the page Image + scaled bbox-highlight rectangle
    chrome.slint             # mic indicator, voice picker, opacity slider, nav buttons
  devices/                 # shipped HID profiles (TOML)
  models/                  # whisper models (gitignored; downloaded separately)
  pages-sample/            # a small sample of generator output for dev / fixture tests
  tests/                   # integration tests
```

## Visual rendering — generator output + Slint highlight overlay

**The overlay does not parse markdown directly.** Instead, it consumes the static output of the sibling kneeboards generator: one rendered PNG per page (pixel-perfect, matches what gets copied into DCS's built-in kneeboard) plus a sidecar JSON describing the structured item model and per-item bounding boxes.

Slint then renders:
- The PNG via a Slint `Image` element, scaled to fit the overlay window.
- A semi-transparent highlight `Rectangle` positioned at the current item's bbox, scaled by the same factor as the image.

This gives us **pixel-perfect visual parity** with the generator at zero CSS-replication cost, while keeping Slint as the only UI framework.

### Generator-side enhancement (work item in the sibling repo)
The kneeboards generator at `C:\Users\anpea\OneDrive\Documents\DevProjects\kneeboards` currently emits PNGs and a combined PDF. We need to extend it to also emit, per page, a sidecar JSON file. Proposed schema:

```jsonc
// output/F-16C_50/page-03.json
{
  "page_index": 3,
  "title": "AGM-65 Maverick",
  "image": "page-03.png",
  "image_size": [1358, 2037],
  "items": [
    {
      "idx": 0,
      "group": "AGM-65D/G IR PRE",
      "kind": "step",                  // "step" | "note-info" | "note-check"
                                       // | "note-optional" | "note-caution"
                                       // | "note-warning" | "note-radio"
                                       // | "branch-header" | "section-header"
                                       // | "table" | "notes-block"
      "text": "FCR switch | FCR provides target ranging ... ON",
      "spoken": null,                  // optional override; null = derive from text
      "navigable": true,               // false for headings, NOTES content, radio calls
      "bbox": [x, y, w, h]             // pixel coords in image_size space
    },
    ...
  ]
}
```

Implementation in the generator: after Puppeteer screenshots each `.page` element, run a second `page.evaluate()` that walks the rendered DOM, reads each step/note's `getBoundingClientRect()`, and writes the sidecar JSON. ~50 lines of changes in `lib/renderer.js`.

### Overlay-side rendering
- Load PNG into a Slint `Image`. Track displayed image rect (offset + scale) so we can transform bbox coordinates.
- For the current `(page_idx, item_idx)`, look up the item's bbox in the sidecar JSON. Compute `display_bbox = bbox * scale + image_offset`. Position a Slint `Rectangle { border-color, opacity }` at `display_bbox`.
- For "auto-scroll to current item": position the image's viewport so `display_bbox` is centered (or anchored near the top with margin). Slint's `Flickable` covers this.

### Navigation model
The sidecar JSON's `items[]` array, concatenated across pages within the active aircraft's checklist, is the navigation order. `Next`/`Previous` walk `idx` (skipping `navigable: false` entries). When `Next` crosses a page boundary, swap the displayed PNG.

### TTS source text
TTS reads `item.spoken ?? derive(item.text)`. `derive()` is a small Rust function: strip `|` context from step text, normalize `...` to a comma pause, expand common abbreviations (`AGM` → "Aim-Gee-Em", `FCR` → "F-C-R", etc.). The abbreviation table lives in `config.toml` so the user can tune it.

### Source-of-truth split
- The kneeboards generator owns: markdown parsing, page rendering, item indexing.
- The overlay owns: navigation state, voice input, TTS, overlay window management.
- They share: the sidecar JSON schema. Treat it as the contract between the two projects. Version the schema (`"schema_version": "1.0"` field) so the overlay can detect mismatches.

### Hot-reload
- The overlay watches the generator's `output/<aircraft>/` directory via `notify`.
- When a PNG + JSON pair changes, reload them; if the current item still exists in the new JSON, preserve it; else clamp to nearest.
- Authoring loop: user edits markdown → runs `node build.js <aircraft>` (or `node build.js preview` for live reload from the generator side) → overlay reloads automatically.

### What about the markdown grammar?
We don't need to parse it in the overlay. But for reference, the grammar is fully documented in the sibling repo's `CLAUDE.md` / `DOCS.md` (`>`, `+>`, `?>`, `!>`, `!!>`, `@>`, `i>`, `~>` line prefixes; `N. CONTROL | context ... STATE` step format; `---` page separators; `## NOTES` sections). When you're adding new fields to the sidecar JSON, check those docs first to make sure you understand what each line type means.

### Spoken overrides
Currently no per-step `spoken` field in the markdown. When we add one, it gets parsed in the generator and emitted in the sidecar JSON as `item.spoken`. Candidate syntax: a footnote-style `>~ spoken: ...` line after the step. Deferred until TTS quality testing reveals where literal text falls down.

## Input binding — key points

- Actions: `PushToTalk`, `Next`, `Previous`, `Repeat`, `ReadCurrent`, `ToggleClickThrough`, `ToggleVisibility`, `LoadList(name)`, `Cancel`.
- Triggers: keyboard combos, gamepad button/axis/hat (via `gilrs`, covers FreeJoy/HOTAS/button boxes), raw HID button (via `hidapi` + per-device TOML profile).
- PTT is edge-sensitive (press starts capture, release submits to STT). Everything else is press-edge by default.
- Multiple bindings per action allowed. Device identity persisted by `gilrs` GUID, not connection order.
- Bind UX in v1: **capture mode only** — "press the button you want to bind." No button-grid pickers (impractical for 64+ button FreeJoy boards).
- Capture-mode filter: ignore axes that haven't moved ≥70% of range from rest.

## Defaults

| Action | Default |
|---|---|
| PushToTalk | `RightCtrl` (revisit — may collide with DCS) |
| ToggleClickThrough | `Ctrl+Alt+K` |
| ToggleVisibility | `Ctrl+Alt+H` |
| Next/Previous/Repeat/ReadCurrent | Unbound (user binds to HOTAS) |

## Performance budgets

- Idle CPU < 1%; STT latency for 2 s utterance < 500 ms on CPU with `base.en`; TTS time-to-first-audio < 300 ms; memory < 400 MB resident with whisper model loaded; 60 FPS Slint when visible, throttled when no input.

## Milestones (from SPEC §16)

1. **M1 Skeleton** — Slint window with always-on-top + layered flags. Loads a hand-authored sample sidecar JSON (`pages-sample/page-03.json`) and PNG, shows the page Image, draws the highlight Rectangle at item 0's bbox. On-screen Next/Prev buttons walk `items[]`.
2. **M2 Generator sidecar emitter** — Land the small change in the sibling kneeboards repo: emit `page-NN.json` alongside each PNG. Use real generator output to drive the M1 skeleton.
3. **M3 TTS** — `TtsEngine` trait + `WinRtTts` impl. Read button speaks `item.spoken ?? derive(item.text)`.
4. **M4 STT + PTT** — `SttEngine` + Whisper, `cpal` capture, PTT hotkey, voice router. Logs transcripts.
5. **M5 PageStore + hot-reload** — Watch the generator's `output/<aircraft>/` dir, reload PNG+JSON pairs on change.
6. **M6 Config + persistence** — `config.toml`, window pos, voice/rate, hotkey rebinding UI (capture mode).
7. **M7 Polish** — Error banners, opacity slider, click-through toggle, aircraft picker, auto-scroll on cursor change.
8. **M8 DCS export (optional)** — `Export.lua` + UDP listener, auto aircraft detection.
9. **M9 Packaging** — Zip artifact, README, sample pages, whisper model download script.

## Dev workflow

- `cargo run` for dev. Slint `.slint` files hot-reload via `slint-build`.
- `cargo clippy -- -D warnings`, `cargo fmt`.
- Tests:
  - Voice router: string → command mapping.
  - Sidecar JSON loader: fixture JSON → in-memory model; bbox parsing; `navigable: false` skipping.
  - `ChecklistController` state transitions (pure model, no UI).
- CI: GitHub Actions, Windows runner, build + test + clippy.

## Error & resilience expectations

- Missing whisper model → banner, UI nav still works.
- TTS init failure → log, mute icon, app stays usable.
- Audio device disappears → catch `cpal` errors, banner, reopen on next PTT.
- Missing or malformed sidecar JSON → log path, skip that page; if no pages load, banner "No kneeboards found at <path>. Run the generator?".
- Schema version mismatch in sidecar JSON → banner asking the user to update the generator or the overlay.
- PNG missing while JSON is present (or vice versa) → log and skip that page; UI shows a gap with the page title.
- Hotkey registration conflict → surface to user, fall back to UI buttons.

## Out of scope (v1)

macOS/Linux, cloud STT/TTS, multiplayer/telemetry, in-app checklist editing, touch UI, modifying DCS, installer, code signing.

## Related project

`C:\Users\anpea\OneDrive\Documents\DevProjects\kneeboards` — the existing Node.js/Puppeteer build-time generator. Reads markdown, renders pages via Chromium, emits PNG (+ PDF) for DCS's built-in kneeboard system. **Do not merge** — different languages and runtimes.

**Coupling between the two projects:** the overlay consumes the generator's `output/<aircraft>/` directory directly — both the PNGs and the new per-page sidecar JSON (see "Visual rendering" above). The sidecar JSON schema is the formal contract between them; version it.

**Work item in the sibling repo (M2):** extend `lib/renderer.js` to emit `page-NN.json` alongside each PNG. After the screenshot pass, run a second `page.evaluate()` that walks the rendered DOM, captures per-item bounding boxes + structured fields, and writes the JSON. The existing markdown parser in `templates/kneeboard.js` already classifies items by prefix — the sidecar emitter just reads back what was rendered.
