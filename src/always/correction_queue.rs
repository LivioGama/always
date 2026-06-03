//! Pending-corrections review queue.
//!
//! The passive clipboard watcher does not write to the glossary
//! directly — that would silently mutate user data based on a
//! probabilistic match. Instead it appends candidate
//! `(wrong → right)` pairs to this queue, the menu bar app shows a
//! badge with the pending count, and the user explicitly approves or
//! rejects each one.
//!
//! Persisted at `~/.always/pending_corrections.json` so the queue
//! survives daemon restarts. Entries older than [`STALE_AFTER`] are
//! pruned on every load — a forgotten correction that nobody acted on
//! within a week is almost certainly noise.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::always::correction::CorrectionPair;

/// Pending corrections older than this are dropped on load.
pub const STALE_AFTER: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// Hard cap on queue size — past this we evict the oldest unreviewed
/// entry on insert.
pub const MAX_PENDING: usize = 100;

/// Where the pending correction queue triggered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CorrectionSource {
    /// User pressed the correction-capture hotkey.
    Hotkey,
    /// Passive clipboard watcher noticed a re-copy that resembles the
    /// most recent paste.
    Passive,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingCorrection {
    pub id: Uuid,
    pub pair: CorrectionPair,
    /// Unix-millis timestamp of when the candidate was queued.
    pub queued_at_unix_ms: u64,
    pub source: CorrectionSource,
}

impl PendingCorrection {
    fn new(pair: CorrectionPair, source: CorrectionSource) -> Self {
        Self {
            id: Uuid::new_v4(),
            pair,
            queued_at_unix_ms: now_unix_ms(),
            source,
        }
    }
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// In-memory queue + on-disk mirror.
///
/// Locking strategy: every public method acquires a single
/// `parking_lot::Mutex` over the entire `Inner` value; persistence
/// happens *inside* the lock so the file always reflects the current
/// in-memory state. The queue is small (≤ MAX_PENDING entries), so
/// holding the lock across an fs write is fine.
#[derive(Clone)]
pub struct CorrectionQueue {
    inner: Arc<Mutex<Inner>>,
}

struct Inner {
    pending: HashMap<Uuid, PendingCorrection>,
    persistence_path: PathBuf,
}

impl CorrectionQueue {
    /// Create a queue rooted at the user's `~/.always/pending_corrections.json`.
    pub fn new_for_user() -> Result<Self> {
        let path = default_persistence_path()
            .ok_or_else(|| anyhow::anyhow!("could not resolve ~/.always for queue"))?;
        Self::with_path(path)
    }

    /// Create a queue with an explicit persistence path. Used by tests.
    pub fn with_path(path: PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let pending = load_from_disk(&path).unwrap_or_default();
        let inner = Inner {
            pending,
            persistence_path: path,
        };
        let q = Self {
            inner: Arc::new(Mutex::new(inner)),
        };
        q.prune_stale_in_place();
        Ok(q)
    }

    /// Append a new candidate to the queue. Returns the assigned ID
    /// so callers can refer to it via UDS events. If `pair.right` is
    /// already pending against the same `pair.wrong`, returns the
    /// existing ID instead of creating a duplicate.
    pub fn add(&self, pair: CorrectionPair, source: CorrectionSource) -> Result<Uuid> {
        let mut inner = self.inner.lock();
        if let Some(existing) = inner
            .pending
            .values()
            .find(|p| p.pair == pair)
            .map(|p| p.id)
        {
            return Ok(existing);
        }

        // Evict oldest if at capacity.
        if inner.pending.len() >= MAX_PENDING
            && let Some(oldest_id) = inner
                .pending
                .iter()
                .min_by_key(|(_, p)| p.queued_at_unix_ms)
                .map(|(id, _)| *id)
        {
            inner.pending.remove(&oldest_id);
        }

        let pending = PendingCorrection::new(pair, source);
        let id = pending.id;
        inner.pending.insert(id, pending);
        Self::persist_locked(&inner);
        Ok(id)
    }

    /// Snapshot of all pending corrections, sorted oldest-first so the
    /// UI shows the longest-waiting items at the top.
    pub fn list(&self) -> Vec<PendingCorrection> {
        let inner = self.inner.lock();
        let mut out: Vec<PendingCorrection> = inner.pending.values().cloned().collect();
        out.sort_by_key(|p| p.queued_at_unix_ms);
        out
    }

    /// Remove an entry from the queue and return it.
    pub fn take(&self, id: Uuid) -> Option<PendingCorrection> {
        let mut inner = self.inner.lock();
        let removed = inner.pending.remove(&id);
        if removed.is_some() {
            Self::persist_locked(&inner);
        }
        removed
    }

    /// Drop entries older than [`STALE_AFTER`].
    pub fn prune_stale_in_place(&self) {
        let mut inner = self.inner.lock();
        let cutoff = now_unix_ms().saturating_sub(STALE_AFTER.as_millis() as u64);
        let before = inner.pending.len();
        inner.pending.retain(|_, p| p.queued_at_unix_ms >= cutoff);
        let pruned = before - inner.pending.len();
        if pruned > 0 {
            tracing::info!(pruned, "correction_queue_pruned");
            Self::persist_locked(&inner);
        }
    }

    /// Drop everything.
    pub fn clear(&self) -> Result<()> {
        let mut inner = self.inner.lock();
        inner.pending.clear();
        Self::persist_locked(&inner);
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.inner.lock().pending.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.lock().pending.is_empty()
    }

    fn persist_locked(inner: &Inner) {
        let entries: Vec<&PendingCorrection> = inner.pending.values().collect();
        let json = match serde_json::to_string_pretty(&entries) {
            Ok(j) => j,
            Err(e) => {
                tracing::error!(error = %e, "correction_queue_serialize_failed");
                return;
            }
        };
        // Atomic replace: write to a sibling temp file then rename over the
        // target. A crash/OOM-kill/power-loss between truncate and full
        // write would otherwise leave a partial/empty file that fails to
        // parse on next start, silently dropping the entire review queue.
        let tmp = inner.persistence_path.with_extension("json.tmp");
        if let Err(e) = std::fs::write(&tmp, &json) {
            tracing::error!(error = %e, path = %tmp.display(), "correction_queue_persist_failed");
            return;
        }
        if let Err(e) = std::fs::rename(&tmp, &inner.persistence_path) {
            tracing::error!(error = %e, path = %inner.persistence_path.display(), "correction_queue_persist_failed");
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

fn load_from_disk(path: &std::path::Path) -> Option<HashMap<Uuid, PendingCorrection>> {
    let content = std::fs::read_to_string(path).ok()?;
    if content.trim().is_empty() {
        return Some(HashMap::new());
    }
    let entries: Vec<PendingCorrection> = serde_json::from_str(&content)
        .map_err(|e| {
            tracing::warn!(error = %e, path = %path.display(), "correction_queue_parse_failed");
            e
        })
        .ok()?;
    let mut map = HashMap::with_capacity(entries.len());
    for e in entries {
        map.insert(e.id, e);
    }
    Some(map)
}

fn default_persistence_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".always").join("pending_corrections.json"))
}

/// Process-wide singleton, lazily initialized on first access.
///
/// Returning a `Result<&'static>` is awkward, so we hold the queue
/// inside a `OnceLock<Result<…>>` and surface initialization failures
/// to callers. The `_for_user` constructor only fails if the home
/// directory can't be resolved — extremely unusual outside of CI
/// sandboxes.
static GLOBAL_QUEUE: std::sync::OnceLock<Result<CorrectionQueue, String>> =
    std::sync::OnceLock::new();

pub fn global_queue() -> Result<&'static CorrectionQueue> {
    let entry = GLOBAL_QUEUE.get_or_init(|| {
        CorrectionQueue::new_for_user().map_err(|e| format!("init pending-corrections queue: {e}"))
    });
    match entry {
        Ok(q) => Ok(q),
        Err(msg) => anyhow::bail!("{msg}"),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_path() -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-tmp")
            .join(format!("queue-{}-{}.json", std::process::id(), n));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let _ = std::fs::remove_file(&path);
        path
    }

    fn pair(w: &str, r: &str) -> CorrectionPair {
        CorrectionPair {
            wrong: w.to_string(),
            right: r.to_string(),
        }
    }

    #[test]
    fn add_then_list_returns_the_pair() {
        let q = CorrectionQueue::with_path(temp_path()).unwrap();
        let _ = q
            .add(pair("kuburnetes", "kubernetes"), CorrectionSource::Hotkey)
            .unwrap();
        let listed = q.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].pair.wrong, "kuburnetes");
        assert_eq!(listed[0].pair.right, "kubernetes");
        assert_eq!(listed[0].source, CorrectionSource::Hotkey);
    }

    #[test]
    fn duplicate_pair_returns_existing_id() {
        let q = CorrectionQueue::with_path(temp_path()).unwrap();
        let id1 = q
            .add(pair("a-foo", "afoo"), CorrectionSource::Passive)
            .unwrap();
        let id2 = q
            .add(pair("a-foo", "afoo"), CorrectionSource::Hotkey)
            .unwrap();
        assert_eq!(id1, id2);
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn take_removes_and_returns_entry() {
        let q = CorrectionQueue::with_path(temp_path()).unwrap();
        let id = q
            .add(pair("kuburnetes", "kubernetes"), CorrectionSource::Hotkey)
            .unwrap();
        let taken = q.take(id).expect("entry should exist");
        assert_eq!(taken.pair.right, "kubernetes");
        assert_eq!(q.len(), 0);
    }

    #[test]
    fn take_unknown_id_returns_none() {
        let q = CorrectionQueue::with_path(temp_path()).unwrap();
        assert!(q.take(Uuid::new_v4()).is_none());
    }

    #[test]
    fn persistence_round_trips() {
        let path = temp_path();
        {
            let q = CorrectionQueue::with_path(path.clone()).unwrap();
            q.add(pair("kuburnetes", "kubernetes"), CorrectionSource::Hotkey)
                .unwrap();
            q.add(pair("doker", "docker"), CorrectionSource::Passive)
                .unwrap();
        }
        let q2 = CorrectionQueue::with_path(path).unwrap();
        assert_eq!(q2.len(), 2);
        let listed = q2.list();
        assert!(listed.iter().any(|p| p.pair.right == "kubernetes"));
        assert!(listed.iter().any(|p| p.pair.right == "docker"));
    }

    #[test]
    fn clear_empties_and_persists() {
        let path = temp_path();
        let q = CorrectionQueue::with_path(path.clone()).unwrap();
        q.add(pair("kuburnetes", "kubernetes"), CorrectionSource::Hotkey)
            .unwrap();
        q.clear().unwrap();
        assert!(q.is_empty());
        let q2 = CorrectionQueue::with_path(path).unwrap();
        assert!(q2.is_empty());
    }

    #[test]
    fn list_sorted_oldest_first() {
        let q = CorrectionQueue::with_path(temp_path()).unwrap();
        let id_a = q
            .add(pair("aaa1", "aaaa"), CorrectionSource::Hotkey)
            .unwrap();
        // Force ordering by sleeping briefly — UNIX_MS resolution is high
        // but we can still hit the same millisecond on fast machines.
        std::thread::sleep(Duration::from_millis(2));
        let id_b = q
            .add(pair("bbb1", "bbbb"), CorrectionSource::Hotkey)
            .unwrap();
        let listed = q.list();
        assert_eq!(listed[0].id, id_a);
        assert_eq!(listed[1].id, id_b);
    }
}
