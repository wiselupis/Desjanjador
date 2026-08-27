//! Tiny JSON settings persistence (just the Active toggle for now; the
//! Start-with-Windows state is owned by the autostart plugin / OS).

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Settings {
    pub active: bool,
}

pub fn load(dir: &Path) -> Settings {
    let p = dir.join("settings.json");
    std::fs::read_to_string(p)
        .ok()
        .and_then(|s| serde_json::from_str(s.trim_start_matches('\u{feff}')).ok())
        .unwrap_or_default()
}

pub fn save(dir: &Path, s: &Settings) {
    let _ = std::fs::create_dir_all(dir);
    let p = dir.join("settings.json");
    if let Ok(json) = serde_json::to_string_pretty(s) {
        let _ = std::fs::write(p, json);
    }
}
