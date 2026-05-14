//! Filesystem watcher for hot-reloading page sources.
//!
//! Wraps `notify-debouncer-mini` so we can watch the active tab's source
//! directory and emit a single "reload" signal per burst of writes. The
//! watcher swaps its target whenever the active tab changes — we never run
//! more than one watcher at a time.

use anyhow::Result;
use notify::RecursiveMode;
use notify_debouncer_mini::{new_debouncer, DebounceEventResult, Debouncer};
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::time::Duration;

/// Owns the active debouncer. Replacing this field tears the previous
/// watcher down. The Sender survives across swaps so the UI side only needs
/// one channel.
pub struct Watcher {
    debouncer: Option<Debouncer<notify::RecommendedWatcher>>,
    pub watched_path: Option<PathBuf>,
}

impl Watcher {
    pub fn new() -> Self {
        Self {
            debouncer: None,
            watched_path: None,
        }
    }

    /// Point the watcher at `path` (or stop watching if `path` is None).
    /// Idempotent — calling with the same path is a no-op.
    pub fn watch(&mut self, path: Option<PathBuf>, tx: Sender<PathBuf>) -> Result<()> {
        if self.watched_path == path {
            return Ok(());
        }
        // Drop the old debouncer first so the OS handle is released.
        self.debouncer = None;
        self.watched_path = None;

        let Some(p) = path else {
            eprintln!("[watch] stopped");
            return Ok(());
        };
        if !p.exists() {
            eprintln!("[watch] target does not exist: {}", p.display());
            return Ok(());
        }

        let watch_root = p.clone();
        let mut debouncer = new_debouncer(
            Duration::from_millis(250),
            move |res: DebounceEventResult| {
                // Any event for this path coalesces into a single signal:
                // the consumer just re-reads the directory.
                match res {
                    Ok(events) if !events.is_empty() => {
                        let _ = tx.send(watch_root.clone());
                    }
                    Ok(_) => {}
                    Err(e) => {
                        eprintln!("[watch] error: {e:?}");
                    }
                }
            },
        )?;
        debouncer
            .watcher()
            .watch(&p, RecursiveMode::NonRecursive)?;
        eprintln!("[watch] watching {}", p.display());
        self.debouncer = Some(debouncer);
        self.watched_path = Some(p);
        Ok(())
    }
}
