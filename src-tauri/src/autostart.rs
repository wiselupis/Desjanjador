//! "Start with Windows" via a Scheduled Task (run at logon, highest privileges).
//! Because the app runs elevated, the task starts it at login WITHOUT a UAC
//! prompt each boot. The task references the exe — it does not copy it.

#[cfg(windows)]
const TASK: &str = "Desjanjador";

#[cfg(windows)]
fn schtasks(args: &[&str]) -> std::io::Result<std::process::Output> {
    use std::os::windows::process::CommandExt;
    std::process::Command::new("schtasks")
        .args(args)
        .creation_flags(0x08000000) // CREATE_NO_WINDOW
        .output()
}

#[cfg(windows)]
pub fn is_enabled() -> bool {
    schtasks(&["/Query", "/TN", TASK])
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(windows)]
pub fn enable() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    // "--tray" makes an autostarted instance stay in the tray (no window popup).
    let tr = format!("\"{}\" --tray", exe.display());
    // /DELAY 20s: an ONLOGON task fires before the shell tray + WebView2 runtime
    // are ready at cold boot, which left the app half-started (tray icon shows but
    // the window won't open). A short delay lets the desktop settle first.
    let out = schtasks(&[
        "/Create", "/TN", TASK, "/TR", &tr, "/SC", "ONLOGON", "/DELAY", "0000:20",
        "/RL", "HIGHEST", "/F",
    ])
    .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

#[cfg(windows)]
pub fn disable() -> Result<(), String> {
    let _ = schtasks(&["/Delete", "/TN", TASK, "/F"]);
    Ok(())
}

#[cfg(not(windows))]
pub fn is_enabled() -> bool {
    false
}
#[cfg(not(windows))]
pub fn enable() -> Result<(), String> {
    Ok(())
}
#[cfg(not(windows))]
pub fn disable() -> Result<(), String> {
    Ok(())
}
