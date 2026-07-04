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
//!
//! Cross-process freshness: `invalidate_cache()` only clears the
//! *calling* process's static `CACHE`. The CLI and the daemon are
//! separate processes, so a CLI `config set`/`config reset` would
//! otherwise leave the running daemon serving a stale allowlist until
//! restart. To pick up cross-process edits, the cache is keyed on the
//! DB file's mtime: each `load()` cheaply `stat`s the DB and re-reads
//! the (tiny) blob when the mtime changed. That's one `stat()` per
//! call instead of a full open+query+parse, so the hot path stays
//! cheap while a CLI edit is reflected on the very next focus change.

use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::SystemTime;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::db;

/// Always's own macOS bundle identifier (mirrors
/// `FocusedAppMonitor.ownBundleId` in Swift and `CFBundleIdentifier`
/// in `Always/Info.plist`). The daemon refuses to write an override
/// for this bundle id so the user can never accidentally add Always
/// to its own resumed-app allowlist.
pub const ALWAYS_OWN_BUNDLE_IDS: &[&str] = &["com.always", "com.always.v2", "com.always.v3", "com.alwaysapp"];

/// Return true when a bundle id belongs to an Always GUI process.
pub fn is_own_bundle_id(bundle_id: &str) -> bool {
    ALWAYS_OWN_BUNDLE_IDS.contains(&bundle_id)
}

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

/// Cached parsed overrides plus the freshness signal they were read
/// at. `overrides == None` means "not yet loaded". `db_mtime` is the
/// DB file's modification time captured when the blob was parsed; a
/// differing mtime on the next `load()` (e.g. after a write from
/// *another* process) forces a re-read.
#[derive(Default)]
struct CacheState {
    overrides: Option<AppOverrides>,
    db_mtime: Option<SystemTime>,
}

/// Cached parsed overrides. Populated on first call to `load()` after
/// process start, and re-read whenever the DB file's mtime changes.
static CACHE: LazyLock<RwLock<CacheState>> = LazyLock::new(|| RwLock::new(CacheState::default()));

/// Freshness signal for the preferences DB: the latest modification
/// time across the main DB file and its WAL sidecar. Used so that when
/// the CLI writes per-app settings, the daemon sees a newer mtime and
/// re-reads. In WAL mode an `UPDATE` lands in `always.db-wal` first and
/// only touches `always.db` on checkpoint, so we take the max of both
/// — that catches the change immediately regardless of checkpoint
/// timing. Returns `None` if neither file can be stat'd (in which case
/// `load()` conservatively re-reads).
fn db_mtime() -> Option<SystemTime> {
    let db = crate::config::db_path();
    let wal = {
        let mut p = db.clone().into_os_string();
        p.push("-wal");
        std::path::PathBuf::from(p)
    };
    let stat = |p: &std::path::Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    [stat(&db), stat(&wal)].into_iter().flatten().max()
}

/// Force a re-read on the next `load()` call. Called from the
/// `set_preference("per_app_settings_json", ...)` path so settings UI
/// edits in *this* process take effect on the very next focus change.
/// Cross-process edits are handled by the mtime check in `load()`.
pub fn invalidate_cache() {
    *CACHE.write() = CacheState::default();
}

/// Test-only: inject parsed overrides without touching SQLite.
#[cfg(test)]
pub fn set_cache_for_test(overrides: AppOverrides) {
    *CACHE.write() = CacheState {
        overrides: Some(overrides),
        db_mtime: db_mtime(),
    };
}

/// Load the JSON overlay from preferences. Returns an empty map on any
/// failure so callers don't have to special-case missing keys. Cached
/// after first call, but re-read when the DB file's mtime changes so a
/// cross-process edit (CLI `config set`/`reset`) is picked up.
pub fn load() -> AppOverrides {
    let mtime = db_mtime();
    {
        let cache = CACHE.read();
        if let Some(cached) = cache.overrides.as_ref()
            && cache.db_mtime == mtime
        {
            return cached.clone();
        }
    }
    let parsed = load_from_db();
    *CACHE.write() = CacheState {
        overrides: Some(parsed.clone()),
        db_mtime: mtime,
    };
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
        // macOS has a focused-app monitor and paste target, so stay
        // conservative until it reports context. CLI-only platforms do
        // not send focus events; treating "no focused app" as paused
        // would leave the daemon permanently muted.
        return cfg!(target_os = "macos");
    };
    // Always itself is never on the allowlist — we'd be transcribing
    // into Settings. Treat as paused regardless of any stale override
    // that might still be sitting in the DB.
    if is_own_bundle_id(&bundle) {
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
            if is_own_bundle_id(k) {
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
    if is_own_bundle_id(bundle_id) {
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
