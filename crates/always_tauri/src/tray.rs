//! System tray menu builder for the Always daemon GUI.
//!
//! The tray icon provides quick access to the two most-common controls:
//! Pause / Resume and Auto-Enter.  It also exposes Settings, Onboarding
//! and Quit.
//!
//! Menu items are updated reactively when the daemon state changes.

use std::sync::Arc;
use tauri::menu::{Menu, MenuBuilder, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Wry};
use tokio::sync::mpsc;

use crate::uds_client::{ClientEvent, DaemonEvent, DaemonState, UdsClient};

/// Build the system tray with the menu and connect event-driven updates.
pub fn build_tray(app: &AppHandle, uds: Arc<UdsClient>) -> Result<TrayIcon<Wry>, tauri::Error> {
    // Initial menu state — updated reactively once the first event arrives.
    let initial_pause = MenuItem::new(app, "Pause", true, None::<&str>)?;
    let initial_auto = MenuItem::new(app, "Auto-Enter", false, None::<&str>)?;

    let uds_for_click = Arc::clone(&uds);
    let tray = TrayIconBuilder::new()
        .icon(app.default_window_icon().unwrap().clone())
        .tooltip("Always")
        .menu(&build_menu(app, &initial_pause, &initial_auto)?)
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
                &MenuItem::new(app, "Settings", true, None::<&str>)?,
                &MenuItem::new(app, "Onboarding", true, None::<&str>)?,
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
