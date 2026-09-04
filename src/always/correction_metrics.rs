//! Correction effectiveness metrics — atomic counters that track
//! how often corrections are applied, deferred, rejected, or
//! back-corrected.
//!
//! The counters are process-lifetime (not persisted) and are logged
//! periodically via [`log_summary`]. They intentionally do NOT record
//! transcript content — only aggregate counts and the substitution
//! pairs (glossary terms, not user speech).
//!
//! Use [`record`] at each decision point in the correction pipeline.

use std::sync::atomic::{AtomicU64, Ordering};

/// What happened at a single correction decision point.
#[derive(Debug, Clone, Copy)]
pub enum MetricEvent {
    /// An exact mistranscription match was applied immediately.
    ExactApplied,
    /// An exact match was deferred to the LLM (ambiguous).
    ExactDeferred,
    /// A fuzzy match was applied (high confidence or heuristic-approved).
    FuzzyApplied,
    /// A fuzzy match was deferred to the LLM.
    FuzzyDeferred,
    /// A fuzzy match was rejected (ambiguous, no LLM, no heuristic cue).
    FuzzyRejected,
    /// A back-correction removed a bad mapping.
    BackCorrection,
    /// A correction pair was written to the glossary.
    PairWritten,
    /// An LLM extraction found additional pairs.
    LlmExtractionFound,
    /// An LLM extraction found nothing additional.
    LlmExtractionEmpty,
    /// An LLM extraction call failed.
    LlmExtractionFailed,
}

static EXACT_APPLIED: AtomicU64 = AtomicU64::new(0);
static EXACT_DEFERRED: AtomicU64 = AtomicU64::new(0);
static FUZZY_APPLIED: AtomicU64 = AtomicU64::new(0);
static FUZZY_DEFERRED: AtomicU64 = AtomicU64::new(0);
static FUZZY_REJECTED: AtomicU64 = AtomicU64::new(0);
static BACK_CORRECTION: AtomicU64 = AtomicU64::new(0);
static PAIR_WRITTEN: AtomicU64 = AtomicU64::new(0);
static LLM_FOUND: AtomicU64 = AtomicU64::new(0);
static LLM_EMPTY: AtomicU64 = AtomicU64::new(0);
static LLM_FAILED: AtomicU64 = AtomicU64::new(0);

/// Record a metric event. Increment the corresponding atomic counter.
pub fn record(event: MetricEvent) {
    let counter = match event {
        MetricEvent::ExactApplied => &EXACT_APPLIED,
        MetricEvent::ExactDeferred => &EXACT_DEFERRED,
        MetricEvent::FuzzyApplied => &FUZZY_APPLIED,
        MetricEvent::FuzzyDeferred => &FUZZY_DEFERRED,
        MetricEvent::FuzzyRejected => &FUZZY_REJECTED,
        MetricEvent::BackCorrection => &BACK_CORRECTION,
        MetricEvent::PairWritten => &PAIR_WRITTEN,
        MetricEvent::LlmExtractionFound => &LLM_FOUND,
        MetricEvent::LlmExtractionEmpty => &LLM_EMPTY,
        MetricEvent::LlmExtractionFailed => &LLM_FAILED,
    };
    counter.fetch_add(1, Ordering::Relaxed);
}

/// Log a summary of all metrics at INFO level. Called periodically
/// (e.g. every N utterances) or on demand.
pub fn log_summary() {
    let exact_applied = EXACT_APPLIED.load(Ordering::Relaxed);
    let exact_deferred = EXACT_DEFERRED.load(Ordering::Relaxed);
    let fuzzy_applied = FUZZY_APPLIED.load(Ordering::Relaxed);
    let fuzzy_deferred = FUZZY_DEFERRED.load(Ordering::Relaxed);
    let fuzzy_rejected = FUZZY_REJECTED.load(Ordering::Relaxed);
    let back_corr = BACK_CORRECTION.load(Ordering::Relaxed);
    let pair_written = PAIR_WRITTEN.load(Ordering::Relaxed);
    let llm_found = LLM_FOUND.load(Ordering::Relaxed);
    let llm_empty = LLM_EMPTY.load(Ordering::Relaxed);
    let llm_failed = LLM_FAILED.load(Ordering::Relaxed);

    tracing::info!(
        exact_applied,
        exact_deferred,
        fuzzy_applied,
        fuzzy_deferred,
        fuzzy_rejected,
        back_correction = back_corr,
        pair_written,
        llm_found,
        llm_empty,
        llm_failed,
        "correction_metrics_summary"
    );
}

/// Get a snapshot of all metrics (for programmatic access).
pub fn snapshot() -> MetricsSnapshot {
    MetricsSnapshot {
        exact_applied: EXACT_APPLIED.load(Ordering::Relaxed),
        exact_deferred: EXACT_DEFERRED.load(Ordering::Relaxed),
        fuzzy_applied: FUZZY_APPLIED.load(Ordering::Relaxed),
        fuzzy_deferred: FUZZY_DEFERRED.load(Ordering::Relaxed),
        fuzzy_rejected: FUZZY_REJECTED.load(Ordering::Relaxed),
        back_correction: BACK_CORRECTION.load(Ordering::Relaxed),
        pair_written: PAIR_WRITTEN.load(Ordering::Relaxed),
        llm_found: LLM_FOUND.load(Ordering::Relaxed),
        llm_empty: LLM_EMPTY.load(Ordering::Relaxed),
        llm_failed: LLM_FAILED.load(Ordering::Relaxed),
    }
}

#[derive(Debug, Clone, Default)]
pub struct MetricsSnapshot {
    pub exact_applied: u64,
    pub exact_deferred: u64,
    pub fuzzy_applied: u64,
    pub fuzzy_deferred: u64,
    pub fuzzy_rejected: u64,
    pub back_correction: u64,
    pub pair_written: u64,
    pub llm_found: u64,
    pub llm_empty: u64,
    pub llm_failed: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_increments_counters() {
        // Note: these are global atomics, so we can only test relative
        // increments, not absolute values (other tests may have run).
        let before = snapshot();
        record(MetricEvent::ExactApplied);
        record(MetricEvent::ExactApplied);
        record(MetricEvent::FuzzyDeferred);
        let after = snapshot();
        assert_eq!(after.exact_applied - before.exact_applied, 2);
        assert_eq!(after.fuzzy_deferred - before.fuzzy_deferred, 1);
    }
}
