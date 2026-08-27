//! Discord client-mod support: detect installs, install BetterDiscord, and drop
//! a Go Live client patch.
//!
//! Reality check: Discord's Go Live block is a server-assigned experiment keyed
//! to the gateway's exit IP. The real unblock is Desjanjador's gateway proxy;
//! this client patch is a safety net (region override) — a scaffold to extend.

use std::path::PathBuf;

fn appdata() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(PathBuf::from)
}
fn localappdata() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
}

const FLAVORS: &[&str] = &["Discord", "DiscordPTB", "DiscordCanary"];

#[derive(serde::Serialize, Clone)]
pub struct ClientReport {
    pub betterdiscord: bool,
    pub vencord: bool,
    pub equicord: bool,
    pub discord_installs: Vec<String>,
}

pub fn detect() -> ClientReport {
    let ad = appdata();
    let has = |sub: &str| ad.as_ref().map(|p| p.join(sub).is_dir()).unwrap_or(false);
    let mut installs = Vec::new();
    if let Some(la) = localappdata() {
        for f in FLAVORS {
            if la.join(f).is_dir() {
                installs.push((*f).to_string());
            }
        }
    }
    ClientReport {
        betterdiscord: has("BetterDiscord"),
        vencord: has("Vencord"),
        equicord: has("Equicord"),
        discord_installs: installs,
    }
}

/// Newest `app-*/modules/discord_desktop_core-*/discord_desktop_core/index.js`.
fn core_index(flavor: &str) -> Option<PathBuf> {
    let base = localappdata()?.join(flavor);
    let mut apps: Vec<PathBuf> = std::fs::read_dir(&base)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_dir()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with("app-"))
                    .unwrap_or(false)
        })
        .collect();
    apps.sort();
    for app in apps.into_iter().rev() {
        if let Ok(rd) = std::fs::read_dir(app.join("modules")) {
            for e in rd.flatten() {
                if e.file_name().to_string_lossy().starts_with("discord_desktop_core") {
                    let idx = e.path().join("discord_desktop_core").join("index.js");
                    if idx.is_file() {
                        return Some(idx);
                    }
                }
            }
        }
    }
    None
}

/// Download the BetterDiscord asar (if missing) and inject it into every Discord
/// install by prepending a `require` to the core's index.js (backed up first).
pub async fn install_betterdiscord() -> Result<String, String> {
    let ad = appdata().ok_or("no APPDATA")?;
    let data = ad.join("BetterDiscord").join("data");
    std::fs::create_dir_all(&data).map_err(|e| e.to_string())?;
    let asar = data.join("betterdiscord.asar");

    if !asar.is_file() {
        let client = reqwest::Client::builder()
            .user_agent("Desjanjador/0.1")
            .build()
            .map_err(|e| e.to_string())?;
        let bytes = client
            .get("https://github.com/BetterDiscord/BetterDiscord/releases/latest/download/betterdiscord.asar")
            .send()
            .await
            .map_err(|e| format!("falha no download: {e}"))?
            .error_for_status()
            .map_err(|e| format!("falha no download: {e}"))?
            .bytes()
            .await
            .map_err(|e| e.to_string())?;
        std::fs::write(&asar, &bytes).map_err(|e| e.to_string())?;
    }
    let asar_js = asar.to_string_lossy().replace('\\', "/");

    let mut done = Vec::new();
    for f in FLAVORS {
        if let Some(idx) = core_index(f) {
            let content = std::fs::read_to_string(&idx).unwrap_or_default();
            if content.contains("betterdiscord.asar") {
                done.push(format!("{f} (already)"));
                continue;
            }
            let _ = std::fs::write(idx.with_extension("js.bak"), &content);
            let patched = format!("require(\"{asar_js}\");\n{content}");
            std::fs::write(&idx, patched).map_err(|e| e.to_string())?;
            done.push(f.to_string());
        }
    }
    if done.is_empty() {
        return Err("nenhuma instalação do Discord encontrada".into());
    }
    crate::log::log(&format!("clients: BetterDiscord injected into {}", done.join(", ")));
    Ok(format!(
        "BetterDiscord pronto em: {} — reinicie o Discord por completo",
        done.join(", ")
    ))
}

/// Drop the Go Live client patch. BetterDiscord gets a drop-in plugin;
/// Vencord/Equicord need a build-time userplugin (not automated yet).
pub fn patch_client() -> Result<String, String> {
    let ad = appdata().ok_or("no APPDATA")?;
    let bd_plugins = ad.join("BetterDiscord").join("plugins");
    if bd_plugins.is_dir() {
        let file = bd_plugins.join("Desjanjador.plugin.js");
        std::fs::write(&file, GO_LIVE_PLUGIN).map_err(|e| e.to_string())?;
        crate::log::log("clients: wrote Desjanjador.plugin.js");
        return Ok("Desjanjador.plugin.js criado — ative em BetterDiscord ▸ Configurações ▸ Plugins".into());
    }
    let r = detect();
    if r.vencord || r.equicord {
        return Err("Vencord/Equicord detectado, mas precisa de userplugin em build — instale o BetterDiscord para patch direto".into());
    }
    Err("Nenhum mod de cliente — clique em Instalar BetterDiscord primeiro".into())
}

/// Safety-net BD plugin: keeps the voice region off Brazil so a session created
/// abroad stays coherent. Defensive (never throws into the client). Extend the
/// marked spot with the exact Go Live experiment override if you want.
const GO_LIVE_PLUGIN: &str = r#"/**
 * @name Desjanjador
 * @author Lucas
 * @description Go Live safety-net: overrides voice region away from Brazil. The real unblock is Desjanjador's gateway proxy.
 * @version 0.1.0
 */
module.exports = class Desjanjador {
  constructor() { this._undo = []; this.REGION = "us-east"; }
  start() {
    try {
      const W = BdApi.Webpack;
      const RTC = W.getModule(m => m && (m.getPreferredRegions || m.getPreferredRegion));
      if (RTC) {
        const region = this.REGION;
        for (const key of ["getPreferredRegion", "getPreferredRegions"]) {
          if (typeof RTC[key] === "function") {
            const orig = RTC[key].bind(RTC);
            RTC[key] = function () {
              try { return key.endsWith("s") ? [region] : region; }
              catch (e) { return orig.apply(this, arguments); }
            };
            this._undo.push(() => { RTC[key] = orig; });
          }
        }
      }
      // === extend here: override the Go Live guard experiment for full client-side effect ===
      if (BdApi.UI && BdApi.UI.showToast) BdApi.UI.showToast("Desjanjador: region override active", { type: "success" });
    } catch (e) { console.error("[Desjanjador]", e); }
  }
  stop() { for (const u of this._undo) { try { u(); } catch (e) {} } this._undo = []; }
};
"#;
