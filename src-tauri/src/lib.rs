mod autostart;
mod clients;
mod elevate;
mod log;
mod pool;
mod proxy;
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
    // Pool: validate now, then re-validate every 5 minutes until stopped.
    {
        let sh = shared.clone();
        let mut rx_pool = rx.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                pool::refresh_pool(sh.clone()).await;
                tokio::select! {
                    _ = tokio::time::sleep(std::time::Duration::from_secs(120)) => {}
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
    shared.set_status("parado");
}

/// Always-safe teardown for app exit: never leave the system PAC pointing at us.
fn cleanup(shared: &Arc<Shared>) {
    deactivate(shared);
    let _ = sysproxy::disable();
}

fn status_dto(_app: &AppHandle, shared: &Shared) -> StatusDto {
    StatusDto {
        active: shared.active.load(Ordering::SeqCst),
        autostart: shared.autostart.load(Ordering::SeqCst),
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
    settings::save(&dir, &settings::Settings { active: on });
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
                let _ = w.set_title("Desjanjador");
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
            shared_setup
                .autostart
                .store(autostart::is_enabled(), Ordering::SeqCst);

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
                    "open" => {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
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
                        let app = tray.app_handle();
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                })
                .build(&handle)?;

            // Restore previous Active state on launch; otherwise clear any stale
            // system PAC a prior crash may have left pointing at us.
            let dir = shared_setup.config_dir.lock().unwrap().clone();
            if settings::load(&dir).active {
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

            // Autostart (tray) launch: quietly notify about updates instead of
            // popping the in-window dialog. The dialog still appears if the user
            // opens the window.
            if std::env::args().any(|a| a == "--tray") {
                let h = handle.clone();
                tauri::async_runtime::spawn(async move {
                    if let Ok(info) = updater::check().await {
                        if info.available {
                            use tauri_plugin_notification::NotificationExt;
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
