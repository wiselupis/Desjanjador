//! Tiny JSON settings persistence: the Active toggle plus the last working exit,
//! so a restart can re-try that exit instantly (concurrently) instead of
//! re-searching from scratch. Lives in the app config dir next to the log.

use crate::state::ExitInfo;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Mutex;

/// Serializes every read-modify-write of settings.json. Two threads persist here
/// — the Active toggle (command thread) and the last-exit save (pool thread) —
/// so without this a concurrent toggle-off during a discovery could lose the
/// user's OFF (each loads, mutates its own field, writes back the other's stale
/// value). The lock makes the load→mutate→write atomic across writers.
static IO_LOCK: Mutex<()> = Mutex::new(());

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Settings {
    pub active: bool,
    /// The most recent validated exit, re-tried on next launch for a fast start.
    #[serde(default)]
    pub last_exit: Option<ExitInfo>,
}

pub fn load(dir: &Path) -> Settings {
    let p = dir.join("settings.json");
    std::fs::read_to_string(p)
        .ok()
        .and_then(|s| serde_json::from_str(s.trim_start_matches('\u{feff}')).ok())
        .unwrap_or_default()
}

/// Write via a temp file + rename so a concurrent `load()` never sees a
/// half-written file. Caller must hold IO_LOCK.
fn write_atomic(dir: &Path, s: &Settings) {
    let _ = std::fs::create_dir_all(dir);
    let p = dir.join("settings.json");
    let tmp = dir.join("settings.json.tmp");
    if let Ok(json) = serde_json::to_string_pretty(s) {
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
    }
}

/// Update just the Active flag, preserving the cached last exit.
pub fn save_active(dir: &Path, active: bool) {
    let _g = IO_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut s = load(dir);
    s.active = active;
    write_atomic(dir, &s);
}

/// Remember (or clear) the last working exit, preserving the Active flag.
pub fn save_last_exit(dir: &Path, exit: Option<ExitInfo>) {
    let _g = IO_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut s = load(dir);
    s.last_exit = exit;
    write_atomic(dir, &s);
}
