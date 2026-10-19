//! Transcript history — the store behind Settings → History.
//!
//! Every utterance the daemon pastes is appended as one JSON line to
//! `<config_dir>/history.jsonl` (macOS: `~/Library/Application
//! Support/always/history.jsonl`), the same per-instance directory as
//! `always.db` and `prefs.db`, so the GUI finds it through
//! `AppInstance.supportDir` with no plumbing.
//!
//! This used to be read back out of the daily tracing log
//! (`transcription_pasted` with `raw_text`). Release builds keep transcript
//! text out of diagnostic logs (`telemetry::should_log_transcripts`), so on a
//! real install the History tab was always empty. The history is a user
//! feature, local to the machine and cleared by "Delete All"; diagnostic
//! logs stay text-free.
//!
//! Failures here must never break the paste path — errors are logged at
//! warn and swallowed.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

/// File name inside the config dir. The GUI reads the same name.
pub const FILE_NAME: &str = "history.jsonl";

/// Rotate once the file exceeds this size. The panel shows today's last
/// entries, so older content only needs to stay bounded.
const MAX_HISTORY_BYTES: u64 = 2 * 1024 * 1024;

/// Resolve the history path. `ALWAYS_HISTORY_PATH` overrides (tests/CI);
/// otherwise it sits next to `always.db` in [`crate::config::config_dir`],
/// which honours `ALWAYS_CONFIG_DIR` and the dev-instance suffix.
pub fn history_path() -> PathBuf {
    if let Ok(p) = std::env::var("ALWAYS_HISTORY_PATH")
        && !p.is_empty()
    {
        return PathBuf::from(p);
    }
    #[cfg(test)]
    {
        // Unit tests drive the paste pipeline; never touch the real
        // user's history from `cargo test`.
        std::env::temp_dir()
            .join(format!("always-history-unit-{}", std::process::id()))
            .join(FILE_NAME)
    }
    #[cfg(not(test))]
    {
        crate::config::config_dir().join(FILE_NAME)
    }
}

/// Record one pasted utterance. Never fails the caller.
pub fn append(raw: &str, processed: &str) {
    let path = history_path();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    if let Err(err) = append_at(&path, raw, processed, ts) {
        tracing::warn!(error = %err, path = %path.display(), "history_append_failed");
    }
}

/// Testable core: append `{ts, raw_text, processed_text}` as one JSON line,
/// creating the directory on first run and rotating to `<path>.old` first
/// when the file is oversized.
pub fn append_at(path: &Path, raw: &str, processed: &str, ts_ms: u64) -> anyhow::Result<()> {
    if raw.trim().is_empty() && processed.trim().is_empty() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    rotate_if_needed(path)?;
    let line = serde_json::json!({
        "ts": ts_ms,
        "raw_text": raw,
        "processed_text": processed,
    });
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    // serde_json escapes embedded newlines, so this is always one line.
    writeln!(file, "{line}")?;
    Ok(())
}

fn rotate_if_needed(path: &Path) -> anyhow::Result<()> {
    let Ok(meta) = std::fs::metadata(path) else {
        return Ok(()); // no file yet
    };
    if meta.len() <= MAX_HISTORY_BYTES {
        return Ok(());
    }
    let mut old = path.as_os_str().to_owned();
    old.push(".old");
    std::fs::rename(path, PathBuf::from(old))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("always-history-test-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // Nested, not-yet-existing dir: the fresh-install case.
        dir.join("Library/Application Support/always")
            .join(FILE_NAME)
    }

    #[test]
    fn first_append_creates_missing_dir_and_writes_line() {
        let path = temp_path("fresh");
        assert!(!path.parent().unwrap().exists());

        append_at(&path, "bonjour tout le monde", "Bonjour tout le monde.", 42).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 1);
        let v: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(v["ts"], 42u64);
        assert_eq!(v["raw_text"], "bonjour tout le monde");
        assert_eq!(v["processed_text"], "Bonjour tout le monde.");
    }

    #[test]
    fn blank_utterance_is_not_recorded() {
        let path = temp_path("blank");
        append_at(&path, "  ", "", 1).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn embedded_newlines_stay_on_one_line() {
        let path = temp_path("newlines");
        append_at(&path, "a\nb", "A.\nB.", 1).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content.lines().count(), 1);
    }

    #[test]
    fn oversized_history_rotates_to_old() {
        let path = temp_path("rotate");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "x".repeat((MAX_HISTORY_BYTES + 1) as usize)).unwrap();

        append_at(&path, "fresh", "Fresh.", 2).unwrap();

        let mut old = path.as_os_str().to_owned();
        old.push(".old");
        assert!(PathBuf::from(old).exists());
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 1);
    }
}
