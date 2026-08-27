//! Self-updater via GitHub releases (public repo, no auth needed).
//!
//! Checks the latest release tag; if newer than the running build, the UI shows
//! a popup. On accept we download the `.exe` asset, rename the running exe out of
//! the way (allowed on Windows), drop the new one in its place, relaunch, and quit.

use serde::Serialize;

const REPO: &str = "wiselupis/Desjanjador";
const CURRENT: &str = env!("CARGO_PKG_VERSION");

#[derive(Serialize, Clone)]
pub struct UpdateInfo {
    pub available: bool,
    pub version: String,
    pub current: String,
    pub notes: String,
    pub url: String,
}

pub async fn check() -> Result<UpdateInfo, String> {
    let client = reqwest::Client::builder()
        .user_agent("Desjanjador")
        .build()
        .map_err(|e| e.to_string())?;
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let v: serde_json::Value = client
        .get(url)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;

    let tag = v
        .get("tag_name")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .trim_start_matches('v')
        .to_string();
    let notes = v.get("body").and_then(|t| t.as_str()).unwrap_or("").to_string();
    let exe_url = v
        .get("assets")
        .and_then(|a| a.as_array())
        .and_then(|arr| {
            arr.iter().find_map(|asset| {
                let name = asset.get("name").and_then(|n| n.as_str()).unwrap_or("");
                if name.to_ascii_lowercase().ends_with(".exe") {
                    asset
                        .get("browser_download_url")
                        .and_then(|u| u.as_str())
                        .map(|s| s.to_string())
                } else {
                    None
                }
            })
        })
        .unwrap_or_default();

    let available = !tag.is_empty() && is_newer(&tag, CURRENT) && !exe_url.is_empty();
    Ok(UpdateInfo {
        available,
        version: tag,
        current: CURRENT.to_string(),
        notes,
        url: exe_url,
    })
}

fn is_newer(remote: &str, current: &str) -> bool {
    let parse = |s: &str| {
        s.split('.')
            .filter_map(|p| p.parse::<u32>().ok())
            .collect::<Vec<_>>()
    };
    let r = parse(remote);
    let c = parse(current);
    for i in 0..3 {
        let rv = *r.get(i).unwrap_or(&0);
        let cv = *c.get(i).unwrap_or(&0);
        if rv != cv {
            return rv > cv;
        }
    }
    false
}

/// Download the new exe and swap it in for the running one, then relaunch.
pub async fn apply(url: String) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .user_agent("Desjanjador")
        .build()
        .map_err(|e| e.to_string())?;
    let bytes = client
        .get(&url)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .bytes()
        .await
        .map_err(|e| e.to_string())?;

    let cur = std::env::current_exe().map_err(|e| e.to_string())?;
    let old = cur.with_extension("old.exe");
    let _ = std::fs::remove_file(&old);
    // Windows lets you RENAME a running exe (just not overwrite/delete it).
    std::fs::rename(&cur, &old).map_err(|e| format!("renomear atual: {e}"))?;
    std::fs::write(&cur, &bytes).map_err(|e| format!("gravar novo exe: {e}"))?;
    std::process::Command::new(&cur)
        .spawn()
        .map_err(|e| format!("reabrir: {e}"))?;
    Ok(())
}

/// On startup, delete the leftover `*.old.exe` from a previous self-update.
pub fn cleanup_old() {
    if let Ok(cur) = std::env::current_exe() {
        let _ = std::fs::remove_file(cur.with_extension("old.exe"));
    }
}
