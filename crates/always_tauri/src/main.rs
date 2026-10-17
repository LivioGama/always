//! Always Tauri — system tray application that talks to the Always daemon
//! over Unix Domain Socket.
//!
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

    tauri::Builder::default()
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
        ])
        // ── Auto-launch (enabled via system settings outside Tauri) ──
        // The daemon itself provides auto-start via the `start` command
        // with PID-guard.  We may add `tauri-plugin-autostart` once it
        // reaches a stable release for Tauri v2.
        .setup(|app| {
            // ── Windows ──────────────────────────────────────────────
            // Settings window — always available.
            if let Some(win) = app.get_webview_window("settings") {
                win.show().ok();
                win.set_focus().ok();
            }

            // Onboarding window — initially hidden.
            if let Some(win) = app.get_webview_window("onboarding") {
                win.hide().ok();
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

            // ── WindowManager ────────────────────────────────────────
            let wm = Arc::new(WindowManager::new(app.handle().clone()));

            // ── System Tray ──────────────────────────────────────────
            match tray::build_tray(app.handle(), Arc::clone(&uds)) {
                Ok(t) => {
                    let _ = t;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed_to_build_tray");
                }
            }

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running the Always Tauri app");
}
