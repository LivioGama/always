//! Passive correction-capture watcher.
//!
//! See `correction.rs` for the active hotkey path. This module
//! implements the opt-in passive path: poll the system clipboard,
//! detect when the user re-copies a fragment that closely resembles a
//! recently-pasted transcript, and queue the diff for user review via
//! `correction_queue::CorrectionQueue`.
//!
//! Only enabled when the daemon's `passive_correction_capture`
//! preference is `true`. Default is `false` so users opt in
//! deliberately — this watches the clipboard, which warrants explicit
//! consent.

use std::time::Duration;

/// Spawn the passive watcher onto the supplied tokio runtime handle.
///
/// Polls every [`POLL_INTERVAL`] and, when the clipboard changes within
/// [`PASTE_WINDOW`] of a `pause::set_last_pasted` snapshot, runs a
/// word-level diff and (if any pair clears the correction-similarity
/// gate) queues the candidate via `correction_queue::global_queue()`.
///
/// The handle is dropped when the daemon exits — no explicit shutdown
/// needed.
pub fn spawn_if_enabled(rt: &tokio::runtime::Handle, enabled: bool) {
    if !enabled {
        tracing::debug!("passive_correction_capture disabled");
        return;
    }
    rt.spawn(async {
        run_loop().await;
    });
}

const POLL_INTERVAL: Duration = Duration::from_millis(250);
/// A clipboard change that arrives within this window of a paste is a
/// candidate correction. Beyond it, we assume the user has moved on.
pub const PASTE_WINDOW: Duration = Duration::from_secs(60);

async fn run_loop() {
    use crate::always::{correction, correction_queue, event, pause};

    tracing::info!("passive_correction_watcher_started");

    let queue = match correction_queue::global_queue() {
        Ok(q) => q,
        Err(e) => {
            tracing::error!(error = %e, "passive watcher: correction queue init failed");
            return;
        }
    };

    let mut last_seen = correction::read_clipboard().unwrap_or_default();

    let mut interval = tokio::time::interval(POLL_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        interval.tick().await;

        let current = match correction::read_clipboard() {
            Ok(s) => s,
            Err(_) => continue,
        };
        if current == last_seen {
            continue;
        }
        last_seen = current.clone();

        // Only consider clipboard changes that happened within the
        // paste window of our most recent paste.
        let Some((last_pasted, _)) = pause::take_last_pasted_within(PASTE_WINDOW) else {
            continue;
        };

        let last_clean = last_pasted.trim_end().to_string();
        let current_clean = current.trim_end().to_string();
        if current_clean == last_clean || current_clean.is_empty() {
            continue;
        }

        // Reject obvious copy-paste of unrelated content. Use a global
        // sequence-similarity gate before doing word-level diff so a
        // wholly different selection doesn't pollute the queue.
        let global_sim = strsim::normalized_levenshtein(&last_clean, &current_clean);
        if global_sim < 0.55 {
            continue;
        }

        let pairs = correction::diff_words(&last_clean, &current_clean);
        if pairs.is_empty() {
            continue;
        }

        for pair in pairs {
            match queue.add(pair.clone(), correction_queue::CorrectionSource::Passive) {
                Ok(id) => {
                    event::global_broadcaster().send(event::DaemonEvent::CorrectionPending {
                        id: id.to_string(),
                        wrong: pair.wrong.clone(),
                        right: pair.right.clone(),
                    });
                    tracing::info!(
                        wrong = %pair.wrong,
                        right = %pair.right,
                        "passive_correction_queued"
                    );
                }
                Err(e) => {
                    tracing::error!(error = %e, "passive_correction_queue_add_failed");
                }
            }
        }
    }
}
