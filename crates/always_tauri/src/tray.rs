//! System tray menu builder for the Always daemon GUI.
//!
//! The tray icon provides quick access to the two most-common controls:
//! Pause / Resume and Auto-Enter.  It also exposes Settings, Onboarding
//! and Quit.
//!
//! Menu items are updated reactively when the daemon state changes.
//!
//! macOS is excluded: the Swift menu bar in `Always.app` owns the only
//! menu-bar surface there and opens the Tauri settings window directly
//! (launching/focusing the bundled `always-gui` binary), so a Tauri tray
//! icon would show up as a duplicate menu.  `build_tray` stays callable
//! on macOS as a no-op stub so `main` remains platform-uniform.

use std::sync::Arc;

#[cfg(not(target_os = "macos"))]
use tauri::menu::{Menu, MenuBuilder, MenuItem, PredefinedMenuItem, Submenu};
#[cfg(not(target_os = "macos"))]
use tauri::tray::{TrayIcon, TrayIconBuilder, TrayIconEvent};
#[cfg(not(target_os = "macos"))]
use tauri::{AppHandle, Wry};
#[cfg(not(target_os = "macos"))]
use tokio::sync::mpsc;

#[cfg(not(target_os = "macos"))]
use crate::uds_client::{ClientEvent, DaemonEvent, DaemonState, UdsClient};
#[cfg(not(target_os = "macos"))]
use crate::window_manager::{ShowOnboardingReason, WindowManager};

/// macOS stub — no tray icon is created (see module docs).
#[cfg(target_os = "macos")]
#[allow(dead_code)]
pub fn build_tray(
    app: &tauri::AppHandle,
    uds: Arc<crate::uds_client::UdsClient>,
) -> Result<(), tauri::Error> {
    let _ = (app, uds);
    tracing::info!("tray_disabled_on_macos_swift_menu_bar_owns_surface");
    Ok(())
}

/// Build the system tray with the menu and connect event-driven updates.
#[cfg(not(target_os = "macos"))]
pub fn build_tray(app: &AppHandle, uds: Arc<UdsClient>) -> Result<TrayIcon<Wry>, tauri::Error> {
    // Initial menu state — updated reactively once the first event arrives.
    let initial_pause = MenuItem::new(app, "Pause", true, None::<&str>)?;
    let initial_auto = MenuItem::new(app, "Auto-Enter", false, None::<&str>)?;

    let uds_for_click = Arc::clone(&uds);
    let tray = TrayIconBuilder::new()
        .icon(app.default_window_icon().unwrap().clone())
        .tooltip("Always")
        .menu(&build_menu(app, &initial_pause, &initial_auto)?)
        .on_menu_event(|app, event| {
            let wm = WindowManager::new(app.clone());
            match event.id().as_ref() {
                "settings" => wm.show_settings(),
                "onboarding" => wm.show_onboarding(ShowOnboardingReason::Reinvoked),
                _ => {}
            }
        })
        .on_tray_icon_event(move |tray, event| {
            if let TrayIconEvent::Click { .. } = event {
                // Left-click toggles pause/resume directly.
                let uds = Arc::clone(&uds_for_click);
                tauri::async_runtime::spawn(async move {
                    let cmd = crate::uds_client::DaemonCommand::TogglePause;
                    if let Err(e) = uds.send_command(cmd).await {
                        tracing::warn!(error = %e, "tray_left_click_toggle_pause_failed");
                    }
                });
            }
        })
        .build(app)?;

    // Spawn the state-updater task: it listens for state-changing events
    // via the UdsClient's callback mechanism and rebuilds the tray menu.
    let tray_handle = tray.clone();
    let uds_arc = Arc::clone(&uds);

    tauri::async_runtime::spawn(async move {
        update_menu_loop(&tray_handle, &uds_arc).await;
    });

    Ok(tray)
}

/// Build the tray menu from the given pause / auto-enter items.
#[cfg(not(target_os = "macos"))]
fn build_menu(
    app: &AppHandle,
    pause_item: &MenuItem<Wry>,
    auto_enter_item: &MenuItem<Wry>,
) -> Result<Menu<Wry>, tauri::Error> {
    MenuBuilder::new(app)
        .item(pause_item)
        .item(auto_enter_item)
        .separator()
        .item(&Submenu::with_items(
            app,
            "Settings",
            true,
            &[
                &MenuItem::with_id(app, "settings", "Settings", true, None::<&str>)?,
                &MenuItem::with_id(app, "onboarding", "Onboarding", true, None::<&str>)?,
            ],
        )?)
        .separator()
        .item(&PredefinedMenuItem::quit(app, None)?)
        .build()
}

/// Spawn a handler that rebuilds the tray menu on state changes.
///
/// Registers a callback on `uds` that fires a channel token whenever
/// the pause or auto-enter state changes.  The main loop then reads
/// tokens and rebuilds the menu.
#[cfg(not(target_os = "macos"))]
async fn update_menu_loop(tray: &TrayIcon<Wry>, uds: &Arc<UdsClient>) {
    let (tx, mut rx) = mpsc::channel::<()>(16);

    // Register a state-change callback on the UDS client.
    let tx = std::sync::Mutex::new(Some(tx));
    uds.on_event(move |event| {
        let ClientEvent::Event(line) = event else {
            return;
        };
        let Ok(parsed) = serde_json::from_str::<DaemonEvent>(&line) else {
            return;
        };
        let needs_update = matches!(
            parsed,
            DaemonEvent::Paused
                | DaemonEvent::Resumed
                | DaemonEvent::ResumedQuietly
                | DaemonEvent::AutoEnterEnabled
                | DaemonEvent::AutoEnterDisabled
                | DaemonEvent::MasterPauseChanged { .. }
                | DaemonEvent::ListeningStarted
                | DaemonEvent::ProcessingStarted
                | DaemonEvent::ProcessingStopped
        );
        if needs_update {
            if let Some(ref tx) = *tx.lock().unwrap() {
                let _ = tx.try_send(());
            }
        }
    });

    // Consume channel tokens and rebuild the menu.
    while rx.recv().await.is_some() {
        let app = tray.app_handle();
        let state = uds.get_state().await;
        let auto_enter = uds.is_auto_enter_enabled().await;
        let paused = uds.is_paused().await;

        let label = match state {
            DaemonState::Listening => "Pause",
            DaemonState::Paused => "Resume",
            DaemonState::Processing => "Pause",
            DaemonState::Unknown => "Pause",
        };

        let pause_item = match MenuItem::new(app, label, true, None::<&str>) {
            Ok(item) => item,
            Err(e) => {
                tracing::warn!(error = %e, "failed_to_create_pause_menu_item");
                continue;
            }
        };
        let auto_item = match MenuItem::new(app, "Auto-Enter", auto_enter || paused, None::<&str>) {
            Ok(item) => item,
            Err(e) => {
                tracing::warn!(error = %e, "failed_to_create_auto_enter_menu_item");
                continue;
            }
        };

        if let Ok(menu) = build_menu(app, &pause_item, &auto_item) {
            if let Err(e) = tray.set_menu(Some(menu)) {
                tracing::warn!(error = %e, "tray_set_menu_failed");
            }
        }
    }
}
