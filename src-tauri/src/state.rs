use serde::Serialize;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;
use tokio::sync::{watch, Notify};

/// A validated non-BR exit the router can dial through.
#[derive(Clone, Serialize, Default, Debug)]
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
    pub exit: Mutex<Option<ExitInfo>>,
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
            exit: Mutex::new(None),
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
}
