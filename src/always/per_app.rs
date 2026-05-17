//! Per-application settings overlay. The Swift app sends
//! `NotifyFocusedAppChanged { bundle_id }` whenever the user switches
//! focus; the daemon keeps `pause::current_app()` updated and consults
//! `effective_auto_enter()` / `effective_paused()` to decide the actual
//! runtime state for that app. Stored as a single JSON blob in the
//! `per_app_settings_json` preference so we don't have to add a new
//! table.
//!
//! Caching: the previous implementation re-opened the SQLite DB, ran a
//! query, and re-parsed the JSON on every focus change, every paste,
//! and every auto-enter resolution. With rapid app switching that
//! produced visible DB thrashing. Now we cache the parsed overrides in
//! memory and invalidate via `invalidate_cache()` — called from the
//! `set_preference` write path for `per_app_settings_json` so the
//! Settings UI's "edit overlay" remains a 1-frame change.

use std::collections::HashMap;
use std::sync::LazyLock;

use parking_lot::RwLock;
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

/// Cached parsed overrides. `None` = "not yet loaded"; populated on
/// first call to `load()` after process start or after a write.
static CACHE: LazyLock<RwLock<Option<AppOverrides>>> = LazyLock::new(|| RwLock::new(None));

/// Force a re-read on the next `load()` call. Called from the
/// `set_preference("per_app_settings_json", ...)` path so settings UI
/// edits take effect on the very next focus change.
pub fn invalidate_cache() {
    *CACHE.write() = None;
}

/// Load the JSON overlay from preferences. Returns an empty map on any
/// failure so callers don't have to special-case missing keys. Cached
/// after first call.
pub fn load() -> AppOverrides {
    if let Some(cached) = CACHE.read().as_ref() {
        return cached.clone();
    }
    let parsed = load_from_db();
    *CACHE.write() = Some(parsed.clone());
    parsed
}

fn load_from_db() -> AppOverrides {
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
