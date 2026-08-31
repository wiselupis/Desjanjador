//! Windows Firewall self-exception.
//!
//! `os error 10013` (WSAEACCES) on our outbound sockets means a firewall/security
//! product is blocking `desjanjador.exe` — the app can't fetch the proxy lists or
//! reach any exit, even though the same URLs open in a browser. Windows Defender
//! Firewall can hold a leftover BLOCK rule for us (e.g. the user clicked "Cancel/Block"
//! on a prompt), and a block rule WINS over an allow — so we DELETE every rule that
//! targets our exe, then add a broad allow. The app already runs elevated, so netsh
//! succeeds. This CANNOT override a third-party AV/VPN network filter (the user must
//! whitelist us there), but it clears the common Defender-Firewall case.

#[cfg(windows)]
pub fn ensure_allowed() {
    let exe = match std::env::current_exe() {
        Ok(p) => p.to_string_lossy().into_owned(),
        Err(_) => return,
    };
    // Drop any rule (block or allow) that names our exe, then re-add allow both ways.
    let _ = netsh(&[
        "advfirewall",
        "firewall",
        "delete",
        "rule",
        "name=all",
        &format!("program={exe}"),
    ]);
    for dir in ["out", "in"] {
        let _ = netsh(&[
            "advfirewall",
            "firewall",
            "add",
            "rule",
            "name=Desjanjador",
            &format!("dir={dir}"),
            "action=allow",
            &format!("program={exe}"),
            "enable=yes",
            "profile=any",
        ]);
    }
}

#[cfg(windows)]
fn netsh(args: &[&str]) -> std::io::Result<std::process::ExitStatus> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new("netsh")
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .status()
}

#[cfg(not(windows))]
pub fn ensure_allowed() {}
