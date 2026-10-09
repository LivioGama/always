//! Always Tauri — system tray application that talks to the Always daemon
//! over Unix Domain Socket.
//!
#![warn(clippy::print_stdout, clippy::print_stderr)]

use std::sync::Arc;

use tauri::Manager;

mod tray;
mod uds_client;
mod window_manager;

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

            let uds = Arc::new(uds_client::UdsClient::new(socket_path, None));

            // ── WindowManager ────────────────────────────────────────
            let wm = Arc::new(WindowManager::new(app.handle().clone()));

            // ── System Tray ──────────────────────────────────────────
            match tray::build_tray(app.handle(), &uds) {
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