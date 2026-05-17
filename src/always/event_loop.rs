use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::runtime::{Handle, Runtime};

use crate::always::log::{Event, Logger};
use crate::always::{
    AlwaysConfig, auto_enter_countdown, clipboard_watcher, daemon, event, filter, idle_watcher,
    keyboard, mic_monitor, paste, pause, per_app, uds_server, vad,
};

pub fn run(cfg: &AlwaysConfig) -> Result<()> {
    let _pid = daemon::PidGuard::install()?;
    let mut log = Logger::open(&cfg.log_path)?;
    log.write(Event::Start { cfg });
    print_banner(cfg);

    let rt = Runtime::new()?;

    // Install SIGTERM/SIGINT handlers BEFORE any subsystem starts so a
    // signal during init still triggers cleanup. The handler removes
    // the pid + socket files and calls `std::process::exit(0)`.
    daemon::install_signal_handlers(rt.handle());

    // Pre-warm TLS+HTTP/2 connection to Groq API in background using existing runtime
    {
        let api_key = cfg.groq_stt_api_key.clone();
        rt.spawn(async move {
            let client = crate::http_client::async_client();
            let _ = client
                .get("https://api.groq.com/openai/v1/models")
                .header("Authorization", format!("Bearer {}", api_key))
                .send()
                .await;
        });
    }

    // Initialize the auto-enter state from config
    pause::init_auto_enter(cfg.auto_enter);

    // Start keyboard listener for shortcuts
    keyboard::start_keyboard_listener()?;

    // Start UDS server in background and track the task
    let _uds_handle = rt.spawn(async {
        if let Err(e) = uds_server::start_server().await {
            tracing::error!(error = %e, "UDS server error");
        }
    });

    // Heartbeat task: emit Heartbeat every 5s so connected GUI clients can
    // detect a dead/stalled daemon via watchdog timeout.
    let _heartbeat_handle = rt.spawn(async {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        // First tick fires immediately; skip it to avoid double-emit at startup.
        interval.tick().await;
        loop {
            interval.tick().await;
            event::global_broadcaster().send(event::DaemonEvent::Heartbeat);
        }
    });

    // Spawn the passive correction-capture watcher when the user has
    // opted in via the `passive_correction_capture` preference. The
    // module is a no-op when `enabled == false`; we still call it so
    // the daemon's wiring is the single source of truth and a future
    // toggle from the UI can simply restart the daemon. Failure to
    // load preferences is non-fatal — we treat it as "feature off".
    let passive_correction_enabled = crate::db::open()
        .and_then(|conn| crate::db::get_preferences(&conn))
        .ok()
        .and_then(|p| p.passive_correction_capture)
        .unwrap_or(false);
    clipboard_watcher::spawn_if_enabled(rt.handle(), passive_correction_enabled);

    // Idle-pause watchdog. Spawns at most one task; no-op when
    // `idle_pause_secs == 0`. Lives for the daemon lifetime.
    idle_watcher::spawn(rt.handle(), cfg.idle_pause_secs, cfg.idle_pause_action);

    // Send initial state events
    event::global_broadcaster().listening_started();
    if cfg.auto_enter {
        event::global_broadcaster().auto_enter_enabled();
    }

    let mut last_process = Instant::now() - Duration::from_secs(10);

    // Initialize microphone monitor for auto-pause functionality
    let mut mic_monitor = mic_monitor::MicrophoneMonitor::new();
    let mut auto_paused_for_mic = false;

    loop {
        // Check if other apps are using the microphone
        match mic_monitor.is_microphone_in_use() {
            Ok(true) if !auto_paused_for_mic => {
                // Another app started using the microphone - pause Always
                if let Ok(users) = mic_monitor.get_microphone_users() {
                    let app_list = if users.is_empty() {
                        "another application".to_string()
                    } else {
                        users.join(", ")
                    };
                    tracing::info!(apps = %app_list, "microphone_auto_paused");

                    // Log the auto-pause event
                    log.write(Event::MicrophoneAutoPaused { apps: &app_list });
                }
                let (_effective, changed) = pause::set_paused(true);
                event::global_broadcaster().master_pause_changed(true);
                if changed {
                    pause::dictation_buffer_clear();
                    event::global_broadcaster().paused();
                }
                auto_paused_for_mic = true;
            }
            Ok(false) if auto_paused_for_mic => {
                // Microphone is no longer in use - resume Always
                tracing::info!("microphone_auto_resumed");

                // Log the auto-resume event
                log.write(Event::MicrophoneAutoResumed);

                // Clear the idle-auto-paused flag so subsequent
                // audio-output stop events resume the daemon. Previously
                // we set `paused=false` but left `idle_auto_paused=true`,
                // so `uds_server::NotifySystemAudioState{playing:false}`
                // would refuse to auto-resume (it gates on
                // `!is_idle_auto_paused()`) and the daemon stayed paused
                // even though the user could see "Listening" in the UI.
                pause::set_idle_auto_paused(false);
                let (effective, changed) = pause::set_paused(false);
                event::global_broadcaster().master_pause_changed(false);
                // Reset the voice-seen timestamp so the idle watchdog
                // doesn't immediately re-pause us based on the gap that
                // accumulated while we were mic-paused.
                pause::mark_voice_seen();
                if changed {
                    if effective {
                        event::global_broadcaster().paused();
                    } else {
                        event::global_broadcaster().resumed();
                    }
                }
                auto_paused_for_mic = false;
            }
            Err(e) => {
                tracing::warn!(error = %e, "microphone_check_failed");
            }
            _ => {} // No change in microphone state
        }

        if pause::is_paused() {
            // When paused, sleep for a short time to avoid busy waiting
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }

        // Note: Removed auto_enter config reload that was causing race conditions
        // Auto-enter state is now managed consistently through keyboard shortcuts and settings UI

        if let Err(e) = process_one(cfg, &mut log, &mut last_process, rt.handle()) {
            tracing::error!(error = %e, "voice_processing_error");
            log.write(Event::Error {
                message: &format!("Voice processing error: {:#}", e),
            });
            // Don't exit - continue running
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}

fn process_one(
    cfg: &AlwaysConfig,
    log: &mut Logger,
    last_process: &mut Instant,
    rt: &Handle,
) -> Result<()> {
    match vad::record_utterance(cfg, log).context("failed to record/transcribe utterance")? {
        vad::RecordResult::Speech {
            text,
            energy,
            transcription,
        } => {
            handle_speech(cfg, log, &text, energy, &transcription, last_process, rt)?;
        }
        vad::RecordResult::Silence => {
            // Don't log silence events - they're too frequent and not useful
        }
        vad::RecordResult::Timeout => log.write(Event::Timeout),
        vad::RecordResult::DroppedLowEnergy { energy } => {
            log.write(Event::DroppedLowEnergy { energy });
        }
        vad::RecordResult::DroppedNoise { raw } => {
            tracing::debug!(raw, "dropped_noise");
            log.write(Event::DroppedNoise { raw: &raw });
        }
    }
    Ok(())
}

/// Classification of an incoming transcription. Keeping this an enum
/// instead of the original tangled control flow makes the decision tree
/// testable in isolation — we can feed in any combination of text +
/// transcription metadata and assert the outcome without spawning a
/// daemon, calling Groq, or touching the user's pasteboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpeechAction {
    /// In cooldown window (anti-double-paste). No-op.
    InCooldown,
    /// Hard or AI filter rejected the text. The reason is the human-readable
    /// label emitted on the UDS stream so the GUI can display it.
    Rejected { reason: String },
    /// Whisper hallucination detector rejected the text.
    Hallucinated { reason: String },
    /// Accepted — daemon should copy + paste this final text.
    Paste { text: String },
}

/// Words that Whisper often capitalizes at sentence start which are
/// safe to lowercase when appending mid-sentence. Conservative on
/// purpose — anything not in this set is left as-is so proper nouns
/// (Kubernetes, John, Berlin) are preserved when starting a continuation.
///
/// "I" and its contractions are intentionally absent: they MUST stay
/// capitalized even mid-sentence.
const SAFE_LOWERCASE_STARTERS: &[&str] = &[
    "The", "A", "An", "And", "But", "Or", "So", "Yet", "Nor",
    "If", "When", "While", "Because", "Since", "Although", "Though",
    "Then", "Now", "Just", "Also", "Only", "Even", "Still",
    "We", "You", "They", "He", "She", "It",
    "This", "That", "These", "Those", "There", "Here",
    "In", "On", "At", "To", "For", "Of", "With", "Without", "From",
    "By", "About", "Into", "Onto", "Over", "Under", "Through",
    "As", "Is", "Are", "Was", "Were", "Will", "Would", "Should",
    "Could", "Can", "May", "Might", "Must", "Has", "Have", "Had", "Do",
    "Does", "Did", "Be", "Been", "Being",
    "Some", "Any", "All", "Most", "Many", "Few", "Each", "Every",
    "My", "Your", "Our", "Their", "His", "Her", "Its",
    "So", "Such", "What", "Which", "How", "Why", "Where",
];

/// Append `addition` to `previous` for the dictation-merge case.
///
/// Rules:
///   - If `previous` already ends with a sentence terminator (`. ! ?`),
///     the addition starts a new sentence — keep its capitalization.
///   - Else, attempt to lowercase the first word IF and ONLY IF it's a
///     known "safe" sentence starter (see `SAFE_LOWERCASE_STARTERS`).
///     This biases toward preserving proper nouns when in doubt.
///   - Always insert a single space between the two pieces unless
///     `previous` already ends with whitespace.
///
/// Returns `(joined_full_text, delta_to_paste)`. The delta is what
/// should be pasted at the cursor; the joined text is what becomes the
/// new dictation buffer.
pub fn merge_dictation(previous: &str, addition: &str) -> (String, String) {
    let addition = addition.trim_start();
    if addition.is_empty() {
        return (previous.to_string(), String::new());
    }

    let previous_trimmed = previous.trim_end();
    let ends_sentence = previous_trimmed
        .chars()
        .last()
        .map(|c| matches!(c, '.' | '!' | '?'))
        .unwrap_or(false);

    let adjusted = if ends_sentence {
        addition.to_string()
    } else {
        lowercase_if_safe_starter(addition)
    };

    let needs_space = !previous.ends_with(char::is_whitespace) && !previous.is_empty();
    let delta = if needs_space {
        format!(" {}", adjusted)
    } else {
        adjusted.clone()
    };
    let joined = format!("{}{}", previous, delta);
    (joined, delta)
}

fn lowercase_if_safe_starter(text: &str) -> String {
    // Split first whitespace-delimited token; preserve trailing exactly.
    let trimmed = text.trim_start();
    let first_word_end = trimmed
        .find(|c: char| c.is_whitespace())
        .unwrap_or(trimmed.len());
    let first_word = &trimmed[..first_word_end];
    let rest = &trimmed[first_word_end..];

    // Strip trailing punctuation for lookup so "And," still matches "And".
    let lookup = first_word.trim_end_matches(|c: char| !c.is_alphanumeric());
    if SAFE_LOWERCASE_STARTERS.contains(&lookup) {
        let mut chars = first_word.chars();
        if let Some(first_char) = chars.next() {
            let lowered = first_char.to_lowercase().collect::<String>();
            return format!("{}{}{}", lowered, chars.as_str(), rest);
        }
    }
    text.to_string()
}

/// Pure decision function — no I/O, no globals. Takes the raw
/// transcription and returns what the daemon should do next.
pub fn classify_transcription(
    cfg: &AlwaysConfig,
    text: &str,
    transcription: &crate::stt::TranscriptionResult,
    now: Instant,
    last_process: Instant,
) -> SpeechAction {
    if in_cooldown(now, last_process, cfg.cooldown_ms) {
        return SpeechAction::InCooldown;
    }

    let filter_result = filter::should_accept_with_reason(text, cfg);
    if !matches!(filter_result, filter::FilterReason::None) {
        return SpeechAction::Rejected {
            reason: filter_result.to_log_string(),
        };
    }

    if let Some(reason) = crate::always::hallucination::is_hallucination(transcription) {
        return SpeechAction::Hallucinated {
            reason: reason.to_string(),
        };
    }

    let _ = cfg; // `cfg` reserved for future per-utterance decisions
    SpeechAction::Paste {
        text: text.to_string(),
    }
}

fn handle_speech(
    cfg: &AlwaysConfig,
    log: &mut Logger,
    text: &str,
    energy: f64,
    transcription: &crate::stt::TranscriptionResult,
    last_process: &mut Instant,
    rt: &Handle,
) -> Result<()> {
    let now = Instant::now();
    let action = classify_transcription(cfg, text, transcription, now, *last_process);

    // Cooldown is observable in the decision but does not advance
    // last_process — let the next utterance re-check.
    if !matches!(action, SpeechAction::InCooldown) {
        *last_process = now;
        log.write(Event::Transcribed { text, energy });
    }

    match action {
        SpeechAction::InCooldown => Ok(()),
        SpeechAction::Rejected { reason } => {
            tracing::info!(text, filter_reason = %reason, "filtered");
            // Re-derive the structured filter result for logging fidelity.
            let filter_result = filter::should_accept_with_reason(text, cfg);
            log.write(Event::Filtered {
                text,
                energy,
                reason: filter_result,
            });
            pause::set_last_filtered(text);
            event::global_broadcaster().voice_activity_ended();
            event::global_broadcaster().transcription_filtered(reason);
            Ok(())
        }
        SpeechAction::Hallucinated { reason } => {
            tracing::info!(text, reason = %reason, "hallucination filtered");
            log.write(Event::Filtered {
                text,
                energy,
                reason: filter::FilterReason::HardPhrase(reason.clone()),
            });
            pause::set_last_filtered(text);
            event::global_broadcaster().voice_activity_ended();
            event::global_broadcaster().transcription_filtered(reason);
            Ok(())
        }
        SpeechAction::Paste { text: transformed } => {
            // Log the raw Whisper transcript for debugging STT quality issues
            tracing::info!(
                stage = "whisper_raw",
                transcript = %text,
                "speech-to-text output"
            );

            // Short-utterance bypass: tiny inputs like "yes", "ok", "done"
            // get pasted verbatim. They don't benefit from acoustic fix-up
            // (no glossary term that short) and the LLM round-trip adds
            // latency for zero quality gain. Thresholds are intentionally
            // conservative — anything beyond a one- or two-word reply
            // still flows through the full pipeline.
            let final_text = if is_short_utterance(&transformed) {
                tracing::info!(
                    stage = "short_utterance_bypass",
                    text = %transformed,
                    "skipping correction stack — too short to benefit"
                );
                transformed.clone()
            } else {
                run_corrections(&transformed, cfg, rt)
            };

            tracing::info!(
                stage = "final_result",
                text = %final_text,
                energy = energy,
                "transcript ready for pasting"
            );
            log.write(Event::Pasting {
                raw: text,
                processed: &final_text,
                energy,
            });

            event::global_broadcaster().transcript_final(final_text.clone());

            // Resume-merge path: if the auto-enter countdown is still
            // active from the previous utterance, the user paused only
            // briefly and is now continuing. Append the new transcript
            // to the existing buffered text (preserving sentence casing
            // — see `merge_dictation`) instead of pasting it as a fresh,
            // re-capitalized sentence. Cancels the in-flight countdown
            // and reschedules so the timer restarts from the resumed
            // speech, giving the user the full delay to keep going.
            let merge_active =
                pause::countdown_active() && pause::dictation_buffer_text().is_some();
            let (paste_clipboard, buffer_text) = if merge_active {
                let previous = pause::dictation_buffer_text().unwrap_or_default();
                let (joined, delta) = merge_dictation(&previous, &final_text);
                tracing::info!(
                    stage = "dictation_merge",
                    previous = %previous,
                    addition = %final_text,
                    delta = %delta,
                    joined = %joined,
                    "merging resumed utterance into in-flight dictation"
                );
                pause::countdown_request_cancel();
                // Delta carries its own leading space when needed; no
                // trailing space here because we want the merged buffer
                // to end exactly at the last character pasted so a
                // subsequent merge picks up correct sentence-end state.
                (delta, joined)
            } else {
                // Fresh paste — preserve the historical trailing space
                // on the clipboard payload (clients expect it).
                (format!("{} ", final_text), final_text.clone())
            };

            paste::copy_to_clipboard(paste_clipboard)?;

            // Skip paste if Command is held — likely a shortcut in flight.
            if keyboard::is_cmd_held() {
                tracing::debug!("skipped_paste_cmd_held");
                log.write(Event::Error {
                    message: "Skipped paste: Command key held",
                });
                return Ok(());
            }

            // Always paste WITHOUT enter — the auto-enter Return key
            // is now decoupled, gated behind an optional countdown so
            // the user can intercept (any key cancels). When
            // `auto_enter_delay_ms == 0` the countdown helper presses
            // Return immediately, preserving the legacy behavior.
            paste::paste_text(false)?;

            let auto_enter_effective = per_app::effective_auto_enter(pause::is_auto_enter_enabled());
            if auto_enter_effective {
                // Hold the merged buffer so the NEXT utterance can
                // append again if it arrives before the countdown
                // fires. Set BEFORE schedule() so even a 0-delay
                // immediate-Return path sees the buffer for clearing.
                pause::dictation_buffer_set(buffer_text.clone());
                let delay = per_app::effective_auto_enter_delay_ms(cfg.auto_enter_delay_ms);
                auto_enter_countdown::schedule(rt, delay);
            } else {
                // No auto-enter — no merge window. Drop any stale buffer
                // so the next utterance doesn't accidentally try to
                // merge with text the user has long since committed.
                pause::dictation_buffer_clear();
            }

            // Snapshot the freshly-pasted text as the diff baseline for
            // the manual-correction capture pipeline. We store the
            // post-vocabulary, post-postprocess `final_text` (i.e. the
            // exact bytes the user sees in their app) so the ⌃⌥X hotkey
            // and passive clipboard watcher can compare it against the
            // user's selection without having to reconstruct what was
            // pasted. Only runs on the actually-pasted branch — if
            // Cmd was held the early-return above skipped paste and
            // there's nothing for a correction-diff to anchor on.
            // For merge: store the full joined text so correction tools
            // diff against what's actually on screen.
            pause::set_last_pasted(buffer_text);
            Ok(())
        }
    }
}

fn in_cooldown(now: Instant, last_process: Instant, cooldown_ms: u32) -> bool {
    now.duration_since(last_process).as_millis() < cooldown_ms as u128
}

/// Maximum word count for the short-utterance bypass. Inputs at or below
/// this go straight to paste without the acoustic+LLM correction stack.
pub const SHORT_UTTERANCE_MAX_WORDS: usize = 2;

/// Maximum character count (after trim) for the short-utterance bypass.
/// Belt-and-braces companion to `SHORT_UTTERANCE_MAX_WORDS` so a single
/// long word ("acknowledged") still flows through correction, but a
/// two-letter "ok" does not.
pub const SHORT_UTTERANCE_MAX_CHARS: usize = 8;

/// True when the transcript is short enough to skip the correction stack
/// (`≤ SHORT_UTTERANCE_MAX_WORDS` words OR `≤ SHORT_UTTERANCE_MAX_CHARS`
/// trimmed chars). Empty input also returns true — nothing to correct.
pub fn is_short_utterance(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return true;
    }
    let words = trimmed.split_whitespace().count();
    let chars = trimmed.chars().count();
    words <= SHORT_UTTERANCE_MAX_WORDS || chars <= SHORT_UTTERANCE_MAX_CHARS
}

/// Run the two-stage correction pipeline:
///   1. acoustic glossary fix (`text_match::apply_custom_words`)
///   2. optional LLM grammar cleanup (`PostProcessor::process`)
fn run_corrections(text: &str, cfg: &AlwaysConfig, rt: &Handle) -> String {
    // Stage 1 — acoustic glossary fix-up (Soundex + Levenshtein).
    // Catches Whisper mishears like `cloud → Claude`, deterministic,
    // doesn't invent: substitutes only from the user's approved list.
    let custom_words = crate::glossary::user_glossary_terms();
    let (acoustic, acoustic_subs) = crate::always::text_match::apply_custom_words(
        text,
        &custom_words,
        crate::always::text_match::DEFAULT_THRESHOLD,
    );
    if !acoustic_subs.is_empty() {
        let pairs: Vec<String> = acoustic_subs
            .iter()
            .map(|(w, r)| format!("{w}->{r}"))
            .collect();
        tracing::info!(
            stage = "acoustic_match",
            before = %text,
            after = %acoustic,
            count = acoustic_subs.len(),
            pairs = %pairs.join(", "),
            "glossary corrections applied"
        );
    } else {
        tracing::debug!(
            stage = "acoustic_match",
            text = %acoustic,
            "no glossary corrections needed"
        );
    }

    // Stage 2 — optional LLM grammar cleanup. Gated on the user pref
    // `grammar_correction_enabled` AND a constructed post_processor
    // (which itself silently no-ops without an API key).
    if cfg.postprocess_config.grammar_correction_enabled
        && let Some(ref pp) = cfg.post_processor
    {
        let started = Instant::now();
        match rt.block_on(pp.process(&acoustic, None)) {
            Ok(cleaned) => {
                let elapsed_ms = started.elapsed().as_millis() as u64;
                if acoustic != cleaned {
                    tracing::info!(
                        stage = "grammar_correction",
                        before = %acoustic,
                        after = %cleaned,
                        elapsed_ms = elapsed_ms,
                        "grammar correction applied"
                    );
                } else {
                    tracing::debug!(
                        stage = "grammar_correction",
                        text = %cleaned,
                        elapsed_ms = elapsed_ms,
                        "no grammar changes"
                    );
                }
                cleaned
            }
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    fallback_text = %acoustic,
                    "grammar correction failed, using acoustic match result"
                );
                acoustic
            }
        }
    } else {
        tracing::debug!(
            stage = "grammar_correction",
            text = %acoustic,
            "grammar correction disabled"
        );
        acoustic
    }
}

fn print_banner(cfg: &AlwaysConfig) {
    tracing::info!(
        energy_threshold = cfg.energy_threshold,
        silence_secs = cfg.silence_secs,
        auto_enter = cfg.auto_enter,
        filter_enabled = cfg.filter_enabled,
        log_path = %cfg.log_path.display(),
        "daemon_banner"
    );
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use super::in_cooldown;
    use crate::always::AlwaysConfig;

    fn test_config() -> AlwaysConfig {
        use crate::always::config::{IdlePauseAction, PostprocessConfig, VocabConfig};

        AlwaysConfig {
            lang: "en".to_string(),
            timeout_secs: 30,
            silence_secs: 2.0,
            auto_enter: false,
            filter_enabled: true,
            energy_threshold: 0.05,
            onset_ms: 50,
            cooldown_ms: 1500,
            log_path: PathBuf::from("always.log"),
            post_processor: None,
            project_root: None,
            learning_enabled: false,
            groq_stt_api_key: "test-key".to_string(),
            vad_mode: crate::always::config::VadMode::Local,
            silero_threshold: 0.5,
            vocab_config: VocabConfig::default(),
            postprocess_config: PostprocessConfig::default(),
            auto_enter_delay_ms: 0,
            idle_pause_secs: 0,
            idle_pause_action: IdlePauseAction::default(),
        }
    }

    #[test]
    fn cooldown_uses_millisecond_window() {
        let now = Instant::now();
        assert!(in_cooldown(now, now - Duration::from_millis(1499), 1500));
        assert!(!in_cooldown(now, now - Duration::from_millis(1500), 1500));
    }

    // ----------------------------------------------------------------------
    // is_short_utterance — bypass thresholds.
    // ----------------------------------------------------------------------

    use super::{SHORT_UTTERANCE_MAX_CHARS, SHORT_UTTERANCE_MAX_WORDS, is_short_utterance};

    #[test]
    fn short_utterance_bypass_matches_thresholds() {
        // Empty / whitespace — nothing to correct.
        assert!(is_short_utterance(""));
        assert!(is_short_utterance("   "));

        // Single short word — bypass.
        assert!(is_short_utterance("yes"));
        assert!(is_short_utterance("ok"));
        assert!(is_short_utterance("done"));
        assert!(is_short_utterance("  done  "), "trim before checking");

        // Two short words — bypass (word count threshold).
        assert!(is_short_utterance("yes please"));
        assert!(is_short_utterance("all good"));

        // Spec is `≤2 words OR ≤8 chars` (intentionally permissive) —
        // a single long word still hits the bypass via word count.
        assert!(
            is_short_utterance("acknowledged"),
            "single word always bypasses regardless of length"
        );

        // Three short words but ≤8 chars — char count gates it.
        assert!(
            is_short_utterance("a b c d"),
            "7-char 4-word input still bypasses via char threshold"
        );

        // Three words AND >8 chars — runs full pipeline.
        assert!(
            !is_short_utterance("yes no maybe"),
            "3 words above char threshold must run through correction"
        );
        assert!(
            !is_short_utterance("this is a sentence that needs cleanup"),
            "long multi-word input must run through correction"
        );

        // Sanity: thresholds are at the documented values.
        assert_eq!(SHORT_UTTERANCE_MAX_WORDS, 2);
        assert_eq!(SHORT_UTTERANCE_MAX_CHARS, 8);
    }

    // ----------------------------------------------------------------------
    // classify_transcription — pure decision tree.
    // These cover the speech-handling control flow end-to-end without
    // spawning a daemon, calling Groq, or touching the pasteboard.
    // ----------------------------------------------------------------------

    use super::{SpeechAction, classify_transcription};
    use crate::stt::TranscriptionResult;

    fn empty_transcription(text: &str) -> TranscriptionResult {
        TranscriptionResult {
            text: text.to_string(),
            duration: 1.0,
            language: "en".to_string(),
            segments: vec![],
        }
    }

    #[test]
    fn classify_in_cooldown_returns_no_op() {
        let cfg = test_config();
        let now = Instant::now();
        let action = classify_transcription(
            &cfg,
            "open file",
            &empty_transcription("open file"),
            now,
            // last_process is just now → still in cooldown
            now,
        );
        assert_eq!(action, SpeechAction::InCooldown);
    }

    #[test]
    fn classify_outside_cooldown_pastes_clean_text() {
        let cfg = test_config();
        let now = Instant::now();
        let action = classify_transcription(
            &cfg,
            "open file",
            &empty_transcription("open file"),
            now,
            now - Duration::from_secs(10),
        );
        match action {
            SpeechAction::Paste { text } => assert_eq!(text, "open file"),
            other => panic!("expected Paste, got {:?}", other),
        }
    }

    #[test]
    fn classify_does_not_rewrite_text_pre_llm() {
        // Post-simplification: the pre-LLM pass is a no-op. Text reaches
        // the LLM postprocessor (or paste) verbatim from Whisper.
        let cfg = test_config();
        let now = Instant::now();
        let action = classify_transcription(
            &cfg,
            "open src/main.rs",
            &empty_transcription("open src/main.rs"),
            now,
            now - Duration::from_secs(10),
        );
        match action {
            SpeechAction::Paste { text } => assert_eq!(text, "open src/main.rs"),
            other => panic!("expected Paste, got {:?}", other),
        }
    }

    // ----------------------------------------------------------------------
    // merge_dictation — bug 3: resume after a pause appends to the in-flight
    // dictation buffer instead of pasting a fresh capitalized sentence.
    // ----------------------------------------------------------------------

    use super::{lowercase_if_safe_starter, merge_dictation};

    #[test]
    fn merge_lowercases_safe_starter_mid_sentence() {
        let (joined, delta) = merge_dictation("I went to the store", "And bought milk");
        assert_eq!(joined, "I went to the store and bought milk");
        assert_eq!(delta, " and bought milk");
    }

    #[test]
    fn merge_preserves_capitalization_after_sentence_terminator() {
        let (joined, delta) = merge_dictation("Done.", "And now we continue");
        // Period ended the prior sentence — leave the new sentence's
        // capitalization intact.
        assert_eq!(joined, "Done. And now we continue");
        assert_eq!(delta, " And now we continue");
    }

    #[test]
    fn merge_preserves_proper_noun_at_start_of_continuation() {
        // "Kubernetes" is NOT in the safe-lowercase whitelist, so even
        // mid-sentence we keep its capitalization — the false-positive
        // bias is deliberate.
        let (joined, _) = merge_dictation("I work on", "Kubernetes clusters");
        assert_eq!(joined, "I work on Kubernetes clusters");
    }

    #[test]
    fn merge_keeps_i_capitalized_mid_sentence() {
        // "I" must stay capitalized even when appended mid-sentence.
        let (joined, _) = merge_dictation("Then", "I went home");
        assert_eq!(joined, "Then I went home");
    }

    #[test]
    fn merge_handles_safe_starter_with_trailing_punctuation() {
        // Whisper sometimes emits "And, ..." — punctuation after the
        // first word must not block the lowercase rule.
        let (joined, _) = merge_dictation("Let's go", "And, well, maybe later");
        assert_eq!(joined, "Let's go and, well, maybe later");
    }

    #[test]
    fn merge_does_not_double_space_when_previous_ends_in_space() {
        let (joined, delta) = merge_dictation("Hello ", "And then");
        assert_eq!(joined, "Hello and then");
        assert_eq!(delta, "and then");
    }

    #[test]
    fn merge_with_empty_addition_is_noop() {
        let (joined, delta) = merge_dictation("Existing text", "");
        assert_eq!(joined, "Existing text");
        assert_eq!(delta, "");
    }

    #[test]
    fn merge_after_question_mark_keeps_capital() {
        let (joined, _) = merge_dictation("Ready?", "Then let's start");
        assert_eq!(joined, "Ready? Then let's start");
    }

    #[test]
    fn merge_after_exclamation_keeps_capital() {
        let (joined, _) = merge_dictation("Wow!", "That was fast");
        assert_eq!(joined, "Wow! That was fast");
    }

    #[test]
    fn lowercase_safe_starter_leaves_acronyms_alone() {
        // First word "API" is NOT in the safe-starter list (not in
        // SAFE_LOWERCASE_STARTERS) so it survives untouched — defensive
        // behavior for acronyms.
        let out = lowercase_if_safe_starter("API request was made");
        assert_eq!(out, "API request was made");
    }

    #[test]
    fn lowercase_safe_starter_only_touches_first_word() {
        // Only the first word is considered; "The" gets lowercased,
        // "Cake" is left intact (proper noun by default heuristic).
        let out = lowercase_if_safe_starter("The Cake is ready");
        assert_eq!(out, "the Cake is ready");
    }

    #[test]
    fn classify_rejects_filler_phrases() {
        let cfg = test_config();
        let now = Instant::now();
        let action = classify_transcription(
            &cfg,
            "thanks for watching", // hard-blocklisted phrase
            &empty_transcription("thanks for watching"),
            now,
            now - Duration::from_secs(10),
        );
        match action {
            SpeechAction::Rejected { reason } => {
                assert!(!reason.is_empty(), "rejection should carry a reason");
            }
            other => panic!("expected Rejected, got {:?}", other),
        }
    }
}
