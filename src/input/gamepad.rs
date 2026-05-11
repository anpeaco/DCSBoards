//! gilrs-backed gamepad / HOTAS listener.
//!
//! Runs gilrs on a dedicated thread so a stuck pad driver can't stall the UI.
//! Button-press events are forwarded as `Trigger::Gamepad` values over an
//! mpsc channel; the UI thread drains the channel via a Slint timer.

use crate::input::{InputEvent, Trigger};
use anyhow::Result;
use gilrs::{EventType, Gilrs};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

/// Spawn the gamepad poller. Returns Err if gilrs can't initialise (e.g. no
/// input subsystem available); the app stays usable on keyboard only.
pub fn spawn(tx: Sender<InputEvent>) -> Result<()> {
    let mut gilrs = Gilrs::new().map_err(|e| anyhow::anyhow!("{e}"))?;
    eprintln!("[gamepad] gilrs initialised");
    for (_id, gp) in gilrs.gamepads() {
        eprintln!(
            "[gamepad] connected at startup: {} ({})",
            gp.name(),
            fmt_uuid(gp.uuid())
        );
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
                    eprintln!("[gamepad] connected: {} ({})", gp.name(), fmt_uuid(gp.uuid()));
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
