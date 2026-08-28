use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tokio::sync::{watch, Notify};

/// Fixed SOCKS5 credentials used ONLY for a local Tor proxy (addr 127.0.0.1:*).
/// Supplying the same auth on the validate/probe and the gateway streams engages
/// Tor's IsolateSOCKSAuth so they share one circuit — otherwise Tor could carry the
/// gateway through a different (possibly disallowed) country than we validated.
pub const TOR_USER: &str = "desjanjador";
pub const TOR_PASS: &str = "dj";

/// How many warm, pre-validated backup exits to keep ready — a small pool so a
/// rotation is always instant to a proven, low-latency exit, and so a couple can die
/// between cycles without ever emptying it.
pub const BACKUP_TARGET: usize = 3;

/// A validated exit (allowlisted country, reaches Discord's gateway) the router
/// can dial through.
#[derive(Clone, Serialize, Deserialize, Default, Debug)]
pub struct ExitInfo {
    pub addr: String,
    pub ip: String,
    pub country: String,
    /// Measured Discord-gateway connect latency (ms) at validation/probe time —
    /// used to promote the FASTEST backup on rotation.
    #[serde(default)]
    pub latency_ms: u32,
}

/// Snapshot sent to the React UI.
#[derive(Clone, Serialize)]
pub struct StatusDto {
    pub active: bool,
    pub autostart: bool,
    pub proxy_api: bool,
    pub use_tor: bool,
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
    /// Also route Discord's REST API through the exit (unblocks age-restricted
    /// channels' ID check). Off by default — it adds latency to Discord actions.
    pub proxy_api: AtomicBool,
    /// Opt-in: also try a locally-running Tor daemon as an exit. Off by default —
    /// Tor can't be pinned to an allowed country across circuit refreshes, so it's a
    /// user-chosen fallback, never automatic.
    pub use_tor: AtomicBool,
    pub exit: Mutex<Option<ExitInfo>>,
    /// A small pool of already-validated, continuously-pinged exits kept warm, so a
    /// dead/degraded primary is swapped INSTANTLY to the fastest one (no gap). Kept
    /// full at BACKUP_TARGET by the health loop.
    pub backups: Mutex<Vec<ExitInfo>>,
    pub status: Mutex<String>,
    pub stop_tx: Mutex<Option<watch::Sender<bool>>>,
    pub config_dir: Mutex<PathBuf>,
    /// Wake the pool loop to re-validate immediately (e.g. the current exit died).
    pub refresh_now: Notify,
    /// One-shot exclusion set for the NEXT discovery: the exits the user just asked to
    /// drop (refresh icon), so we don't hand back the same one(s).
    pub refresh_avoid: Mutex<Vec<String>>,
}

impl Shared {
    pub fn new(port: u16) -> Self {
        Shared {
            port,
            active: AtomicBool::new(false),
            autostart: AtomicBool::new(false),
            proxy_api: AtomicBool::new(false),
            use_tor: AtomicBool::new(false),
            exit: Mutex::new(None),
            backups: Mutex::new(Vec::new()),
            status: Mutex::new("parado".into()),
            stop_tx: Mutex::new(None),
            config_dir: Mutex::new(PathBuf::new()),
            refresh_now: Notify::new(),
            refresh_avoid: Mutex::new(Vec::new()),
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

    /// Fold a fresh gateway-latency sample into the PRIMARY's smoothed latency index
    /// (EWMA, α=0.5), only if it's still the exit at `addr`. Gives the UI a live
    /// number and feeds the proactive-upgrade decision.
    pub fn set_exit_latency(&self, addr: &str, ms: u32) {
        if let Some(e) = self.exit.lock().unwrap().as_mut() {
            if e.addr == addr {
                e.latency_ms = if e.latency_ms == 0 {
                    ms
                } else {
                    (e.latency_ms / 2) + (ms / 2)
                };
            }
        }
    }

    /// Install `new` as the exit ONLY if still active AND the current exit is either
    /// empty or still the dead `addr` — i.e. no other task (the router) already
    /// promoted a DIFFERENT fresh exit. Returns whether it wrote. Lets the health-loop
    /// rotation promote a backup without clobbering a primary another path just
    /// published (the caller returns the backup to the pool on false).
    pub fn replace_dead_exit(&self, addr: &str, new: ExitInfo) -> bool {
        let mut g = self.exit.lock().unwrap();
        if !self.active.load(Ordering::SeqCst) {
            return false;
        }
        match g.as_ref() {
            None => {
                *g = Some(new);
                true
            }
            Some(e) if e.addr == addr => {
                *g = Some(new);
                true
            }
            _ => false, // a different fresh exit is present — don't clobber it
        }
    }

    /// Snapshot of the warm backup pool (for health-probing each one).
    pub fn get_backups(&self) -> Vec<ExitInfo> {
        self.backups.lock().unwrap().clone()
    }

    pub fn backups_len(&self) -> usize {
        self.backups.lock().unwrap().len()
    }

    pub fn clear_backups(&self) {
        self.backups.lock().unwrap().clear();
    }

    /// Add a warm backup — only if still active, there's room (< BACKUP_TARGET), and
    /// it duplicates neither the primary nor an existing backup.
    pub fn push_backup_if_active(&self, e: ExitInfo) {
        // Read the primary addr first (exit lock released before the backups lock,
        // keeping a consistent exit-before-backups lock order everywhere).
        let primary = self.exit.lock().unwrap().as_ref().map(|p| p.addr.clone());
        let mut g = self.backups.lock().unwrap();
        // Re-check `active` WHILE holding the backups lock so it serializes with
        // deactivate()'s clear_backups (which flips active=false then clears under this
        // same lock): any push that slips in with active==true is cleared by the
        // following clear_backups, so a stopped session never keeps a phantom backup.
        if self.active.load(Ordering::SeqCst)
            && g.len() < BACKUP_TARGET
            && primary.as_deref() != Some(e.addr.as_str())
            && !g.iter().any(|b| b.addr == e.addr)
        {
            g.push(e);
        }
    }

    /// Drop a dead backup by addr.
    pub fn remove_backup(&self, addr: &str) {
        self.backups.lock().unwrap().retain(|b| b.addr != addr);
    }

    /// Fold a fresh gateway-latency sample into a backup's SMOOTHED latency index
    /// (EWMA, α=0.5) so one spike doesn't reorder the pool but a sustained slowdown
    /// does — this is the "how good is this exit right now" score we promote by.
    pub fn set_backup_latency(&self, addr: &str, ms: u32) {
        if let Some(b) = self
            .backups
            .lock()
            .unwrap()
            .iter_mut()
            .find(|b| b.addr == addr)
        {
            b.latency_ms = if b.latency_ms == 0 {
                ms
            } else {
                (b.latency_ms / 2) + (ms / 2)
            };
        }
    }

    /// Peek the lowest backup latency (without removing) — for the proactive-upgrade
    /// check. None if the pool is empty.
    pub fn fastest_backup_latency(&self) -> Option<u32> {
        self.backups.lock().unwrap().iter().map(|b| b.latency_ms).min()
    }

    /// Take (remove + return) the FASTEST warm backup — used to promote it to
    /// primary on rotation. None if the pool is empty.
    pub fn take_fastest_backup(&self) -> Option<ExitInfo> {
        let mut g = self.backups.lock().unwrap();
        let idx = g
            .iter()
            .enumerate()
            .min_by_key(|(_, b)| b.latency_ms)
            .map(|(i, _)| i)?;
        Some(g.remove(idx))
    }

    /// Arm the one-shot discovery exclusion (the exits the user just refreshed away).
    pub fn set_refresh_avoid(&self, v: Vec<String>) {
        *self.refresh_avoid.lock().unwrap() = v;
    }

    /// Consume the one-shot discovery exclusion.
    pub fn take_refresh_avoid(&self) -> Vec<String> {
        std::mem::take(&mut *self.refresh_avoid.lock().unwrap())
    }
}
