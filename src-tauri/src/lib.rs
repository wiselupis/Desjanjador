mod autostart;
mod clients;
mod elevate;
mod log;
mod pool;
mod proxy;
mod secret;
mod settings;
mod state;
mod sysproxy;
mod updater;

use state::{Shared, StatusDto};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, State};

/// Local port the router + PAC live on.
const PORT: u16 = 43110;

// ---------- lifecycle ----------

fn activate(shared: &Arc<Shared>) {
    if shared.active.swap(true, Ordering::SeqCst) {
        return; // already active
    }
    log::log("lifecycle: activate");
    // Load the "route the API too" + "use Tor" preferences so the PAC/router/pool honor them.
    {
        let dir = shared.config_dir.lock().unwrap().clone();
        let s = settings::load(&dir);
        shared.proxy_api.store(s.proxy_api, Ordering::SeqCst);
        shared.use_tor.store(s.use_tor, Ordering::SeqCst);
        shared.set_custom(settings::load_custom_proxy(&dir));
    }
    let (tx, rx) = tokio::sync::watch::channel(false);
    *shared.stop_tx.lock().unwrap() = Some(tx);

    if let Err(e) = sysproxy::enable(shared.port) {
        shared.set_status(format!("falha ao configurar o proxy do sistema: {e}"));
    } else {
        shared.set_status("ativo — procurando saída fora do Brasil…");
    }

    // Router
    {
        let sh = shared.clone();
        let rx_router = rx.clone();
        tauri::async_runtime::spawn(async move {
            proxy::run_router(sh, rx_router).await;
        });
    }
    // Pool: find a primary exit + a warm backup now, then health-check every 20s
    // (a degrading/dead primary is swapped to the pre-validated backup before
    // Discord notices), or immediately when the router signals a failure via
    // `refresh_now`. `Maint` carries the grace counter + cached candidate list.
    {
        let sh = shared.clone();
        let mut rx_pool = rx.clone();
        tauri::async_runtime::spawn(async move {
            // Warm start: seed the maintainer with the last session's best exits so the
            // first discovery re-tries them IN PARALLEL (usually an instant reconnect,
            // plus a pre-warmed pool from the runners-up).
            let cached: Vec<String> = {
                let dir = sh.config_dir.lock().unwrap().clone();
                settings::load(&dir)
                    .last_exits
                    .into_iter()
                    .map(|e| e.addr)
                    .collect()
            };
            let mut maint = pool::Maint::new(cached);
            loop {
                pool::maintain(sh.clone(), &mut maint).await;
                tokio::select! {
                    _ = tokio::time::sleep(std::time::Duration::from_secs(20)) => {}
                    _ = sh.refresh_now.notified() => {}
                    changed = rx_pool.changed() => {
                        if changed.is_err() || *rx_pool.borrow() { break; }
                    }
                }
            }
        });
    }
}

fn deactivate(shared: &Arc<Shared>) {
    if !shared.active.swap(false, Ordering::SeqCst) {
        return; // already inactive
    }
    log::log("lifecycle: deactivate");
    if let Some(tx) = shared.stop_tx.lock().unwrap().take() {
        let _ = tx.send(true);
    }
    let _ = sysproxy::disable();
    shared.set_exit(None);
    shared.clear_backups();
    shared.set_refresh_avoid(Vec::new()); // don't leak a pending refresh exclusion into next start
    shared.set_status("parado");
}

/// Always-safe teardown for app exit: never leave the system PAC pointing at us.
fn cleanup(shared: &Arc<Shared>) {
    deactivate(shared);
    let _ = sysproxy::disable();
}

/// Bring the main window to the foreground reliably (tray Open / click) — works
/// whether it's hidden or minimized. Logs if the window is somehow gone.
fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    } else {
        log::log("tray: janela principal ausente");
    }
}

fn status_dto(_app: &AppHandle, shared: &Shared) -> StatusDto {
    StatusDto {
        active: shared.active.load(Ordering::SeqCst),
        autostart: shared.autostart.load(Ordering::SeqCst),
        proxy_api: shared.proxy_api.load(Ordering::SeqCst),
        use_tor: shared.use_tor.load(Ordering::SeqCst),
        custom_proxy: shared.get_custom().unwrap_or_default(),
        status: shared.status.lock().unwrap().clone(),
        exit: shared.get_exit(),
        port: shared.port,
    }
}

// ---------- commands ----------

#[tauri::command]
fn get_status(app: AppHandle, shared: State<Arc<Shared>>) -> StatusDto {
    status_dto(&app, &shared)
}

#[tauri::command]
fn set_active(app: AppHandle, shared: State<Arc<Shared>>, on: bool) -> StatusDto {
    if on {
        activate(&shared);
    } else {
        deactivate(&shared);
    }
    let dir = shared.config_dir.lock().unwrap().clone();
    settings::save_active(&dir, on);
    status_dto(&app, &shared)
}

/// Toggle also-route-the-API. Re-applies the system PAC so Chromium reloads it
/// with (or without) the extra Discord API hosts.
#[tauri::command]
fn set_proxy_api(app: AppHandle, shared: State<Arc<Shared>>, on: bool) -> StatusDto {
    shared.proxy_api.store(on, Ordering::SeqCst);
    let dir = shared.config_dir.lock().unwrap().clone();
    settings::save_proxy_api(&dir, on);
    if shared.active.load(Ordering::SeqCst) {
        let _ = sysproxy::enable(shared.port);
    }
    status_dto(&app, &shared)
}

/// Toggle the opt-in Tor fallback. Takes effect on the next discovery/rotation; if
/// the user turns it OFF while a Tor exit is serving, drop it so the pool moves to a
/// country-pinned free proxy.
#[tauri::command]
fn set_use_tor(app: AppHandle, shared: State<Arc<Shared>>, on: bool) -> StatusDto {
    shared.use_tor.store(on, Ordering::SeqCst);
    let dir = shared.config_dir.lock().unwrap().clone();
    settings::save_use_tor(&dir, on);
    if !on {
        // Drop only Tor-labelled entries (addr 127.0.0.1:*), keeping healthy free
        // backups, then re-discover if we just dropped the serving exit.
        for b in shared.get_backups() {
            if b.addr.starts_with("127.0.0.1:") {
                shared.remove_backup(&b.addr);
            }
        }
        let dropped_primary = shared
            .get_exit()
            .map_or(false, |e| e.addr.starts_with("127.0.0.1:"));
        if dropped_primary {
            shared.set_exit(None);
        }
        if dropped_primary && shared.active.load(Ordering::SeqCst) {
            shared.refresh_now.notify_one();
        }
    }
    status_dto(&app, &shared)
}

/// Set (or clear, with an empty string) the user's custom proxy. Accepts a comma/`;`
/// list of proxies (first = priority) or an http(s) URL to a proxy list. Stored
/// ENCRYPTED at rest. Takes effect immediately (drops the current exit so the pool loop
/// re-picks with the new preference on the next tick).
#[tauri::command]
fn set_custom_proxy(app: AppHandle, shared: State<Arc<Shared>>, value: String) -> StatusDto {
    let dir = shared.config_dir.lock().unwrap().clone();
    settings::save_custom_proxy(&dir, &value);
    shared.set_custom(Some(value));
    if shared.active.load(Ordering::SeqCst) {
        // Re-pick with the new preference: clear the serving exit (the pool loop will
        // prefer the custom proxy, or fall back), and wake it now.
        shared.set_exit(None);
        shared.set_status("aplicando saída…");
        shared.refresh_now.notify_one();
    }
    status_dto(&app, &shared)
}

/// Force a fresh exit now (user tapped the refresh icon): drop the primary +
/// backup and wake the pool to rediscover.
#[tauri::command]
fn refresh_exit(app: AppHandle, shared: State<Arc<Shared>>) -> StatusDto {
    if shared.active.load(Ordering::SeqCst) {
        // Remember what we're dropping so the next discovery doesn't just hand back
        // the same exit(s) — refresh must give a genuinely different one.
        let mut avoid: Vec<String> = shared.get_exit().map(|e| e.addr).into_iter().collect();
        avoid.extend(shared.get_backups().into_iter().map(|b| b.addr));
        shared.set_refresh_avoid(avoid);
        shared.set_exit(None);
        shared.clear_backups();
        shared.set_status("atualizando saída…");
        shared.refresh_now.notify_one();
    }
    status_dto(&app, &shared)
}

#[tauri::command]
fn set_autostart(shared: State<Arc<Shared>>, on: bool) -> Result<bool, String> {
    if on {
        autostart::enable()?;
    } else {
        autostart::disable()?;
    }
    let now = autostart::is_enabled();
    shared.autostart.store(now, Ordering::SeqCst);
    Ok(now)
}

#[tauri::command]
fn exit_app(app: AppHandle, shared: State<Arc<Shared>>) {
    cleanup(&shared);
    app.exit(0);
}

#[tauri::command]
fn detect_clients() -> clients::ClientReport {
    clients::detect()
}

#[tauri::command]
async fn install_betterdiscord() -> Result<String, String> {
    clients::install_betterdiscord().await
}

#[tauri::command]
fn patch_client() -> Result<String, String> {
    clients::patch_client()
}

#[tauri::command]
async fn check_update() -> Result<updater::UpdateInfo, String> {
    updater::check().await
}

#[tauri::command]
async fn apply_update(
    app: AppHandle,
    shared: State<'_, Arc<Shared>>,
    url: String,
) -> Result<(), String> {
    cleanup(&shared);
    updater::apply(url).await?;
    app.exit(0);
    Ok(())
}

// ---------- run ----------

/// Kill any other running instance so a new launch replaces the old one
/// (never two at once).
#[cfg(windows)]
fn kill_other_instances() {
    use std::os::windows::process::CommandExt;
    let pid = std::process::id();
    let _ = std::process::Command::new("taskkill")
        .args(["/IM", "desjanjador.exe", "/F", "/FI", &format!("PID ne {pid}")])
        .creation_flags(0x08000000)
        .output();
}
#[cfg(not(windows))]
fn kill_other_instances() {}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    elevate::ensure_elevated();
    kill_other_instances();
    let shared = Arc::new(Shared::new(PORT));
    let shared_setup = shared.clone();
    let shared_run = shared.clone();

    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .manage(shared.clone())
        .invoke_handler(tauri::generate_handler![
            get_status,
            set_active,
            set_proxy_api,
            set_use_tor,
            set_custom_proxy,
            refresh_exit,
            set_autostart,
            exit_app,
            detect_clients,
            install_betterdiscord,
            patch_client,
            check_update,
            apply_update
        ])
        .on_window_event(|window, event| {
            // Close (X) hides to tray instead of quitting.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .setup(move |app| {
            let handle = app.handle().clone();

            // Clean up a leftover exe from a previous self-update, and force the
            // window title (fixes the truncated "Desjanjado" in the title bar).
            updater::cleanup_old();
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.set_title(&format!("Desjanjador v{}", env!("CARGO_PKG_VERSION")));
                // Fit the window to the screen so it never runs off the bottom on small
                // or high-DPI displays (e.g. 150% scaling on a 1080p panel = 720 logical
                // px, less than our 730 design height, or a 1366x768 laptop). Clamp the
                // height to the monitor's usable area; the content's own scroll
                // (overflow-y:auto with the footer pinned) absorbs any shortfall.
                if let Ok(Some(mon)) = w.current_monitor() {
                    let logical_h = mon.size().height as f64 / mon.scale_factor();
                    let usable = (logical_h - 80.0).max(480.0); // reserve taskbar + title bar
                    let h = 730.0_f64.min(usable);
                    let _ = w.set_size(tauri::LogicalSize::new(440.0, h));
                    let _ = w.center();
                }
            }

            // DESJANJADOR_AUTOUPDATE=1 applies an available update silently (no
            // popup) — used to validate the self-updater with an old exe.
            if std::env::var("DESJANJADOR_AUTOUPDATE").is_ok() {
                let h = handle.clone();
                let sh = shared_setup.clone();
                tauri::async_runtime::spawn(async move {
                    match updater::check().await {
                        Ok(info) if info.available => {
                            log::log(&format!("autoupdate: {} -> {}", info.current, info.version));
                            cleanup(&sh);
                            if updater::apply(info.url).await.is_ok() {
                                h.exit(0);
                            }
                        }
                        Ok(_) => log::log("autoupdate: ja atualizado"),
                        Err(e) => log::log(&format!("autoupdate erro: {e}")),
                    }
                });
            }

            // Resolve + remember the config dir for settings persistence.
            if let Ok(dir) = app.path().app_config_dir() {
                log::init(&dir);
                *shared_setup.config_dir.lock().unwrap() = dir;
            }
            let autostart_on = autostart::is_enabled();
            shared_setup.autostart.store(autostart_on, Ordering::SeqCst);
            // Migrate an existing autostart task to the current exe path + logon
            // delay (older versions created it with no delay -> half-started tray
            // at cold boot). Idempotent; only when already enabled.
            if autostart_on {
                let _ = autostart::enable();
            }

            // Tray icon with an Open / Exit menu.
            let open_i = MenuItem::with_id(&handle, "open", "Open", true, None::<&str>)?;
            let exit_i = MenuItem::with_id(&handle, "exit", "Exit", true, None::<&str>)?;
            let menu = Menu::with_items(&handle, &[&open_i, &exit_i])?;

            let sh_menu = shared_setup.clone();
            let _tray = TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("Desjanjador")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(move |app, event| match event.id.as_ref() {
                    "open" => show_main(app),
                    "exit" => {
                        cleanup(&sh_menu);
                        app.exit(0);
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show_main(tray.app_handle());
                    }
                })
                .build(&handle)?;

            // Restore previous Active state on launch; otherwise clear any stale
            // system PAC a prior crash may have left pointing at us.
            let dir = shared_setup.config_dir.lock().unwrap().clone();
            let was_active = settings::load(&dir).active;
            if was_active {
                activate(&shared_setup);
            } else {
                sysproxy::disable_if_ours(shared_setup.port);
            }

            // Autostart (launched with --tray) stays hidden in the tray; a manual
            // launch shows the window. Explicit hide, since visible:false alone
            // doesn't reliably keep the webview window hidden.
            if let Some(w) = app.get_webview_window("main") {
                let tray = std::env::args().any(|a| a == "--tray");
                if tray {
                    let _ = w.hide();
                } else {
                    let _ = w.show();
                    let _ = w.set_focus();
                }
                log::log(&format!("window: tray-start={} visible={:?}", tray, w.is_visible()));
            }

            // Autostart (tray) launch.
            if std::env::args().any(|a| a == "--tray") {
                use tauri_plugin_notification::NotificationExt;
                // The app starts ~20s after logon (task delay, for a reliable
                // tray), so if it auto-activated, Discord may already have opened
                // its gateway on the direct BR route — and an already-open socket
                // is never re-proxied. Nudge the user to restart Discord so its
                // gateway re-connects through the exit.
                if was_active {
                    let _ = handle
                        .notification()
                        .builder()
                        .title("Desjanjador ativo")
                        .body("Se o Discord já estiver aberto, reinicie-o para liberar o Go Live e a câmera.")
                        .show();
                }
                // Quietly notify about updates instead of popping the in-window
                // dialog (which still appears if the user opens the window).
                let h = handle.clone();
                tauri::async_runtime::spawn(async move {
                    if let Ok(info) = updater::check().await {
                        if info.available {
                            let _ = h
                                .notification()
                                .builder()
                                .title("Desjanjador")
                                .body(format!(
                                    "Nova versão {} disponível — abra o app para atualizar.",
                                    info.version
                                ))
                                .show();
                        }
                    }
                });
            }

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building Desjanjador")
        .run(move |_app_handle, event| {
            if let tauri::RunEvent::Exit = event {
                cleanup(&shared_run);
            }
        });
}
