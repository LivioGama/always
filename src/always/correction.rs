//! Manual-correction capture pipeline.
//!
//! Two entry points feed glossary-correction pairs:
//!
//! 1. **Active hotkey** (default ⌃⌥X): the user fixes a paste in place,
//!    selects the corrected text, and presses the shortcut. The daemon
//!    snapshots their selection and diffs it against `last_pasted` to
//!    extract `(wrong → right)` word pairs that get appended to
//!    `~/.always/glossary.json` immediately. Routed through
//!    [`capture_via_hotkey`].
//!
//! 2. **Passive clipboard watcher** (opt-in via
//!    `passive_correction_capture` pref): the daemon polls
//!    `NSPasteboard.changeCount` and, when the user re-copies a string
//!    that closely resembles the most recently-pasted transcript,
//!    queues the diff in `~/.always/pending_corrections.json` for the
//!    user to review from the menu bar. Routed through
//!    [`super::clipboard_watcher`] which calls
//!    [`super::correction_queue::CorrectionQueue::add`].
//!
//! Both paths share [`diff_words`] and [`apply_pairs_to_glossary`] so
//! the on-disk effect is identical regardless of source.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::glossary::user_glossary_path;

/// One `(wrong, right)` correction extracted from a diff.
///
/// `wrong` is the token that came out of Whisper, `right` is what the
/// user actually meant. Both are stored verbatim — punctuation,
/// capitalization preserved — so glossary lookups can be exact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorrectionPair {
    pub wrong: String,
    pub right: String,
}

/// Maximum length ratio between `wrong` and `right` for a word pair
/// to count as a "correction" rather than two unrelated words.
/// `len_ratio >= MIN_LEN_RATIO`.
const MIN_LEN_RATIO: f64 = 0.45;
/// Levenshtein distance ceiling — `dist <= max(MIN_DIST_FLOOR, len/MIN_DIST_DIVISOR)`.
const MIN_DIST_FLOOR: usize = 2;
const MIN_DIST_DIVISOR: usize = 3;
/// Words shorter than this on either side are too small to diff
/// meaningfully — typo distance from `the` to `a` is 3, which is the
/// whole word; that's not a correction signal.
const MIN_WORD_LEN: usize = 3;
/// Maximum number of correction pairs we'll extract from a single
/// `last_pasted → selection` diff. Above this, the user almost
/// certainly rewrote the whole sentence rather than correcting words —
/// which would pollute the glossary with prose fragments. Better to
/// skip than to poison the dictionary.
pub const MAX_PAIRS_PER_CAPTURE: usize = 8;

/// True when `wrong` and `right` are similar enough to be a typo
/// correction rather than two unrelated tokens. Combination of length
/// ratio + Levenshtein distance vs. word length so short words are
/// stricter than long ones.
pub fn looks_like_correction(wrong: &str, right: &str) -> bool {
    if wrong == right {
        return false;
    }
    if wrong.len() < MIN_WORD_LEN || right.len() < MIN_WORD_LEN {
        return false;
    }
    let max_len = wrong.len().max(right.len());
    let min_len = wrong.len().min(right.len());
    let ratio = min_len as f64 / max_len as f64;
    if ratio < MIN_LEN_RATIO {
        return false;
    }
    let dist = strsim::levenshtein(wrong, right);
    let allowed = MIN_DIST_FLOOR.max(max_len / MIN_DIST_DIVISOR);
    dist <= allowed
}

/// Word-level diff between `old` (what we pasted) and `new` (what the
/// user corrected to). Returns up to [`MAX_PAIRS_PER_CAPTURE`] pairs.
///
/// Algorithm:
/// 1. Run LCS on the whitespace-tokenized arrays.
/// 2. For each gap between consecutive LCS anchors, look at the
///    unmatched runs on each side:
///    - Both runs same length → emit per-word pairs. Strict
///      [`looks_like_correction`] gate applies only when the gap
///      isn't sandwiched by anchors on both sides; when context is
///      anchored bilaterally, accept the substitution because the
///      surrounding words confirm it's a real correction (e.g.
///      `"... 16 cloud code running ..." → "... 16 Claude Code running ..."`
///      — `looks_like_correction` would reject `cloud → Claude` on
///      Levenshtein, but the anchors `16` and `running` make it
///      unambiguous).
///    - Different lengths → emit a single PHRASE pair joining each
///      run with spaces (e.g. `"cloudcodes" ↔ "Claude Code"`,
///      `"rock trees" ↔ "worktrees"`). This is the lever that lets
///      the user select the whole corrected sentence and have the
///      daemon find multi-word substitutions.
/// 3. Trailing punctuation is stripped from emitted pairs so the
///    glossary doesn't fill up with `"trees."` vs `"trees,"`
///    duplicates.
pub fn diff_words(old: &str, new: &str) -> Vec<CorrectionPair> {
    let old_tokens: Vec<&str> = old.split_whitespace().collect();
    let new_tokens: Vec<&str> = new.split_whitespace().collect();

    if old_tokens.is_empty() || new_tokens.is_empty() {
        return Vec::new();
    }

    let lcs = longest_common_subsequence(&old_tokens, &new_tokens);

    let mut pairs = Vec::new();
    let mut oi = 0;
    let mut ni = 0;
    let mut li = 0;
    // Track whether we just consumed an LCS anchor — used to decide
    // whether the current gap has bilateral anchors.
    let mut prev_was_anchor = false;

    while oi < old_tokens.len() && ni < new_tokens.len() {
        // Both sides at the next LCS anchor → advance.
        if li < lcs.len()
            && oi < old_tokens.len()
            && old_tokens[oi] == lcs[li]
            && ni < new_tokens.len()
            && new_tokens[ni] == lcs[li]
        {
            oi += 1;
            ni += 1;
            li += 1;
            prev_was_anchor = true;
            continue;
        }

        // Collect runs of unmatched words on each side until we hit
        // the next LCS anchor (or end).
        let next_lcs = lcs.get(li).copied();
        let old_run_end = match next_lcs {
            Some(anchor) => old_tokens[oi..]
                .iter()
                .position(|w| *w == anchor)
                .map(|p| oi + p)
                .unwrap_or(old_tokens.len()),
            None => old_tokens.len(),
        };
        let new_run_end = match next_lcs {
            Some(anchor) => new_tokens[ni..]
                .iter()
                .position(|w| *w == anchor)
                .map(|p| ni + p)
                .unwrap_or(new_tokens.len()),
            None => new_tokens.len(),
        };

        let old_len = old_run_end - oi;
        let new_len = new_run_end - ni;
        // Bilateral anchoring: there's an LCS word immediately before
        // (prev_was_anchor) AND an LCS word immediately after
        // (next_lcs is Some). In that case the run is tightly framed
        // by surrounding context, so we trust the substitution.
        let bilateral = prev_was_anchor && next_lcs.is_some();

        if old_len > 0 && new_len > 0 {
            if old_len == new_len {
                // Same-length runs — per-word pairs.
                for k in 0..old_len {
                    let wrong_raw = old_tokens[oi + k];
                    let right_raw = new_tokens[ni + k];
                    let wrong = strip_edge_punct(wrong_raw);
                    let right = strip_edge_punct(right_raw);
                    if wrong.is_empty() || right.is_empty() || wrong == right {
                        continue;
                    }
                    if bilateral || looks_like_correction(wrong, right) {
                        pairs.push(CorrectionPair {
                            wrong: wrong.to_string(),
                            right: right.to_string(),
                        });
                        if pairs.len() >= MAX_PAIRS_PER_CAPTURE {
                            return pairs;
                        }
                    }
                }
            } else {
                // Asymmetric runs — emit a single PHRASE pair joining
                // both sides. Punctuation stripped from the phrase
                // edges so `rock trees.` ↔ `worktrees.` becomes
                // `rock trees` ↔ `worktrees`.
                let wrong_phrase = old_tokens[oi..old_run_end].join(" ");
                let right_phrase = new_tokens[ni..new_run_end].join(" ");
                let wrong = strip_edge_punct(&wrong_phrase).to_string();
                let right = strip_edge_punct(&right_phrase).to_string();
                if !wrong.is_empty() && !right.is_empty() && wrong != right {
                    pairs.push(CorrectionPair { wrong, right });
                    if pairs.len() >= MAX_PAIRS_PER_CAPTURE {
                        return pairs;
                    }
                }
            }
        }

        prev_was_anchor = false;
        oi = old_run_end;
        ni = new_run_end;
    }

    pairs
}

/// Strip leading and trailing punctuation/whitespace from a token or
/// phrase. Internal punctuation is preserved (e.g. dotted module
/// names, hyphenated identifiers).
fn strip_edge_punct(s: &str) -> &str {
    s.trim_matches(|c: char| {
        c.is_whitespace()
            || matches!(c, '.' | ',' | '!' | '?' | ';' | ':' | '"' | '\'' | '(' | ')')
    })
}

/// Standard Wagner-Fisher LCS over token slices.
fn longest_common_subsequence<'a>(a: &[&'a str], b: &[&'a str]) -> Vec<&'a str> {
    let n = a.len();
    let m = b.len();
    if n == 0 || m == 0 {
        return Vec::new();
    }
    // DP table: lengths only; backtrack at the end.
    let mut dp = vec![vec![0usize; m + 1]; n + 1];
    for i in 0..n {
        for j in 0..m {
            dp[i + 1][j + 1] = if a[i] == b[j] {
                dp[i][j] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let mut out = Vec::with_capacity(dp[n][m]);
    let mut i = n;
    let mut j = m;
    while i > 0 && j > 0 {
        if a[i - 1] == b[j - 1] {
            out.push(a[i - 1]);
            i -= 1;
            j -= 1;
        } else if dp[i - 1][j] >= dp[i][j - 1] {
            i -= 1;
        } else {
            j -= 1;
        }
    }
    out.reverse();
    out
}

/// Append correction pairs to the user's glossary.
///
/// Behavior:
/// * If a glossary entry with `term == pair.right` already exists, add
///   `pair.wrong` to its `mistranscriptions` (deduped).
/// * Otherwise, create a new entry `{term: right, mistranscriptions:
///   [wrong], frequency: 100}`.
///
/// Returns the number of *new* mistranscriptions actually written
/// (entries that already had `pair.wrong` listed don't count).
pub fn apply_pairs_to_glossary(pairs: &[CorrectionPair]) -> Result<usize> {
    if pairs.is_empty() {
        return Ok(0);
    }
    let path = user_glossary_path()
        .ok_or_else(|| anyhow::anyhow!("could not resolve ~/.always glossary path"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    let mut entries: Vec<serde_json::Value> = if path.exists() {
        let content = std::fs::read_to_string(&path).context("read glossary.json")?;
        if content.trim().is_empty() {
            Vec::new()
        } else {
            serde_json::from_str(&content).context("parse glossary.json")?
        }
    } else {
        Vec::new()
    };

    let mut written = 0usize;

    for pair in pairs {
        // Find existing entry by term (case-sensitive — preserve the
        // user's exact capitalization).
        let idx = entries
            .iter()
            .position(|e| e.get("term").and_then(|v| v.as_str()) == Some(pair.right.as_str()));

        match idx {
            Some(i) => {
                let entry = &mut entries[i];
                let arr = entry
                    .get_mut("mistranscriptions")
                    .and_then(|v| v.as_array_mut());
                let arr = match arr {
                    Some(a) => a,
                    None => {
                        // Field missing or wrong type — recreate.
                        entry["mistranscriptions"] = serde_json::Value::Array(Vec::new());
                        entry
                            .get_mut("mistranscriptions")
                            .and_then(|v| v.as_array_mut())
                            .expect("just inserted")
                    }
                };
                let already = arr.iter().any(|v| v.as_str() == Some(pair.wrong.as_str()));
                if !already {
                    arr.push(serde_json::Value::String(pair.wrong.clone()));
                    written += 1;
                }
            }
            None => {
                entries.push(serde_json::json!({
                    "term": pair.right,
                    "mistranscriptions": [pair.wrong],
                    "frequency": 100,
                }));
                written += 1;
            }
        }
    }

    let json = serde_json::to_string_pretty(&entries)?;
    std::fs::write(&path, json).context("write glossary.json")?;

    tracing::info!(
        path = %path.display(),
        pairs = pairs.len(),
        written,
        "correction_glossary_updated"
    );

    Ok(written)
}

// ---------------------------------------------------------------------------
// Live capture (clipboard + Cmd+C) — macOS-only
// ---------------------------------------------------------------------------

/// Snapshot the user's current selection by simulating Cmd+C, reading
/// the new clipboard, and restoring the previous clipboard contents so
/// clipboard-history apps don't end up with a polluting copy.
///
/// Returns `Ok(None)` if no selection is available (or the platform
/// doesn't support this).
#[cfg(feature = "macos")]
pub fn read_selection_via_cmd_c() -> Result<Option<String>> {
    use crate::always::paste;

    // 1. Save current clipboard.
    let prev = read_clipboard().ok();

    // 2. Trigger Cmd+C in the foreground app.
    simulate_cmd_c()?;

    // 3. Wait briefly for the app to populate the pasteboard. 60 ms is
    //    empirically sufficient for non-Electron apps; Electron may
    //    need more (Slack, Discord). We poll up to 250 ms.
    let started = Instant::now();
    let mut current = String::new();
    let timeout = Duration::from_millis(250);
    while started.elapsed() < timeout {
        std::thread::sleep(Duration::from_millis(40));
        if let Ok(text) = read_clipboard() {
            // The first non-empty value that differs from `prev` is
            // our selection. If `prev` was None we accept any non-empty
            // value.
            let differs_from_prev = match &prev {
                Some(p) => p != &text,
                None => true,
            };
            if !text.is_empty() && differs_from_prev {
                current = text;
                break;
            }
        }
    }

    // 4. Restore previous clipboard (best-effort; failure is logged
    //    but not propagated — losing the user's clipboard is a worse
    //    outcome than skipping the restore).
    if let Some(prev) = prev
        && let Err(e) = paste::copy_to_clipboard(prev)
    {
        tracing::warn!(error = %e, "failed to restore previous clipboard contents");
    }

    if current.is_empty() {
        Ok(None)
    } else {
        Ok(Some(current))
    }
}

#[cfg(not(feature = "macos"))]
pub fn read_selection_via_cmd_c() -> Result<Option<String>> {
    Ok(None)
}

#[cfg(feature = "macos")]
fn simulate_cmd_c() -> Result<()> {
    use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation, CGKeyCode};
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .map_err(|_| anyhow::anyhow!("Failed to create CGEventSource"))?;
    let c_keycode: CGKeyCode = 8; // 'c'

    let down = CGEvent::new_keyboard_event(source.clone(), c_keycode, true)
        .map_err(|_| anyhow::anyhow!("Failed to create Cmd+C key down event"))?;
    down.set_flags(CGEventFlags::CGEventFlagCommand);
    down.post(CGEventTapLocation::HID);

    let up = CGEvent::new_keyboard_event(source, c_keycode, false)
        .map_err(|_| anyhow::anyhow!("Failed to create Cmd+C key up event"))?;
    up.set_flags(CGEventFlags::CGEventFlagCommand);
    up.post(CGEventTapLocation::HID);

    Ok(())
}

/// Read the current clipboard contents as UTF-8 via `pbpaste`.
pub fn read_clipboard() -> Result<String> {
    let output = std::process::Command::new("pbpaste")
        .output()
        .context("spawn pbpaste")?;
    if !output.status.success() {
        anyhow::bail!("pbpaste exited with status {}", output.status);
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

// ---------------------------------------------------------------------------
// High-level entry points
// ---------------------------------------------------------------------------

/// Outcome of a correction capture attempt — whether triggered by the
/// hotkey or surfaced from the passive clipboard watcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureOutcome {
    /// No `last_pasted` snapshot was available (or it had expired) —
    /// nothing to diff against.
    NoRecentPaste,
    /// The selection was unchanged from the last paste — the user
    /// accepted the transcript as-is, no correction to record.
    NoChange,
    /// Diff produced no pairs that cleared the similarity gate.
    /// Likely the user rewrote rather than corrected.
    NoCorrectionPairs,
    /// Pairs extracted and applied to glossary. `applied` counts new
    /// mistranscriptions actually written; pairs already present are
    /// counted in `pairs.len()` but not in `applied`.
    Applied {
        pairs: Vec<CorrectionPair>,
        applied: usize,
    },
}

/// Hotkey-driven capture: read user's current selection, diff against
/// `last_pasted`, write to glossary. Called on `⌃⌥X`.
///
/// `last_pasted_window` controls how stale a paste can be before we
/// refuse to use it as the diff baseline — a long delay almost
/// certainly means the user has moved on and any selection is
/// unrelated.
#[cfg(feature = "macos")]
pub fn capture_via_hotkey(last_pasted_window: Duration) -> Result<CaptureOutcome> {
    use crate::always::pause;

    let Some((last, _ts)) = pause::take_last_pasted_within(last_pasted_window) else {
        return Ok(CaptureOutcome::NoRecentPaste);
    };

    let selection = match read_selection_via_cmd_c()? {
        Some(s) => s,
        None => return Ok(CaptureOutcome::NoRecentPaste),
    };

    let last_clean = last.trim_end().to_string();
    let selection_clean = selection.trim_end().to_string();
    if last_clean == selection_clean {
        return Ok(CaptureOutcome::NoChange);
    }

    let pairs = diff_words(&last_clean, &selection_clean);
    if pairs.is_empty() {
        return Ok(CaptureOutcome::NoCorrectionPairs);
    }

    let applied = apply_pairs_to_glossary(&pairs)?;
    Ok(CaptureOutcome::Applied { pairs, applied })
}

#[cfg(not(feature = "macos"))]
pub fn capture_via_hotkey(_window: Duration) -> Result<CaptureOutcome> {
    Ok(CaptureOutcome::NoRecentPaste)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_like_correction_accepts_typos() {
        assert!(looks_like_correction("kubernetics", "kubernetes"));
        assert!(looks_like_correction("Slack", "slack"));
        assert!(looks_like_correction("transcrib", "transcribe"));
    }

    #[test]
    fn looks_like_correction_rejects_short_words() {
        // Below MIN_WORD_LEN — too small to be reliable.
        assert!(!looks_like_correction("a", "the"));
        assert!(!looks_like_correction("of", "off"));
    }

    #[test]
    fn looks_like_correction_rejects_dissimilar() {
        assert!(!looks_like_correction("kubernetes", "minikube"));
        assert!(!looks_like_correction("hello", "world"));
        assert!(!looks_like_correction("transcribe", "translation"));
    }

    #[test]
    fn looks_like_correction_rejects_identical() {
        assert!(!looks_like_correction("kubernetes", "kubernetes"));
    }

    #[test]
    fn diff_words_extracts_substitution() {
        let pairs = diff_words(
            "deploy the kubernetics cluster",
            "deploy the kubernetes cluster",
        );
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].wrong, "kubernetics");
        assert_eq!(pairs[0].right, "kubernetes");
    }

    #[test]
    fn diff_words_skips_unrelated_replacements() {
        let pairs = diff_words(
            "deploy the kubernetes cluster",
            "delete the docker container",
        );
        // None of the wrong→right pairs clear the similarity gate.
        assert!(
            pairs.is_empty(),
            "expected no correction pairs, got {:?}",
            pairs
        );
    }

    #[test]
    fn diff_words_handles_insertion() {
        // User added a word — no substitution to learn.
        let pairs = diff_words("deploy the cluster", "deploy the kubernetes cluster");
        assert!(pairs.is_empty(), "got {:?}", pairs);
    }

    #[test]
    fn diff_words_caps_at_max_pairs() {
        // Construct an old/new where every word is a typo of the new one.
        let old: Vec<String> = (0..20).map(|i| format!("wrng{}", i)).collect();
        let new: Vec<String> = (0..20).map(|i| format!("right{}", i)).collect();
        let pairs = diff_words(&old.join(" "), &new.join(" "));
        assert!(
            pairs.len() <= MAX_PAIRS_PER_CAPTURE,
            "got {} pairs, expected <= {}",
            pairs.len(),
            MAX_PAIRS_PER_CAPTURE
        );
    }

    #[test]
    fn diff_words_empty_inputs_return_empty() {
        assert!(diff_words("", "anything").is_empty());
        assert!(diff_words("anything", "").is_empty());
        assert!(diff_words("", "").is_empty());
    }

    #[test]
    fn diff_words_user_demo_phrase_and_word_substitutions() {
        // Real-world example: user dictates → Whisper produces wrong
        // text → user selects the WHOLE corrected sentence and presses
        // ⌃⌥X. Daemon must extract both substitutions even though
        // they're asymmetric (1↔2 words and 2↔1 words).
        let old = "Cursor? Are you living under a rock? I have 16 cloudcodes running in different Git rock trees.";
        let new = "Cursor? Are you living under a rock? I have 16 Claude Code running in different Git worktrees.";
        let pairs = diff_words(old, new);

        let pair_set: Vec<(String, String)> = pairs
            .iter()
            .map(|p| (p.wrong.clone(), p.right.clone()))
            .collect();

        assert!(
            pair_set.contains(&("cloudcodes".to_string(), "Claude Code".to_string())),
            "missing cloudcodes → Claude Code, got {:?}",
            pair_set
        );
        assert!(
            pair_set.contains(&("rock trees".to_string(), "worktrees".to_string())),
            "missing rock trees → worktrees, got {:?}",
            pair_set
        );
        assert_eq!(
            pairs.len(),
            2,
            "expected exactly two phrase pairs, got {:?}",
            pair_set
        );
    }

    #[test]
    fn diff_words_bilateral_anchor_accepts_low_levenshtein_match() {
        // Without bilateral anchoring, "cloud" → "Claude" fails the
        // Levenshtein gate (distance 3 > floor 2 for length-6 words).
        // With anchors on both sides ("16" and "code"), it must pass.
        let pairs = diff_words("16 cloud code today", "16 Claude code today");
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].wrong, "cloud");
        assert_eq!(pairs[0].right, "Claude");
    }

    #[test]
    fn diff_words_preserves_order() {
        let pairs = diff_words(
            "deploy kubernetics and superwhisper today",
            "deploy kubernetes and superwhisper today",
        );
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].wrong, "kubernetics");
    }

    #[test]
    fn lcs_basic() {
        assert_eq!(
            longest_common_subsequence(&["a", "b", "c", "d"], &["a", "x", "c", "y", "d"]),
            vec!["a", "c", "d"]
        );
    }

    /// Cargo runs unit tests in parallel by default; tests that mutate
    /// `$HOME` must not race against each other. Acquire this mutex at
    /// the top of every HOME-touching test.
    fn home_lock() -> std::sync::MutexGuard<'static, ()> {
        use std::sync::{LazyLock, Mutex};
        static LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));
        LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }

    #[test]
    fn apply_pairs_creates_new_entry_in_temp_glossary() {
        let _guard = home_lock();
        // Use a temp ~/.always to avoid touching the real glossary.
        let tmp = tempdir_under_target();
        // SAFETY: serialized via `home_lock()` so no other test in this
        // module reads $HOME concurrently.
        unsafe { std::env::set_var("HOME", &tmp) };

        let pairs = vec![CorrectionPair {
            wrong: "kubernetics".to_string(),
            right: "kubernetes".to_string(),
        }];
        let written = apply_pairs_to_glossary(&pairs).unwrap();
        assert_eq!(written, 1);

        let glossary = std::fs::read_to_string(tmp.join(".always/glossary.json")).unwrap();
        assert!(glossary.contains("\"kubernetes\""));
        assert!(glossary.contains("\"kubernetics\""));

        // Re-applying same pair should be idempotent (already present).
        let written = apply_pairs_to_glossary(&pairs).unwrap();
        assert_eq!(written, 0);
    }

    #[test]
    fn apply_pairs_appends_to_existing_entry() {
        let _guard = home_lock();
        let tmp = tempdir_under_target();
        // SAFETY: serialized via `home_lock()`; see sister test above.
        unsafe { std::env::set_var("HOME", &tmp) };
        let glossary_path = tmp.join(".always/glossary.json");
        std::fs::create_dir_all(glossary_path.parent().unwrap()).unwrap();
        std::fs::write(
            &glossary_path,
            r#"[{"term":"kubernetes","mistranscriptions":["cuber netties"],"frequency":200}]"#,
        )
        .unwrap();

        let written = apply_pairs_to_glossary(&[CorrectionPair {
            wrong: "kubernetics".to_string(),
            right: "kubernetes".to_string(),
        }])
        .unwrap();
        assert_eq!(written, 1);

        let parsed: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&glossary_path).unwrap()).unwrap();
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 1, "should not have created a duplicate entry");
        let mistr = arr[0]["mistranscriptions"].as_array().unwrap();
        assert!(mistr.iter().any(|v| v.as_str() == Some("kubernetics")));
        assert!(mistr.iter().any(|v| v.as_str() == Some("cuber netties")));
        // Pre-existing `frequency` preserved.
        assert_eq!(arr[0]["frequency"].as_u64(), Some(200));
    }

    /// Per-test scratch directory under `target/test-tmp/` — avoids
    /// touching the real `~/.always` while still living inside the
    /// repo's gitignored area.
    fn tempdir_under_target() -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-tmp")
            .join(format!("correction-{pid}-{n}"));
        std::fs::create_dir_all(&path).unwrap();
        path
    }
}
