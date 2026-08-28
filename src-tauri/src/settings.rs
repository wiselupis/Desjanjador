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
    /// The single most recent validated exit — kept for backward compatibility with
    /// older settings files; superseded by `last_exits`.
    #[serde(default)]
    pub last_exit: Option<ExitInfo>,
    /// The top validated exits (best reliability first) from the last session, re-tried
    /// IN PARALLEL on next launch for a fast start + a pre-warmed pool.
    #[serde(default)]
    pub last_exits: Vec<ExitInfo>,
    /// Also route Discord's REST API (unblocks age-restricted channels). Off by
    /// default — it adds latency to Discord actions.
    #[serde(default)]
    pub proxy_api: bool,
    /// Opt-in: also try a local Tor daemon as a fallback exit. Off by default.
    #[serde(default)]
    pub use_tor: bool,
}

/// App-specific settings filename (unique, so it never collides with another app's
/// generic `settings.json`). `LEGACY_FILE` is read once for migration from older builds.
const SETTINGS_FILE: &str = "desjanjador.settings.json";
const LEGACY_FILE: &str = "settings.json";

/// Parse one settings file, returning None if it's missing, unreadable, or not valid
/// JSON — a corrupt file is simply ignored, never fatal.
fn read_file(dir: &Path, name: &str) -> Option<Settings> {
    std::fs::read_to_string(dir.join(name))
        .ok()
        .and_then(|s| serde_json::from_str(s.trim_start_matches('\u{feff}')).ok())
}

pub fn load(dir: &Path) -> Settings {
    // Prefer the unique file; fall back to the legacy name once (migration). Any file
    // that doesn't exist or fails to parse is ignored → sane defaults.
    let mut s: Settings = read_file(dir, SETTINGS_FILE)
        .or_else(|| read_file(dir, LEGACY_FILE))
        .unwrap_or_default();
    // Migrate an old single-exit file so the warm start still works.
    if s.last_exits.is_empty() {
        if let Some(e) = s.last_exit.clone() {
            s.last_exits = vec![e];
        }
    }
    s
}

/// Write via a temp file + rename so a concurrent `load()` never sees a
/// half-written file. Caller must hold IO_LOCK.
fn write_atomic(dir: &Path, s: &Settings) {
    let _ = std::fs::create_dir_all(dir);
    let p = dir.join(SETTINGS_FILE);
    let tmp = dir.join("desjanjador.settings.json.tmp");
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

/// Remember the top validated exits (best-first) for a fast next-launch start,
/// preserving the other fields. Mirrors the first into `last_exit` for downgrade
/// safety. No-op on an empty list so we never wipe a good cache with nothing.
pub fn save_last_exits(dir: &Path, exits: Vec<ExitInfo>) {
    if exits.is_empty() {
        return;
    }
    let _g = IO_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut s = load(dir);
    s.last_exit = exits.first().cloned();
    s.last_exits = exits;
    write_atomic(dir, &s);
}

/// Update the "route the API too" flag, preserving the other fields.
pub fn save_proxy_api(dir: &Path, on: bool) {
    let _g = IO_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut s = load(dir);
    s.proxy_api = on;
    write_atomic(dir, &s);
}

/// Update the "use Tor as a fallback exit" opt-in, preserving the other fields.
pub fn save_use_tor(dir: &Path, on: bool) {
    let _g = IO_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut s = load(dir);
    s.use_tor = on;
    write_atomic(dir, &s);
}
