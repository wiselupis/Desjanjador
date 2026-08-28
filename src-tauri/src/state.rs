use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tokio::sync::{watch, Notify};

/// A validated non-BR exit the router can dial through.
#[derive(Clone, Serialize, Deserialize, Default, Debug)]
pub struct ExitInfo {
    pub addr: String,
    pub ip: String,
    pub country: String,
}

/// Snapshot sent to the React UI.
#[derive(Clone, Serialize)]
pub struct StatusDto {
    pub active: bool,
    pub autostart: bool,
    pub status: String,
    pub exit: Option<ExitInfo>,
    pub port: u16,
}

/// Shared, thread-safe application state.
pub struct Shared {
    pub port: u16,
    pub active: AtomicBool,
    /// Cached "start with Windows" state (avoids spawning schtasks on every poll).
    pub autostart: AtomicBool,
    pub exit: Mutex<Option<ExitInfo>>,
    /// A second, already-validated exit kept warm so a dead/degraded primary can
    /// be swapped instantly (make-before-break), with no gap for the user.
    pub backup: Mutex<Option<ExitInfo>>,
    pub status: Mutex<String>,
    pub stop_tx: Mutex<Option<watch::Sender<bool>>>,
    pub config_dir: Mutex<PathBuf>,
    /// Wake the pool loop to re-validate immediately (e.g. the current exit died).
    pub refresh_now: Notify,
}

impl Shared {
    pub fn new(port: u16) -> Self {
        Shared {
            port,
            active: AtomicBool::new(false),
            autostart: AtomicBool::new(false),
            exit: Mutex::new(None),
            backup: Mutex::new(None),
            status: Mutex::new("parado".into()),
            stop_tx: Mutex::new(None),
            config_dir: Mutex::new(PathBuf::new()),
            refresh_now: Notify::new(),
        }
    }

    pub fn set_status(&self, s: impl Into<String>) {
        *self.status.lock().unwrap() = s.into();
    }

    pub fn set_exit(&self, e: Option<ExitInfo>) {
        *self.exit.lock().unwrap() = e;
    }

    pub fn get_exit(&self) -> Option<ExitInfo> {
        self.exit.lock().unwrap().clone()
    }

    /// Clear the exit ONLY if it is still the one at `addr`. Prevents a slow
    /// router connection from clobbering an exit the health loop already swapped
    /// in for a fresh one.
    pub fn clear_exit_if(&self, addr: &str) {
        let mut g = self.exit.lock().unwrap();
        if matches!(g.as_ref(), Some(e) if e.addr == addr) {
            *g = None;
        }
    }

    /// Replace the exit with `new` ONLY if the current one is still `addr` (the one
    /// that just failed). Returns whether it replaced — lets the router promote the
    /// warm backup for the exact exit that died without clobbering a fresh one.
    pub fn replace_exit_if(&self, addr: &str, new: ExitInfo) -> bool {
        let mut g = self.exit.lock().unwrap();
        if matches!(g.as_ref(), Some(e) if e.addr == addr) {
            *g = Some(new);
            true
        } else {
            false
        }
    }

    /// Set the exit ONLY if still active, re-checking `active` WHILE holding the
    /// exit lock so it serializes with deactivate() (which flips active then clears
    /// the exit under this same lock). Returns whether it wrote. Prevents a
    /// rotation from repopulating the exit after Stop.
    pub fn set_exit_if_active(&self, e: ExitInfo) -> bool {
        let mut g = self.exit.lock().unwrap();
        if self.active.load(Ordering::SeqCst) {
            *g = Some(e);
            true
        } else {
            false
        }
    }

    pub fn get_backup(&self) -> Option<ExitInfo> {
        self.backup.lock().unwrap().clone()
    }

    pub fn set_backup(&self, e: Option<ExitInfo>) {
        *self.backup.lock().unwrap() = e;
    }

    /// Set the backup ONLY if still active, re-checking under the backup lock (same
    /// serialization as set_exit_if_active).
    pub fn set_backup_if_active(&self, e: ExitInfo) {
        let mut g = self.backup.lock().unwrap();
        if self.active.load(Ordering::SeqCst) {
            *g = Some(e);
        }
    }

    /// Take (remove and return) the warm backup — used to promote it to primary.
    pub fn take_backup(&self) -> Option<ExitInfo> {
        self.backup.lock().unwrap().take()
    }
}
