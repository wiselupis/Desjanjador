//! Windows system-proxy control via the WinINET AutoConfigURL (PAC).
//!
//! We proved empirically that Discord ignores the `--proxy-server` flag but
//! DOES honor the system PAC. So "activating" means pointing AutoConfigURL at
//! the router's local PAC endpoint; "deactivating" removes it. Every change is
//! followed by an InternetSetOption refresh so Chromium/Discord pick it up
//! without a relaunch.

#[cfg(windows)]
const INTERNET_SETTINGS: &str = r"Software\Microsoft\Windows\CurrentVersion\Internet Settings";

/// Bumped on every enable() so the AutoConfigURL changes each time we (re)apply it.
#[cfg(windows)]
static PAC_NONCE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

#[cfg(windows)]
pub fn enable(port: u16) -> Result<(), String> {
    use std::sync::atomic::Ordering;
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (key, _) = hkcu
        .create_subkey(INTERNET_SETTINGS)
        .map_err(|e| e.to_string())?;
    // A changing ?v= nonce forces Chromium/WinINET to RE-FETCH the PAC when we
    // re-apply it (e.g. after toggling "route the API too"). An identical URL is
    // deduped/cached, which is why the toggle otherwise silently did nothing.
    let n = PAC_NONCE.fetch_add(1, Ordering::Relaxed);
    let url = format!("http://127.0.0.1:{port}/proxy.pac?v={n}");
    key.set_value("AutoConfigURL", &url).map_err(|e| e.to_string())?;
    refresh();
    Ok(())
}

#[cfg(windows)]
pub fn disable() -> Result<(), String> {
    use winreg::enums::{KEY_ALL_ACCESS, HKEY_CURRENT_USER};
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    if let Ok(key) = hkcu.open_subkey_with_flags(INTERNET_SETTINGS, KEY_ALL_ACCESS) {
        let _ = key.delete_value("AutoConfigURL");
    }
    refresh();
    Ok(())
}

#[cfg(windows)]
fn refresh() {
    use windows_sys::Win32::Networking::WinInet::InternetSetOptionW;
    const INTERNET_OPTION_SETTINGS_CHANGED: u32 = 39;
    const INTERNET_OPTION_REFRESH: u32 = 37;
    unsafe {
        InternetSetOptionW(
            std::ptr::null_mut(),
            INTERNET_OPTION_SETTINGS_CHANGED,
            std::ptr::null(),
            0,
        );
        InternetSetOptionW(
            std::ptr::null_mut(),
            INTERNET_OPTION_REFRESH,
            std::ptr::null(),
            0,
        );
    }
}

/// Crash-recovery: if a previous run left AutoConfigURL pointing at our own PAC
/// (and the router is now gone), remove it so browsing isn't stuck on a dead PAC.
/// Only touches the value if it is exactly ours — never clobbers a user's PAC.
#[cfg(windows)]
pub fn disable_if_ours(port: u16) {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    // Match with or without the ?v= nonce so a leftover nonced URL is still cleaned.
    let ours = format!("http://127.0.0.1:{port}/proxy.pac");
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    if let Ok(key) = hkcu.open_subkey(INTERNET_SETTINGS) {
        if let Ok(cur) = key.get_value::<String, _>("AutoConfigURL") {
            if cur.starts_with(&ours) {
                let _ = disable();
            }
        }
    }
}

#[cfg(not(windows))]
pub fn enable(_port: u16) -> Result<(), String> {
    Err("system proxy control is only implemented on Windows for now".into())
}

#[cfg(not(windows))]
pub fn disable() -> Result<(), String> {
    Ok(())
}

#[cfg(not(windows))]
pub fn disable_if_ours(_port: u16) {}
