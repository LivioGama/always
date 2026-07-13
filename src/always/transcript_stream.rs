//! Transcript stream sink — append-only JSONL of accepted utterances.
//!
//! When `transcript_stream` is enabled, every utterance that survives the
//! full pipeline (filter → hallucination → acoustic → grammar → dedup)
//! is appended as one JSON line to `~/.always/transcripts.jsonl` so
//! external consumers (e.g. an assistant daemon tailing the file) can
//! react to speech without owning a microphone pipeline.
//!
//! Deliberately a file, not the UDS broadcaster: `TranscriptFinal` on the
//! UDS fires before duplicate-paste suppression and ties daemon lifetime
//! to connected clients via the orphan watchdog. A file survives restarts
//! on both sides and O_APPEND single-line writes are atomic for a line
//! reader.
//!
//! Failures here must never break the paste path — errors are logged at
//! warn and swallowed.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Rotate once the stream exceeds this size; the consumer tails live
/// appends, so old content only matters for debugging.
const MAX_STREAM_BYTES: u64 = 10 * 1024 * 1024;

/// Resolve the stream path. `ALWAYS_TRANSCRIPT_STREAM_PATH` is the
/// escape hatch for tests/CI; canonical location sits next to
/// glossary.json in `~/.always`.
pub fn stream_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("ALWAYS_TRANSCRIPT_STREAM_PATH")
        && !p.is_empty()
    {
        return Some(PathBuf::from(p));
    }
    dirs::home_dir().map(|h| h.join(".always").join("transcripts.jsonl"))
}

/// Append one accepted utterance to the stream. Never fails the caller.
pub fn append(text: &str) {
    let Some(path) = stream_path() else {
        tracing::warn!("transcript_stream: no home directory, skipping");
        return;
    };
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    if let Err(err) = append_at(&path, text, ts) {
        tracing::warn!(error = %err, path = %path.display(), "transcript_stream_append_failed");
    }
}

/// Testable core: append `{ts, text, source}` as one JSON line at `path`,
/// rotating to `<path>.old` first when the file is oversized.
fn append_at(path: &Path, text: &str, ts_ms: u64) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    rotate_if_needed(path)?;
    let line = serde_json::json!({"ts": ts_ms, "text": text, "source": "always"});
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    // serde_json escapes embedded newlines, so this is always one line.
    writeln!(file, "{line}")?;
    Ok(())
}

fn rotate_if_needed(path: &Path) -> anyhow::Result<()> {
    let Ok(meta) = std::fs::metadata(path) else {
        return Ok(()); // no file yet
    };
    if meta.len() <= MAX_STREAM_BYTES {
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

    fn temp_stream_path(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "always-transcript-stream-test-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("transcripts.jsonl")
    }

    #[test]
    fn append_writes_one_valid_json_line_per_call() {
        let path = temp_stream_path("append");

        append_at(&path, "Iris, sort my desktop", 1718236800000).unwrap();
        append_at(&path, "second utterance", 1718236801000).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);
        let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["ts"], 1718236800000u64);
        assert_eq!(first["text"], "Iris, sort my desktop");
        assert_eq!(first["source"], "always");
        let second: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(second["text"], "second utterance");
    }

    #[test]
    fn embedded_newlines_stay_on_one_line() {
        let path = temp_stream_path("newlines");

        append_at(&path, "line one\nline two", 1).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 1);
        let parsed: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(parsed["text"], "line one\nline two");
    }

    #[test]
    fn oversized_stream_rotates_to_old() {
        let path = temp_stream_path("rotate");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // Pre-fill beyond the rotation threshold.
        let big = "x".repeat((MAX_STREAM_BYTES + 1) as usize);
        std::fs::write(&path, &big).unwrap();

        append_at(&path, "fresh after rotation", 2).unwrap();

        let mut old = path.as_os_str().to_owned();
        old.push(".old");
        let old = PathBuf::from(old);
        assert!(old.exists(), "oversized file should rotate to .old");
        assert_eq!(std::fs::metadata(&old).unwrap().len(), big.len() as u64);
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content.lines().count(), 1);
        let parsed: serde_json::Value =
            serde_json::from_str(content.lines().next().unwrap()).unwrap();
        assert_eq!(parsed["text"], "fresh after rotation");
    }

    #[test]
    fn env_override_wins_over_home_default() {
        // Env mutation is process-global; this is the only test touching
        // this var, and it restores state before exiting.
        let custom = temp_stream_path("env").to_string_lossy().to_string();
        unsafe { std::env::set_var("ALWAYS_TRANSCRIPT_STREAM_PATH", &custom) };
        let resolved = stream_path();
        unsafe { std::env::remove_var("ALWAYS_TRANSCRIPT_STREAM_PATH") };

        assert_eq!(resolved, Some(PathBuf::from(custom)));
        // Without the override, falls back to ~/.always/transcripts.jsonl.
        let default = stream_path().unwrap();
        assert!(default.ends_with(".always/transcripts.jsonl"));
    }
}
