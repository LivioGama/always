//! Always Tauri — GUI frontend for the Always speech-to-text daemon,
//! talking to it over a Unix Domain Socket.
//!
//! macOS: the Swift menu-bar app (`Always.app`) is the only menu-bar
//! surface. Its "Settings…" item launches/focuses the bundled
//! `always-tauri` binary, whose windows serve Settings and Onboarding;
//! the tray icon is gated off in `tray` (`build_tray` is a macOS stub) so
//! the user never sees a duplicate menu-bar item. Always-tauri keeps a
//! regular Dock icon (right-click → Quit is the escape hatch), and Dock /
//! Finder / `open` re-opens surface the Settings window — the same
//! "click the icon → settings" affordance the tray menu provides
//! elsewhere.
//!
//! Other desktops: system-tray mode, same as before.

#![warn(clippy::print_stdout, clippy::print_stderr)]

use std::sync::Arc;

use tauri::{Emitter, Manager};

mod commands;
mod tray;
mod uds_client;
mod window_manager;

use uds_client::ClientEvent;
use window_manager::WindowManager;

fn main() {
    // Initialise tracing — same env-filter pattern the daemon uses.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .or_else(|_| tracing_subscriber::EnvFilter::try_new("info"))
                .unwrap(),
        )
        .init();

    tracing::info!("tauri_builder_start");
    let app_builder = tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            commands::cli,
            commands::send_uds_command,
            commands::uds_status,
            commands::app_file_path,
            commands::read_app_file,
            commands::write_app_file,
            commands::history_file_path,
            commands::read_history,
            commands::clear_history,
            commands::open_url,
            commands::open_path,
            commands::reveal_path,
            commands::app_version,
            commands::window_label,
            commands::platform,
            commands::show_settings_window,
            commands::show_onboarding_window,
            commands::finish_onboarding,
        ])
        // Closing onboarding counts as skipping it: hide instead of destroy
        // (the tray can re-open it), show Settings, and let the settings
        // webview record completion.
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "onboarding" {
                    api.prevent_close();
                    let app = window.app_handle();
                    let wm = WindowManager::new(app.clone());
                    wm.hide_onboarding();
                    wm.show_settings();
                    let _ = app.emit_to("settings", "onboarding_dismissed", ());
                }
            }
        })
        // ── Auto-launch (enabled via system settings outside Tauri) ──
        // The daemon itself provides auto-start via the `start` command
        // with PID-guard.  We may add `tauri-plugin-autostart` once it
        // reaches a stable release for Tauri v2.
        .setup(|app| {
            tracing::info!("tauri_setup_entered");
            // ── Windows ──────────────────────────────────────────────
            // Both start hidden (tauri.conf.json). The settings webview
            // shows one once it knows whether onboarding is done — so a
            // fresh launch by the Swift menu bar surfaces the right
            // window without any extra flag passing.

            // Start the sibling daemon before connecting. `always start` is
            // detached and single-instance safe, and leaves the UI available
            // to repair settings if the daemon cannot start.
            let resource_dir = app.path().resource_dir().ok();
            if let Err(error) = commands::start_daemon(resource_dir.as_deref()) {
                tracing::warn!(error = %error, "daemon_start_failed");
            }

            // ── UDS Client ───────────────────────────────────────────
            let socket_path = uds_client::resolve_socket_path();
            tracing::info!(path = %socket_path.display(), "uds_client_init");

            let uds = Arc::new(uds_client::UdsClient::new(
                socket_path,
                None::<fn(ClientEvent)>,
            ));

            // Forward daemon events + connection state to every webview —
            // mirrors StateMonitor's broadcast in the Swift app.
            let emit_handle = app.handle().clone();
            uds.on_event(move |ev| match ev {
                ClientEvent::Event(line) => {
                    let _ = emit_handle.emit("uds_event", line);
                }
                ClientEvent::Connected => {
                    let _ = emit_handle.emit("uds_connected", ());
                }
                ClientEvent::Disconnected => {
                    let _ = emit_handle.emit("uds_disconnected", ());
                }
            });
            app.manage(Arc::clone(&uds));

            // ── System Tray ──────────────────────────────────────────
            // Built on every desktop so `main` stays platform-uniform:
            // non-macOS gets the tray menu with Settings/Onboarding
            // entries, macOS gets a no-op stub (Swift owns that surface).
            match tray::build_tray(app.handle(), Arc::clone(&uds)) {
                Ok(_) => tracing::info!("tray_built"),
                Err(e) => {
                    tracing::warn!(error = %e, "failed_to_build_tray");
                }
            }

            Ok(())
        });

    tracing::info!("tauri_run_start");

    // macOS has no tray icon (the Swift menu bar owns that surface), so
    // give it a run-event loop instead: Dock icon clicks and Finder/`open`
    // activations on the already-running process surface Settings — the
    // "focus its window if already running" half of the Swift contract.
    #[cfg(target_os = "macos")]
    {
        app_builder
            .build(tauri::generate_context!())
            .expect("error while building the Always Tauri app")
            .run(|app, event| {
                if let tauri::RunEvent::Reopen { has_visible_windows, .. } = event {
                    tracing::info!(has_visible_windows, "macos_reopen_show_settings");
                    WindowManager::new(app.clone()).show_settings();
                }
            });
    }

    #[cfg(not(target_os = "macos"))]
    {
        app_builder
            .run(tauri::generate_context!())
            .expect("error while running the Always Tauri app");
    }
}
