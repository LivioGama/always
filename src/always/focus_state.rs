//! Persist the last focused app bundle so daemon restarts can restore
//! per-app allowlist state without waiting for a focus-change event.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
struct FocusState {
    bundle_id: Option<String>,
}

fn state_path() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("Always")
        .join("focus-state.json")
}

/// Write the latest focused bundle (or clear when `None`).
pub fn save(bundle_id: Option<&str>) {
    let path = state_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let payload = FocusState {
        bundle_id: bundle_id.map(str::to_string),
    };
    if let Ok(json) = serde_json::to_string(&payload) {
        let _ = fs::write(path, json);
    }
}

/// Read the last persisted bundle, if any.
pub fn load() -> Option<String> {
    let path = state_path();
    let raw = fs::read_to_string(path).ok()?;
    serde_json::from_str::<FocusState>(&raw)
        .ok()
        .and_then(|s| s.bundle_id)
}

/// Restore daemon pause state from disk on startup.
pub fn restore_on_startup() {
    let Some(bundle) = load() else {
        return;
    };
    let (effective, changed) = crate::always::pause::set_current_app_and_recompute(Some(bundle));
    if changed {
        if effective {
            crate::always::event::global_broadcaster().paused_quietly();
        } else {
            crate::always::event::global_broadcaster().resumed_quietly();
        }
        tracing::info!(bundle = ?crate::always::pause::current_app(), effective, "focus_state_restored");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_bundle_id() {
        let dir = std::env::temp_dir().join(format!("always-focus-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("focus-state.json");
        let payload = FocusState {
            bundle_id: Some("com.example.app".to_string()),
        };
        fs::write(&path, serde_json::to_string(&payload).unwrap()).unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        let parsed: FocusState = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed.bundle_id.as_deref(), Some("com.example.app"));
        let _ = fs::remove_dir_all(&dir);
    }
}
