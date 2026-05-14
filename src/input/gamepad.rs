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

/// Process-wide guid → friendly device name cache. Populated by the gamepad
/// thread on Connected events (and at startup); read by `Trigger::display()`
/// so the bindings UI can show "F16 ICP" instead of a 32-char GUID.
fn registry() -> &'static Mutex<HashMap<String, String>> {
    static R: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Look up the friendly name for a gamepad guid, if it's been seen this
/// session. Returns None for unknown devices (disconnected before startup,
/// or gilrs failed to initialise) — callers fall back to the raw guid.
pub fn device_name(guid: &str) -> Option<String> {
    registry().lock().ok()?.get(guid).cloned()
}

fn remember(guid: String, name: String) {
    if let Ok(mut map) = registry().lock() {
        map.insert(guid, name);
    }
}

/// Spawn the gamepad poller. Returns Err if gilrs can't initialise (e.g. no
/// input subsystem available); the app stays usable on keyboard only.
pub fn spawn(tx: Sender<InputEvent>) -> Result<()> {
    let mut gilrs = Gilrs::new().map_err(|e| anyhow::anyhow!("{e}"))?;
    eprintln!("[gamepad] gilrs initialised");
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
                    eprintln!("[gamepad] connected: {} ({})", gp.name(), guid);
                    remember(guid, friendly_name(&gp));
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

    #[cfg(windows)]
    if let Some(name) = joy_cpl_oem_name(vid, pid) {
        if !name.trim().is_empty() {
            return name;
        }
    }

    if let Some(name) = hid_product_string(vid, pid) {
        if !name.trim().is_empty() {
            return name;
        }
    }

    fallback()
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
