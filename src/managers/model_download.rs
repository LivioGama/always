//! Shared model download and verification utilities.
//!
//! Provides SHA256 verification and verify-cache helpers for both
//! STT and LLM model registries.

use anyhow::Result;
use parking_lot::Mutex;
use sha2::Digest;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// On-disk row of the persisted verify cache (`.verify-cache.json`).
/// `SystemTime` flattens to unix seconds so the JSON stays portable.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct PersistedVerdict {
    pub path: PathBuf,
    pub len: u64,
    pub mtime_unix_secs: u64,
    pub verdict: bool,
}

/// SHA256 verification verdict cache: `(path, file_len, mtime) -> matched`.
/// Wrapped in Arc for sharing across the registry.
pub type VerifyCache = Arc<Mutex<std::collections::HashMap<(PathBuf, u64, SystemTime), bool>>>;

/// Compute SHA256 hash of a file.
pub fn compute_sha256(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = sha2::Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        Digest::update(&mut hasher, &buffer[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Verify SHA256 hash of a file against expected value.
pub fn verify_sha256(path: &Path, expected: Option<&str>, model_id: &str) -> Result<()> {
    let Some(expected) = expected else {
        return Ok(());
    };
    match compute_sha256(path) {
        Ok(actual) if actual == expected => {
            tracing::info!(model = %model_id, "model_sha256_verified");
            Ok(())
        }
        Ok(actual) => {
            tracing::warn!(
                model = %model_id,
                expected = %expected,
                actual = %actual,
                "model_sha256_mismatch"
            );
            let _ = fs::remove_file(path);
            anyhow::bail!(
                "Download verification failed for model {model_id}: file is corrupt. Please retry."
            )
        }
        Err(e) => {
            let _ = fs::remove_file(path);
            anyhow::bail!("Failed to verify download for model {model_id}: {e}. Please retry.")
        }
    }
}

/// Load verify cache from disk.
pub fn load_verify_cache(cache_path: &Path, cache: &VerifyCache) {
    let Ok(raw) = fs::read_to_string(cache_path) else {
        return;
    };
    let Ok(entries) = serde_json::from_str::<Vec<PersistedVerdict>>(&raw) else {
        tracing::warn!("model_verify_cache_corrupt_discarding");
        return;
    };
    let mut cache = cache.lock();
    for e in entries {
        let mtime = SystemTime::UNIX_EPOCH + Duration::from_secs(e.mtime_unix_secs);
        cache.insert((e.path, e.len, mtime), e.verdict);
    }
    tracing::info!(entries = cache.len(), "model_verify_cache_loaded");
}

/// Save verify cache to disk (atomic tmp + rename).
pub fn save_verify_cache(cache_path: &Path, cache: &VerifyCache) {
    let entries: Vec<PersistedVerdict> = cache
        .lock()
        .iter()
        .map(|((path, len, mtime), &verdict)| PersistedVerdict {
            path: path.clone(),
            len: *len,
            mtime_unix_secs: mtime
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            verdict,
        })
        .collect();
    let Ok(json) = serde_json::to_string(&entries) else {
        return;
    };
    let tmp = cache_path.with_extension("json.tmp");
    if fs::write(&tmp, json).is_ok() {
        let _ = fs::rename(&tmp, cache_path);
    }
}
