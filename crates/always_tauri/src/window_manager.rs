//! Window management for the Always GUI.
//!
//! Manages the two Tauri windows:
//!
//! * **settings** — 900 × 700, always visible.  Opened by the tray menu
//!   "Settings" item or when the daemon is already running.
//! * **onboarding** — 580 × 640, initially hidden.  Shown once when the
//!   app is first launched so the user can enter their voice profile.
//!
//! The window names match the identifiers in `tauri.conf.json`.

use tauri::AppHandle;
use tauri::Manager;

/// Which kind of onboarding flow to present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShowOnboardingReason {
    /// First-ever launch — show the full intro flow.
    FirstLaunch,
    /// User re-invoked onboarding from the tray menu.
    Reinvoked,
}

/// Manages the settings and onboarding windows.
#[derive(Clone)]
pub struct WindowManager {
    app: AppHandle,
}

impl WindowManager {
    /// Create a new WindowManager backed by the Tauri app handle.
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }

    /// Show the settings window (always visible).
    pub fn show_settings(&self) {
        if let Some(win) = self.app.get_webview_window("settings") {
            win.show().ok();
            win.set_focus().ok();
        } else {
            tracing::warn!("settings window not found — is tauri.conf.json configured?");
        }
    }

    /// Hide the settings window.
    pub fn hide_settings(&self) {
        if let Some(win) = self.app.get_webview_window("settings") {
            win.hide().ok();
        }
    }

    /// Show the onboarding window (initially hidden).
    pub fn show_onboarding(&self, reason: ShowOnboardingReason) {
        if let Some(win) = self.app.get_webview_window("onboarding") {
            win.show().ok();
            win.set_focus().ok();
            // Reload to reflect any state change (e.g. first-launch → reinvoked).
            let _ = reason; // the frontend reads the reason from the event bus
        } else {
            tracing::warn!("onboarding window not found — is tauri.conf.json configured?");
        }
    }

    /// Hide the onboarding window.
    pub fn hide_onboarding(&self) {
        if let Some(win) = self.app.get_webview_window("onboarding") {
            win.hide().ok();
        }
    }

    /// Close all GUI windows.
    pub fn close_all(&self) {
        if let Some(win) = self.app.get_webview_window("settings") {
            win.close().ok();
        }
        if let Some(win) = self.app.get_webview_window("onboarding") {
            win.close().ok();
        }
    }
}