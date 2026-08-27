//! Dead-simple append logger to a file in the app config dir, so we can diagnose
//! what actually happened at runtime (exit found? gateway routed or direct?).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

static LOG_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);

pub fn init(dir: &Path) {
    let _ = std::fs::create_dir_all(dir);
    *LOG_PATH.lock().unwrap() = Some(dir.join("desjanjador.log"));
    log("=== session start ===");
}

pub fn log(msg: &str) {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let line = format!("[{secs}] {msg}\n");
    eprint!("{line}");
    if let Some(p) = LOG_PATH.lock().unwrap().clone() {
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&p)
        {
            let _ = f.write_all(line.as_bytes());
        }
    }
}
