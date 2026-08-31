//! Dead-simple append logger to a file in the app config dir, so we can diagnose
//! what actually happened at runtime (exit found? gateway routed or direct?).
//!
//! Size-capped: the pool retries every ~20s forever, so on a machine that never finds
//! an exit the log would grow without bound. We prune it to the last half whenever it
//! passes MAX_LOG_BYTES (checked at startup and periodically), keeping recent history
//! for diagnosis while never letting it balloon.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// The log file's path, and the lock that serializes every append + prune so the
/// rewrite can never race a concurrent append.
static LOG_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);
/// Count of writes since the last size check (prune is checked every PRUNE_EVERY).
static WRITES: AtomicUsize = AtomicUsize::new(0);

/// Cap the log at ~512 KB (thousands of lines — plenty of recent history); when it
/// exceeds this we keep the last KEEP_BYTES so it settles between KEEP and MAX.
const MAX_LOG_BYTES: u64 = 512 * 1024;
const KEEP_BYTES: usize = 256 * 1024;
/// Check the file size only every N writes (a metadata() per line would be wasteful).
const PRUNE_EVERY: usize = 128;

pub fn init(dir: &Path) {
    let _ = std::fs::create_dir_all(dir);
    let path = dir.join("desjanjador.log");
    // Prune an already-large log from previous runs before appending this session.
    prune_if_big(&path);
    *LOG_PATH.lock().unwrap() = Some(path);
    log(&format!("=== session start v{} ===", env!("CARGO_PKG_VERSION")));
}

pub fn log(msg: &str) {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let line = format!("[{secs}] {msg}\n");
    eprint!("{line}");
    // Hold the lock across the append AND the periodic prune, so a prune's read+rewrite
    // never races another thread's append.
    let guard = LOG_PATH.lock().unwrap();
    if let Some(p) = guard.as_ref() {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
            let _ = f.write_all(line.as_bytes());
        }
        if WRITES.fetch_add(1, Ordering::Relaxed) % PRUNE_EVERY == 0 {
            prune_if_big(p);
        }
    }
}

/// If the log is over MAX_LOG_BYTES, rewrite it keeping only the last ~KEEP_BYTES
/// (trimmed to a line boundary), via a temp file + rename so a reader never sees a
/// half-written file. Best-effort: any I/O error just leaves the log as-is.
fn prune_if_big(path: &Path) {
    let len = match std::fs::metadata(path) {
        Ok(m) => m.len(),
        Err(_) => return,
    };
    if len <= MAX_LOG_BYTES {
        return;
    }
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(_) => return,
    };
    let cut = data.len().saturating_sub(KEEP_BYTES);
    // Advance to the start of the next full line so we never begin mid-line.
    let start = data[cut..]
        .iter()
        .position(|&b| b == b'\n')
        .map(|i| cut + i + 1)
        .unwrap_or(cut);
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, &data[start..]).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}
