//! Per-application settings overlay. The Swift app sends
//! `NotifyFocusedAppChanged { bundle_id }` whenever the user switches
//! focus; the daemon keeps `pause::current_app()` updated and consults
//! `effective_auto_enter()` / `effective_paused()` to decide the actual
//! runtime state for that app. Stored as a single JSON blob in the
//! `per_app_settings_json` preference so we don't have to add a new
//! table.
//!
//! **Allowlist semantics (2026-05-17):** the default for any app
//! WITHOUT an override is `paused = true`. An override of `paused:
//! false` is what marks an app as "voice typing is active here". This
//! inverts the old default (active everywhere, opt-out per app) into
//! an explicit allowlist — the user resumes the specific apps they
//! want voice typing in. The global pause toggle still force-pauses
//! every app regardless of overrides (idle auto-pause, audio output,
//! mic conflicts all flip the master switch, not the per-app rules).
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

/// Always's own macOS bundle identifier (mirrors
/// `FocusedAppMonitor.ownBundleId` in Swift and `CFBundleIdentifier`
/// in `Always/Info.plist`). The daemon refuses to write an override
/// for this bundle id so the user can never accidentally add Always
/// to its own resumed-app allowlist.
pub const ALWAYS_OWN_BUNDLE_ID: &str = "com.always";

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

/// Resolve effective paused state for the currently-focused app
/// **independently of MASTER**. Callers in `pause.rs` combine this
/// with `MASTER_PAUSED` to produce the effective gating value.
///
/// Default is `true` (paused) for any app without an override. An
/// override of `paused: false` is what makes an app's voice typing
/// active. See module doc for the design rationale.
pub fn effective_paused_for_current_app() -> bool {
    let Some(bundle) = super::pause::current_app() else {
        // No focused app reported yet — be conservative and stay
        // paused so we don't capture audio before we know whose
        // typing context we'd be pasting into.
        return true;
    };
    // Always itself is never on the allowlist — we'd be transcribing
    // into Settings. Treat as paused regardless of any stale override
    // that might still be sitting in the DB.
    if bundle == ALWAYS_OWN_BUNDLE_ID {
        return true;
    }
    load().get(&bundle).and_then(|o| o.paused).unwrap_or(true)
}

/// True if the supplied bundle id is on the user's resumed-app
/// allowlist (i.e. the stored override sets `paused: false`).
pub fn is_app_resumed(bundle_id: &str) -> bool {
    matches!(load().get(bundle_id).and_then(|o| o.paused), Some(false))
}

/// Returns every bundle id whose override explicitly sets
/// `paused: false`. This is the user's "voice typing allowlist".
///
/// Always's own bundle is filtered out defensively — a DB written by
/// an older daemon build (before `set_app_paused_override` started
/// rejecting it) might still hold the entry. Filtering on read keeps
/// the UI honest without requiring a migration.
pub fn resumed_apps() -> Vec<String> {
    let mut out: Vec<String> = load()
        .iter()
        .filter_map(|(k, v)| {
            if k == ALWAYS_OWN_BUNDLE_ID {
                return None;
            }
            (v.paused == Some(false)).then(|| k.clone())
        })
        .collect();
    out.sort();
    out
}

/// Write a `paused: Some(v)` override for `bundle_id` (or clear it
/// with `paused: None`) and persist back to the DB. Invalidates the
/// in-memory cache so the next `load()` call re-reads from disk.
///
/// Returns `Ok(())` when the write reached SQLite. Best-effort —
/// callers that care should also check `is_app_resumed` afterwards.
pub fn set_app_paused_override(bundle_id: &str, paused: Option<bool>) -> anyhow::Result<()> {
    // Hard-stop: Always must never appear in its own allowlist —
    // resuming voice typing into Settings would paste into the wrong
    // window and confuse every UI surface that surfaces "active in X".
    if bundle_id == ALWAYS_OWN_BUNDLE_ID {
        anyhow::bail!("refusing to write per-app override for Always's own bundle id");
    }
    let mut overrides = load();
    let entry = overrides.entry(bundle_id.to_string()).or_default();
    entry.paused = paused;
    // Prune entries that hold nothing useful — keeps the JSON blob
    // small and the Settings list from showing rows that say
    // "no override set".
    if entry.paused.is_none() && entry.auto_enter.is_none() && entry.auto_enter_delay_ms.is_none() {
        overrides.remove(bundle_id);
    }
    let json = serde_json::to_string(&overrides)?;
    let conn = crate::db::open()?;
    crate::db::set_preference(&conn, "per_app_settings_json", &json)?;
    invalidate_cache();
    Ok(())
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
