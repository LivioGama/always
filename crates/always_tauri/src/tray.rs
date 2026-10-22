//! System tray menu builder for the Always daemon GUI.
//!
//! The tray icon provides quick access to the two most-common controls:
//! Pause / Resume and Auto-Enter (a check item).  It also exposes Settings,
//! Onboarding and Quit.
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
use tauri::menu::{CheckMenuItem, Menu, MenuBuilder, MenuItem};
#[cfg(not(target_os = "macos"))]
use tauri::tray::{TrayIcon, TrayIconBuilder, TrayIconEvent};
#[cfg(not(target_os = "macos"))]
use tauri::{AppHandle, Wry};
#[cfg(not(target_os = "macos"))]
use tokio::sync::mpsc;

#[cfg(not(target_os = "macos"))]
use crate::uds_client::{ClientEvent, DaemonCommand, DaemonEvent, DaemonState, UdsClient};
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
    let uds_for_click = Arc::clone(&uds);
    let uds_for_menu = Arc::clone(&uds);
    let tray = TrayIconBuilder::new()
        .icon(app.default_window_icon().unwrap().clone())
        .tooltip("Always")
        // Initial state; rebuilt from daemon events (update_menu_loop).
        .menu(&build_menu(app, "Pause", false)?)
        .on_menu_event(move |app, event| {
            let wm = WindowManager::new(app.clone());
            let command = match event.id().as_ref() {
                "pause" => Some(DaemonCommand::TogglePause),
                "auto_enter" => Some(DaemonCommand::ToggleAutoEnter),
                "settings" => {
                    wm.show_settings();
                    None
                }
                "onboarding" => {
                    wm.show_onboarding(ShowOnboardingReason::Reinvoked);
                    None
                }
                "quit" => {
                    app.exit(0);
                    None
                }
                _ => None,
            };
            if let Some(cmd) = command {
                let uds = Arc::clone(&uds_for_menu);
                tauri::async_runtime::spawn(async move {
                    if let Err(e) = uds.send_command(cmd).await {
                        tracing::warn!(error = %e, "tray_menu_command_failed");
                    }
                });
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
/// The tray menu. Every item carries an id handled in `build_tray`'s
/// `on_menu_event`; Auto-Enter is a check item showing the daemon's state.
/// Flat: two entries do not warrant a Settings submenu. A custom Quit, since
/// the predefined one is unsupported (shown disabled) in Linux tray menus.
#[cfg(not(target_os = "macos"))]
fn build_menu(
    app: &AppHandle,
    pause_label: &str,
    auto_enter: bool,
) -> Result<Menu<Wry>, tauri::Error> {
    MenuBuilder::new(app)
        .item(&MenuItem::with_id(
            app,
            "pause",
            pause_label,
            true,
            None::<&str>,
        )?)
        .item(&CheckMenuItem::with_id(
            app,
            "auto_enter",
            "Auto-Enter",
            true,
            auto_enter,
            None::<&str>,
        )?)
        .separator()
        .item(&MenuItem::with_id(
            app,
            "settings",
            "Settings…",
            true,
            None::<&str>,
        )?)
        .item(&MenuItem::with_id(
            app,
            "onboarding",
            "Onboarding…",
            true,
            None::<&str>,
        )?)
        .separator()
        .item(&MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?)
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

        let label = match state {
            DaemonState::Listening => "Pause",
            DaemonState::Paused => "Resume",
            DaemonState::Processing => "Pause",
            DaemonState::Unknown => "Pause",
        };

        if let Ok(menu) = build_menu(app, label, auto_enter) {
            if let Err(e) = tray.set_menu(Some(menu)) {
                tracing::warn!(error = %e, "tray_set_menu_failed");
            }
        }
    }
}
