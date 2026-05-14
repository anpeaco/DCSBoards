//! gilrs-backed gamepad / HOTAS listener.
//!
//! Runs gilrs on a dedicated thread so a stuck pad driver can't stall the UI.
//! Button-press events are forwarded as `Trigger::Gamepad` values over an
//! mpsc channel; the UI thread drains the channel via a Slint timer.

use crate::input::{InputEvent, Trigger};
use anyhow::Result;
use gilrs::{EventType, Gilrs};
use std::collections::HashMap;
use std::sync::mpsc::Sender;
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Duration;

/// Process-wide guid → resolved-name cache. `Some(name)` is a hit;
/// `None` is "we tried joy.cpl and hidapi and got nothing — don't retry".
/// Negative caching matters because `HidApi::new()` enumerates every HID
/// device on the system (slow), and `refresh_bindings_ui` runs on the UI
/// thread for every row at startup. Without it, an empty cache would
/// hang the window for seconds while every binding row redid the work.
fn registry() -> &'static Mutex<HashMap<String, Option<String>>> {
    static R: OnceLock<Mutex<HashMap<String, Option<String>>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Look up the friendly name for a gamepad guid. First checks the
/// session cache (populated by Connected events); on miss, parses vid/pid
/// out of the SDL guid bytes and queries Windows joy.cpl + hidapi directly
/// so names still resolve for devices that aren't currently plugged in.
///
/// The resolution outcome is always written back, including negatives, so
/// the slow path runs at most once per unique guid per session.
pub fn device_name(guid: &str) -> Option<String> {
    {
        let map = registry().lock().ok()?;
        if let Some(cached) = map.get(guid) {
            return cached.clone();
        }
    }
    let resolved = match sdl_guid_vid_pid(guid) {
        Some((vid, pid)) => {
            let name = resolve_name_from_vid_pid(vid, pid);
            eprintln!(
                "[gamepad] resolve guid={} vid={:#06x} pid={:#06x} → {:?}",
                guid, vid, pid, name
            );
            name
        }
        None => {
            eprintln!("[gamepad] resolve guid={} → unparseable", guid);
            None
        }
    };
    cache_resolution(guid.to_string(), resolved.clone());
    resolved
}

/// SDL2 game-controller guid format: 16 bytes encoded as 32 hex chars.
/// Bytes 4-5 are the vendor id, bytes 8-9 are the product id, both little-
/// endian. (Byte 0 is the bus type — 0x03 for HID.) Returns None for any
/// guid that doesn't fit that layout, e.g. xinput-style guids on Linux.
fn sdl_guid_vid_pid(guid: &str) -> Option<(u16, u16)> {
    if guid.len() != 32 { return None; }
    let byte = |i: usize| u8::from_str_radix(&guid[i * 2..i * 2 + 2], 16).ok();
    let vid = u16::from_le_bytes([byte(4)?, byte(5)?]);
    let pid = u16::from_le_bytes([byte(8)?, byte(9)?]);
    if vid == 0 && pid == 0 { return None; }
    Some((vid, pid))
}

/// Try the same lookups as `friendly_name(&gp)` but without a gilrs handle:
/// joy.cpl OEMName, then hidapi product string. Returns None if neither
/// path yields a non-empty string (e.g. device truly unknown to Windows).
fn resolve_name_from_vid_pid(vid: u16, pid: u16) -> Option<String> {
    #[cfg(windows)]
    if let Some(name) = joy_cpl_oem_name(vid, pid) {
        if !name.trim().is_empty() { return Some(name); }
    }
    if let Some(name) = hid_product_string(vid, pid) {
        if !name.trim().is_empty() { return Some(name); }
    }
    None
}

fn remember(guid: String, name: String) {
    cache_resolution(guid, Some(name));
}

/// Insert a resolution outcome into the cache. Positive entries are also
/// flushed to disk so cached names survive across runs even when the
/// device is unplugged — joy.cpl `OEMName` entries are written
/// opportunistically by Windows and many generic HID joysticks never
/// get one.
fn cache_resolution(guid: String, name: Option<String>) {
    let positive = name.is_some();
    if let Ok(mut map) = registry().lock() {
        map.insert(guid, name);
    }
    if positive {
        save_persisted_names();
    }
}

fn persisted_names_path() -> std::path::PathBuf {
    std::path::PathBuf::from("device_names.toml")
}

/// Load previously-resolved names from disk into the registry. Called
/// once at startup before the bindings UI renders so cached entries
/// short-circuit the slow joy.cpl + hidapi resolve path.
pub fn load_persisted_names() {
    let path = persisted_names_path();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return; // missing file is the common first-run case, not an error
    };
    let parsed: Result<HashMap<String, String>, _> = toml::from_str(&text);
    match parsed {
        Ok(map) => {
            let n = map.len();
            if let Ok(mut reg) = registry().lock() {
                for (guid, name) in map {
                    reg.insert(guid, Some(name));
                }
            }
            eprintln!(
                "[gamepad] loaded {n} persisted device names from {}",
                path.display()
            );
        }
        Err(e) => eprintln!("[gamepad] {} parse failed: {e}", path.display()),
    }
}

/// Snapshot every positive entry in the registry and write it to disk.
/// Called whenever a new name is cached. The file is small (handful of
/// short strings) so rewriting whole each time keeps the code simple.
fn save_persisted_names() {
    let map: HashMap<String, String> = match registry().lock() {
        Ok(reg) => reg
            .iter()
            .filter_map(|(g, n)| n.as_ref().map(|name| (g.clone(), name.clone())))
            .collect(),
        Err(_) => return,
    };
    let Ok(text) = toml::to_string_pretty(&map) else { return };
    if let Err(e) = std::fs::write(persisted_names_path(), text) {
        eprintln!("[gamepad] failed to save device_names.toml: {e}");
    }
}

/// Spawn the gamepad poller. Returns Err if gilrs can't initialise (e.g. no
/// input subsystem available); the app stays usable on keyboard only.
pub fn spawn(tx: Sender<InputEvent>) -> Result<()> {
    let mut gilrs = Gilrs::new().map_err(|e| anyhow::anyhow!("{e}"))?;
    eprintln!("[gamepad] gilrs initialised");
    let mut any = false;
    for (_id, gp) in gilrs.gamepads() {
        let guid = fmt_uuid(gp.uuid());
        eprintln!(
            "[gamepad] connected at startup: name={:?} os_name={:?} vid={:?} pid={:?} guid={}",
            gp.name(),
            gp.os_name(),
            gp.vendor_id(),
            gp.product_id(),
            guid
        );
        remember(guid, friendly_name(&gp));
        any = true;
    }
    if any {
        // Tell the UI to re-render the bindings panel with resolved names.
        let _ = tx.send(InputEvent::DevicesChanged);
    }
    thread::Builder::new()
        .name("gamepad".into())
        .spawn(move || run(&mut gilrs, tx))?;
    Ok(())
}

fn run(gilrs: &mut Gilrs, tx: Sender<InputEvent>) {
    loop {
        while let Some(ev) = gilrs.next_event() {
            match ev.event {
                EventType::Connected => {
                    let gp = gilrs.gamepad(ev.id);
                    let guid = fmt_uuid(gp.uuid());
                    eprintln!(
                        "[gamepad] connected: name={:?} os_name={:?} vid={:?} pid={:?} guid={}",
                        gp.name(),
                        gp.os_name(),
                        gp.vendor_id(),
                        gp.product_id(),
                        guid
                    );
                    remember(guid, friendly_name(&gp));
                    if tx.send(InputEvent::DevicesChanged).is_err() {
                        return;
                    }
                }
                EventType::Disconnected => {
                    eprintln!("[gamepad] disconnected: id={:?}", ev.id);
                }
                EventType::ButtonPressed(button, code) => {
                    let gp = gilrs.gamepad(ev.id);
                    let trigger = Trigger::Gamepad {
                        guid: fmt_uuid(gp.uuid()),
                        code: format!("{code}"),
                    };
                    eprintln!("[gamepad] press: {} -> {:?} {}", gp.name(), button, trigger.display());
                    if tx.send(InputEvent::Press(trigger)).is_err() {
                        return; // UI side hung up — exit thread.
                    }
                }
                EventType::ButtonReleased(button, code) => {
                    let gp = gilrs.gamepad(ev.id);
                    let trigger = Trigger::Gamepad {
                        guid: fmt_uuid(gp.uuid()),
                        code: format!("{code}"),
                    };
                    eprintln!("[gamepad] release: {} -> {:?} {}", gp.name(), button, trigger.display());
                    if tx.send(InputEvent::Release(trigger)).is_err() {
                        return;
                    }
                }
                _ => {}
            }
        }
        thread::sleep(Duration::from_millis(8));
    }
}

/// Best-effort human-readable name for a gilrs gamepad. gilrs's own `name()`
/// on Windows DirectInput devices returns the generic class string ("HID-
/// compliant Game Controller") rather than the actual product. We try, in
/// order: the Windows joy.cpl `OEMName` registry entry (what the Game
/// Controllers control panel shows — e.g. "F16 MFD 3"), then the USB HID
/// product string, then gilrs's own name as a last resort.
fn friendly_name(gp: &gilrs::Gamepad<'_>) -> String {
    let fallback = || gp.name().to_string();
    let Some(vid) = gp.vendor_id() else { return fallback() };
    let Some(pid) = gp.product_id() else { return fallback() };
    resolve_name_from_vid_pid(vid, pid).unwrap_or_else(fallback)
}

/// Enumerate HID devices and return the product string for the first match
/// on (vid, pid). Returns None if hidapi can't initialise or no device matches.
fn hid_product_string(vid: u16, pid: u16) -> Option<String> {
    let api = hidapi::HidApi::new().ok()?;
    for info in api.device_list() {
        if info.vendor_id() == vid && info.product_id() == pid {
            if let Some(s) = info.product_string() {
                return Some(s.to_string());
            }
        }
    }
    None
}

/// Read the per-device name stored by Windows for joy.cpl. This is the same
/// string the "Game Controllers" control panel lists (e.g. "VPC Stick
/// MT-50CM3", "F16 MFD 3") and is what users recognise. Lives at:
///   HKLM\SYSTEM\CurrentControlSet\Control\MediaProperties\
///       PrivateProperties\Joystick\OEM\VID_xxxx&PID_yyyy
/// value name: `OEMName` (REG_SZ).
#[cfg(windows)]
fn joy_cpl_oem_name(vid: u16, pid: u16) -> Option<String> {
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegGetValueW, RegOpenKeyExW, HKEY, HKEY_LOCAL_MACHINE, KEY_READ,
        RRF_RT_REG_SZ,
    };

    let subkey = format!(
        "SYSTEM\\CurrentControlSet\\Control\\MediaProperties\\PrivateProperties\\Joystick\\OEM\\VID_{:04X}&PID_{:04X}",
        vid, pid
    );
    let subkey_w: Vec<u16> = subkey.encode_utf16().chain(std::iter::once(0)).collect();

    let mut hkey = HKEY::default();
    // SAFETY: subkey_w is a valid null-terminated UTF-16 buffer outliving the call.
    let open = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(subkey_w.as_ptr()),
            0,
            KEY_READ,
            &mut hkey,
        )
    };
    if open != ERROR_SUCCESS {
        return None;
    }

    // First call: query required byte length.
    let mut len: u32 = 0;
    // SAFETY: hkey was just opened; we pass nulls for the buffer to query size only.
    let probe = unsafe {
        RegGetValueW(
            hkey,
            PCWSTR::null(),
            w!("OEMName"),
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut len),
        )
    };
    if probe != ERROR_SUCCESS || len == 0 {
        unsafe { let _ = RegCloseKey(hkey); }
        return None;
    }

    let mut buf = vec![0u8; len as usize];
    let mut got = len;
    // SAFETY: buf has capacity `len` bytes; got tracks the in/out size.
    let read = unsafe {
        RegGetValueW(
            hkey,
            PCWSTR::null(),
            w!("OEMName"),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut got),
        )
    };
    unsafe { let _ = RegCloseKey(hkey); }
    if read != ERROR_SUCCESS {
        return None;
    }

    // Buffer is UTF-16LE bytes; drop the trailing NUL and any odd byte.
    let wide_len = (got as usize) / 2;
    let wide: Vec<u16> = (0..wide_len)
        .map(|i| u16::from_le_bytes([buf[i * 2], buf[i * 2 + 1]]))
        .take_while(|&c| c != 0)
        .collect();
    Some(String::from_utf16_lossy(&wide))
}

/// gilrs returns a 16-byte device UUID as a raw `[u8; 16]`. Render it as a
/// 32-char lowercase hex string so it round-trips through TOML cleanly and
/// matches what `uuid::Uuid::simple()` would produce.
fn fmt_uuid(bytes: [u8; 16]) -> String {
    let mut s = String::with_capacity(32);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sdl_guid_parses_vid_pid_little_endian() {
        // SDL2-format guid for a Thrustmaster device: vid bytes are at
        // positions 4-5, pid bytes at 8-9, both little-endian.
        let (vid, pid) = sdl_guid_vid_pid("030000004f04000053b3000000000000").unwrap();
        assert_eq!(vid, 0x044f);
        assert_eq!(pid, 0xb353);
    }

    #[test]
    fn sdl_guid_rejects_malformed() {
        assert!(sdl_guid_vid_pid("nope").is_none());
        assert!(sdl_guid_vid_pid("").is_none());
        // All-zero vid+pid is treated as unparseable (xinput placeholders).
        assert!(sdl_guid_vid_pid("03000000000000000000000000000000").is_none());
    }
}
