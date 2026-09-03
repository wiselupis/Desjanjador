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

/// On autostart, Discord often opens its gateway on the direct (BR) route before our proxy
/// is ready — and Discord never re-proxies an already-open socket. Restart every RUNNING
/// Discord flavor so it re-reads the system PAC and brings its gateway up through the exit.
/// BetterDiscord/Vencord/Equicord run INSIDE the Discord process, so relaunching the exe
/// re-injects them automatically. Returns the flavors that were restarted.
#[cfg(windows)]
pub fn restart_running_discords() -> Vec<String> {
    let mut restarted = Vec::new();
    for f in FLAVORS {
        let exe = format!("{f}.exe");
        if !process_running(&exe) {
            continue;
        }
        // Relaunch via <flavor>\Update.exe --processStart <exe> — the same launcher the
        // Start-menu shortcut uses (picks the newest app-* build; client mods re-hook).
        let update = match localappdata().map(|p| p.join(f).join("Update.exe")) {
            Some(u) if u.is_file() => u,
            _ => continue, // no canonical launcher — don't kill what we can't relaunch
        };
        kill_process(&exe);
        let _ = run_hidden(std::process::Command::new(&update).arg("--processStart").arg(&exe));
        restarted.push((*f).to_string());
        crate::log::log(&format!("clients: reiniciou {f} (rota do gateway)"));
    }
    restarted
}

#[cfg(windows)]
fn process_running(exe: &str) -> bool {
    run_hidden_output(std::process::Command::new("tasklist").args(["/FI", &format!("IMAGENAME eq {exe}"), "/NH"]))
        .map(|s| s.to_lowercase().contains(&exe.to_lowercase()))
        .unwrap_or(false)
}

#[cfg(windows)]
fn kill_process(exe: &str) {
    let _ = run_hidden(std::process::Command::new("taskkill").args(["/F", "/IM", exe]));
}

#[cfg(windows)]
fn run_hidden(cmd: &mut std::process::Command) -> std::io::Result<std::process::ExitStatus> {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(0x0800_0000).status() // CREATE_NO_WINDOW
}

#[cfg(windows)]
fn run_hidden_output(cmd: &mut std::process::Command) -> Option<String> {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(0x0800_0000)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
}

#[cfg(not(windows))]
pub fn restart_running_discords() -> Vec<String> {
    Vec::new()
}

/// Safety-net BD plugin: keeps the voice region off Brazil so a session created
/// abroad stays coherent. Defensive (never throws into the client). Extend the
/// marked spot with the exact Go Live experiment override if you want.
const GO_LIVE_PLUGIN: &str = r#"/**
 * @name Desjanjador
 * @author Lucas
 * @description Go Live safety-net: voice-region override + Go Live "zombie" (frozen screenshare) DETECTION. The real unblock is Desjanjador's gateway proxy.
 * @version 0.2.0
 */
// RTC freeze ("zombie") discovery + recovery scaffold.
// Discord's Go Live sometimes keeps the connection alive but the ENCODED VIDEO OUTPUT
// freezes (framesEncoded/bitrate/resolution stall) while a viewer still wants it. The
// stream lives in the native `discord_voice` addon, not window.RTCPeerConnection. We wrap
// the stream-connection creator, poll getFilteredStats(2, cb), and detect a stalled output.
// AUTO_RECOVER (conn.destroy() on the STREAM connection only → Discord renegotiates it) is
// gated OFF until the exact stats field names are confirmed from a real freeze's log — a
// wrong-field guess must never drop a healthy stream. Flip AUTO_RECOVER to true once the
// "[Desjanjador RTC] stats sample:" log confirms `framesEncoded` on your Discord build.
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
      if (BdApi.UI && BdApi.UI.showToast) BdApi.UI.showToast("Desjanjador: region override active", { type: "success" });
    } catch (e) { console.error("[Desjanjador]", e); }
    try { this._startRtcWatch(); } catch (e) { console.error("[Desjanjador RTC]", e); }
  }
  _startRtcWatch() {
    const AUTO_RECOVER = false; // <-- flip to true once a real freeze confirms the fields
    const CFG = { POLL_MS: 3000, WARMUP_MS: 90000, FREEZE_MS: 20000, COOLDOWN_MS: 15000, MAX_TRIES: 3, AUTO_RECOVER: AUTO_RECOVER };
    const nm = window.DiscordNative && window.DiscordNative.nativeModules;
    if (!nm || typeof nm.requireModule !== "function") { console.warn("[Desjanjador RTC] no DiscordNative"); return; }
    let voice = null;
    try { voice = nm.requireModule("discord_voice"); } catch (e) {}
    if (!voice) { console.warn("[Desjanjador RTC] discord_voice addon not available"); return; }
    const CREATOR = "createOwnStreamConnectionWithOptions";
    if (typeof voice[CREATOR] !== "function") { console.warn("[Desjanjador RTC] stream creator missing (Discord build changed?)"); return; }
    const self = this;
    const orig = voice[CREATOR];
    voice[CREATOR] = function () {
      const conn = orig.apply(this, arguments);
      try { if (conn) self._watchStream(conn, CFG); } catch (e) {}
      return conn;
    };
    this._undo.push(() => { try { voice[CREATOR] = orig; } catch (e) {} });
    console.log("[Desjanjador RTC] watching Go Live streams (detection" + (AUTO_RECOVER ? "+recover" : "-only") + ")");
  }
  _watchStream(conn, cfg) {
    if (!conn || typeof conn.getFilteredStats !== "function") { console.warn("[Desjanjador RTC] stream conn has no getFilteredStats"); return; }
    const st = { startedAt: Date.now(), lastOut: null, lastOutAt: Date.now(), everProgressed: false, lastActAt: 0, tries: 0, logged: false, dead: false };
    const self = this;
    const timer = setInterval(function () {
      if (st.dead) { clearInterval(timer); return; }
      try { conn.getFilteredStats(2, function (stats) { try { self._evalStats(conn, st, stats, cfg, timer); } catch (e) {} }); }
      catch (e) { clearInterval(timer); }
    }, cfg.POLL_MS);
    this._undo.push(() => clearInterval(timer));
  }
  _evalStats(conn, st, stats, cfg, timer) {
    if (!st.logged) { st.logged = true; try { console.log("[Desjanjador RTC] stats sample:", this._summarize(stats)); } catch (e) {} }
    const v = this._pickVideo(stats);
    if (!v) return;
    const out = Number(v.framesEncoded);
    if (!Number.isFinite(out)) return; // incomplete stats -> fail closed
    const now = Date.now();
    if (st.lastOut === null) { st.lastOut = out; st.lastOutAt = now; return; }
    if (out !== st.lastOut) { if (out > st.lastOut) st.everProgressed = true; st.lastOut = out; st.lastOutAt = now; return; }
    // Output is stalled. Only meaningful if this counter was ALIVE before (guards a wrong field).
    if (!st.everProgressed) return;
    if (now - st.startedAt < cfg.WARMUP_MS) return;
    const frozenMs = now - st.lastOutAt;
    if (frozenMs < cfg.FREEZE_MS) return;
    const capturing = this._captureAlive(v);
    console.warn("[Desjanjador RTC] output FROZEN " + Math.round(frozenMs / 1000) + "s (framesEncoded=" + out + ", capturing=" + capturing + ")");
    if (!capturing) return; // if capture also died it's a different failure; don't churn the stream
    try { if (BdApi.UI && BdApi.UI.showToast) BdApi.UI.showToast("Desjanjador: transmissão congelada (" + Math.round(frozenMs / 1000) + "s)", { type: "warning" }); } catch (e) {}
    if (!cfg.AUTO_RECOVER) return; // detection-only until fields are confirmed
    if (now - st.lastActAt < cfg.COOLDOWN_MS || st.tries >= cfg.MAX_TRIES) return;
    st.lastActAt = now; st.tries++;
    console.warn("[Desjanjador RTC] recovering: destroying the stream connection (try " + st.tries + ")");
    try { conn.destroy(); } catch (e) {}
    st.dead = true; clearInterval(timer); // a fresh stream conn will be re-captured by the creator wrap
  }
  _pickVideo(stats) {
    try {
      if (!stats || typeof stats !== "object") return null;
      let best = null;
      const consider = (o) => { if (o && typeof o === "object" && (typeof o.framesEncoded === "number" || typeof o.captureFrames === "number")) { if (!best || (o.framesEncoded || 0) > (best.framesEncoded || 0)) best = o; } };
      const scan = (o, d) => { if (!o || typeof o !== "object" || d > 2) return; consider(o); for (const k in o) { try { scan(o[k], d + 1); } catch (e) {} } };
      scan(stats, 0);
      return best;
    } catch (e) { return null; }
  }
  _captureAlive(v) { try { return Number(v.captureFrames) > 0 || Number(v.inputFrameRate) > 0 || Number(v.captureFrameRate) > 0; } catch (e) { return true; } }
  _summarize(stats) {
    try {
      const out = [];
      const walk = (o, p, d) => { if (!o || typeof o !== "object" || d > 3) return; for (const k in o) { const val = o[k]; if (typeof val === "number") out.push(p + k + "=" + val); else if (typeof val === "object") walk(val, p + k + ".", d + 1); } };
      walk(stats, "", 0);
      return out.slice(0, 80).join(" ") || "(no numeric fields)";
    } catch (e) { return "(unsummarizable)"; }
  }
  stop() { for (const u of this._undo) { try { u(); } catch (e) {} } this._undo = []; }
};
"#;
