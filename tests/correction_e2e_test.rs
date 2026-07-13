//! End-to-end integration test for the manual correction capture pipeline.
//!
//! Covers the on-disk side of `correction::apply_pairs_to_glossary`,
//! the word-level diff in `correction::diff_words`, the persistence of
//! `correction_queue::CorrectionQueue`, and the regression-fix in
//! `text::Vocabulary::apply` (whole-word replacement instead of naive
//! substring).
//!
//! All tests use a per-test scratch directory under `target/test-tmp/`
//! so the user's real `~/.always/` is never touched. `HOME` is
//! overridden via `unsafe { std::env::set_var(...) }` (mandatory on
//! Rust 1.80+).

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

use always::always::correction::{CorrectionPair, apply_pairs_to_glossary, diff_words};
use always::always::correction_queue::{CorrectionQueue, CorrectionSource};

/// Serializes every test that mutates `$HOME`. Cargo runs integration
/// tests on multiple threads by default, but `std::env::set_var` is
/// process-global — racing on it makes tests flaky. Each HOME-touching
/// test acquires this lock for its lifetime.
fn home_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let m = LOCK.get_or_init(|| Mutex::new(()));
    // If a sibling test panicked while holding the lock, the mutex is
    // poisoned but the data inside is `()` — we don't care.
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Per-test scratch directory under `target/test-tmp/`. Mirrors the
/// helper used by the in-tree unit tests in `correction.rs` so we don't
/// pull in `tempfile` or any new dependency.
fn tempdir_under_target(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-tmp")
        .join(format!("e2e-{label}-{pid}-{n}"));
    std::fs::create_dir_all(&path).unwrap();
    path
}

// ---------------------------------------------------------------------------
// (a) apply_pairs_to_glossary — round trip with a curated entry
// ---------------------------------------------------------------------------

#[test]
fn apply_pairs_appends_to_curated_entry_and_is_idempotent() {
    let _g = home_lock();
    let tmp = tempdir_under_target("curated");
    // SAFETY: integration tests inside this file all override HOME; cargo
    // schedules them on the same process serially per binary by default.
    unsafe { std::env::set_var("HOME", &tmp) };

    // Seed a curated glossary entry (frequency 200, one existing
    // mistranscription).
    let glossary_path = tmp.join(".always/glossary.json");
    std::fs::create_dir_all(glossary_path.parent().unwrap()).unwrap();
    std::fs::write(
        &glossary_path,
        r#"[{"term":"Kubernetes","mistranscriptions":["cuber netties"],"frequency":200}]"#,
    )
    .unwrap();

    // Apply a new mistranscription pair targeting the same canonical term.
    let pair = CorrectionPair {
        wrong: "kuburnetes".to_string(),
        right: "Kubernetes".to_string(),
    };
    let written = apply_pairs_to_glossary(std::slice::from_ref(&pair)).unwrap();
    assert_eq!(written, 1, "should have written one new mistranscription");

    let parsed: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&glossary_path).unwrap()).unwrap();
    let arr = parsed.as_array().unwrap();
    assert_eq!(arr.len(), 1, "should not have duplicated the entry");
    let entry = &arr[0];
    assert_eq!(entry["term"].as_str(), Some("Kubernetes"));
    let mistr = entry["mistranscriptions"].as_array().unwrap();
    assert!(
        mistr.iter().any(|v| v.as_str() == Some("cuber netties")),
        "pre-existing mistranscription must survive"
    );
    assert!(
        mistr.iter().any(|v| v.as_str() == Some("kuburnetes")),
        "new mistranscription must be appended"
    );
    // Curated frequency must NOT be overwritten — we only touch the
    // mistranscriptions list.
    assert_eq!(
        entry["frequency"].as_u64(),
        Some(200),
        "curated frequency must be preserved"
    );

    // Re-applying the same pair is a no-op: dedupe should fire.
    let written = apply_pairs_to_glossary(std::slice::from_ref(&pair)).unwrap();
    assert_eq!(written, 0, "second apply should be idempotent");
}

// ---------------------------------------------------------------------------
// (b) apply_pairs_to_glossary — brand-new term creates a fresh entry
// ---------------------------------------------------------------------------

#[test]
fn apply_pairs_creates_new_entry_with_default_frequency() {
    let _g = home_lock();
    let tmp = tempdir_under_target("newterm");
    // SAFETY: see top of file.
    unsafe { std::env::set_var("HOME", &tmp) };

    // No glossary file exists yet — ensure the .always dir is empty.
    let glossary_path = tmp.join(".always/glossary.json");
    if glossary_path.exists() {
        std::fs::remove_file(&glossary_path).unwrap();
    }

    let written = apply_pairs_to_glossary(&[CorrectionPair {
        wrong: "doker".to_string(),
        right: "Docker".to_string(),
    }])
    .unwrap();
    assert_eq!(written, 1);

    let parsed: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&glossary_path).unwrap()).unwrap();
    let arr = parsed.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["term"].as_str(), Some("Docker"));
    assert_eq!(
        arr[0]["frequency"].as_u64(),
        Some(100),
        "fresh entries must default to frequency 100"
    );
    let mistr = arr[0]["mistranscriptions"].as_array().unwrap();
    assert_eq!(mistr.len(), 1);
    assert_eq!(mistr[0].as_str(), Some("doker"));
}

// ---------------------------------------------------------------------------
// (c) diff_words — realistic Whisper substitution
// ---------------------------------------------------------------------------

#[test]
fn diff_words_extracts_single_substitution_from_realistic_sentence() {
    let pairs = diff_words(
        "deploy the kubernetics cluster to production",
        "deploy the kubernetes cluster to production",
    );
    assert_eq!(
        pairs.len(),
        1,
        "expected exactly one correction pair, got {pairs:?}"
    );
    assert_eq!(pairs[0].wrong, "kubernetics");
    assert_eq!(pairs[0].right, "kubernetes");
}

// ---------------------------------------------------------------------------
// (d) CorrectionQueue — persistence round trip
// ---------------------------------------------------------------------------

#[test]
fn correction_queue_persists_across_drops() {
    let tmp = tempdir_under_target("queue");
    let queue_path = tmp.join("pending_corrections.json");

    {
        let q = CorrectionQueue::with_path(queue_path.clone()).unwrap();
        q.add(
            CorrectionPair {
                wrong: "kuburnetes".to_string(),
                right: "Kubernetes".to_string(),
            },
            CorrectionSource::Hotkey,
        )
        .unwrap();
        q.add(
            CorrectionPair {
                wrong: "doker".to_string(),
                right: "Docker".to_string(),
            },
            CorrectionSource::Passive,
        )
        .unwrap();
        q.add(
            CorrectionPair {
                wrong: "supr whisper".to_string(),
                right: "SuperWhisper".to_string(),
            },
            CorrectionSource::Passive,
        )
        .unwrap();
        assert_eq!(q.len(), 3);
    } // drop — file should already be on disk

    // Re-open at the same path.
    let q2 = CorrectionQueue::with_path(queue_path).unwrap();
    assert_eq!(q2.len(), 3, "queue must survive drop + reload");
    let listed = q2.list();
    assert!(listed.iter().any(|p| p.pair.right == "Kubernetes"));
    assert!(listed.iter().any(|p| p.pair.right == "Docker"));
    assert!(listed.iter().any(|p| p.pair.right == "SuperWhisper"));
    // Source field round-trips correctly.
    let kube = listed
        .iter()
        .find(|p| p.pair.right == "Kubernetes")
        .unwrap();
    assert_eq!(kube.source, CorrectionSource::Hotkey);
    let docker = listed.iter().find(|p| p.pair.right == "Docker").unwrap();
    assert_eq!(docker.source, CorrectionSource::Passive);
}

// Vocabulary apply regression test was removed when `text::Vocabulary`
// was deleted in the pipeline-simplification pass. The acoustic glossary
// match is now `text_match::apply_custom_words` (Soundex + Levenshtein
// over `glossary::user_glossary_terms`), tested in `text_match.rs`.
