//! Per-application settings overlay. The Swift app sends
//! `NotifyFocusedAppChanged { bundle_id }` whenever the user switches
//! focus; the daemon keeps `pause::current_app()` updated and consults
//! `effective_auto_enter()` / `effective_paused()` to decide the actual
//! runtime state for that app. Stored as a single JSON blob in the
//! `per_app_settings_json` preference so we don't have to add a new
//! table.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::db;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppOverride {
    /// If `Some`, this app overrides the global auto-enter toggle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_enter: Option<bool>,
    /// If `Some(true)`, daemon stays paused while this app is focused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused: Option<bool>,
    /// If `Some`, this app overrides the global auto-enter delay (ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_enter_delay_ms: Option<u32>,
}

pub type AppOverrides = HashMap<String, AppOverride>;

/// Load the JSON overlay from preferences. Returns an empty map on any
/// failure so callers don't have to special-case missing keys.
pub fn load() -> AppOverrides {
    let conn = match db::open() {
        Ok(c) => c,
        Err(_) => return HashMap::new(),
    };
    let prefs = match db::get_preferences(&conn) {
        Ok(p) => p,
        Err(_) => return HashMap::new(),
    };
    let Some(raw) = prefs.per_app_settings_json else {
        return HashMap::new();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

/// Resolve the effective auto-enter value for the currently-focused
/// app, falling back to the daemon's global state when no override
/// exists.
pub fn effective_auto_enter(global: bool) -> bool {
    let Some(bundle) = super::pause::current_app() else {
        return global;
    };
    load()
        .get(&bundle)
        .and_then(|o| o.auto_enter)
        .unwrap_or(global)
}

/// Resolve effective paused state.
pub fn effective_paused(global: bool) -> bool {
    let Some(bundle) = super::pause::current_app() else {
        return global;
    };
    load()
        .get(&bundle)
        .and_then(|o| o.paused)
        .unwrap_or(global)
}

/// Resolve effective auto-enter delay (ms).
pub fn effective_auto_enter_delay_ms(global: u32) -> u32 {
    let Some(bundle) = super::pause::current_app() else {
        return global;
    };
    load()
        .get(&bundle)
        .and_then(|o| o.auto_enter_delay_ms)
        .unwrap_or(global)
}
