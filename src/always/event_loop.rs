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
    idle_watcher::spawn(rt.handle(), cfg.idle_pause_secs);

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
                pause::set_paused(true);
                event::global_broadcaster().paused();
                auto_paused_for_mic = true;
            }
            Ok(false) if auto_paused_for_mic => {
                // Microphone is no longer in use - resume Always
                tracing::info!("microphone_auto_resumed");

                // Log the auto-resume event
                log.write(Event::MicrophoneAutoResumed);

                pause::set_paused(false);
                event::global_broadcaster().resumed();
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

    SpeechAction::Paste {
        text: apply_vocabulary(text, cfg),
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
            // Single optional cleanup: LLM postprocess (off by default).
            // The Whisper transcript is otherwise pasted verbatim. The
            // glossary's curated `mistranscriptions` are surfaced to the
            // LLM via the postprocess prompt; deterministic regex
            // substitution was tried and removed because it kept firing
            // on natural speech ("structure" → "struct" etc.).
            let final_text = if cfg.postprocess_config.grammar_correction_enabled
                && let Some(ref pp) = cfg.post_processor
            {
                let started = Instant::now();
                match rt.block_on(pp.process(&transformed, None)) {
                    Ok(cleaned) => {
                        tracing::debug!(
                            elapsed_ms = started.elapsed().as_millis() as u64,
                            "postprocess_applied"
                        );
                        cleaned
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "postprocess_failed_fallback_to_vocab");
                        transformed
                    }
                }
            } else {
                transformed
            };

            tracing::debug!(final_text = %final_text, energy, "pasting");
            log.write(Event::Pasting {
                raw: text,
                processed: &final_text,
                energy,
            });

            event::global_broadcaster().transcript_final(final_text.clone());

            paste::copy_to_clipboard(format!("{} ", final_text))?;

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
                let delay = per_app::effective_auto_enter_delay_ms(cfg.auto_enter_delay_ms);
                auto_enter_countdown::schedule(rt, delay);
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
            pause::set_last_pasted(final_text.clone());
            Ok(())
        }
    }
}

fn in_cooldown(now: Instant, last_process: Instant, cooldown_ms: u32) -> bool {
    now.duration_since(last_process).as_millis() < cooldown_ms as u128
}

/// Pre-LLM transform pass.
///
/// Used to chain Vocabulary::apply (regex glossary), apply_fuzzy_corrections,
/// apply_learned_corrections, and code_aware_pattern_match here. All four
/// were deleted in the pipeline-simplification pass — they piled up bug-
/// by-bug and produced non-debuggable interactions. The single remaining
/// transformation is the optional LLM postprocessor invoked downstream
/// from this function in `handle_speech`. Voice-to-text delivers what was
/// said by default; users opt into the LLM cleanup via
/// `postprocess_enabled` pref.
///
/// Kept as a function (rather than inlining) so the call site in
/// `classify_transcription` and the unit tests don't need to change.
fn apply_vocabulary(text: &str, _cfg: &AlwaysConfig) -> String {
    text.to_string()
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

    use super::{apply_vocabulary, in_cooldown};
    use crate::always::AlwaysConfig;
    use crate::always::text::Vocabulary;

    fn test_config(vocab: Option<Vocabulary>) -> AlwaysConfig {
        use crate::always::config::{PostprocessConfig, VocabConfig};

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
            vocab,
            context_vocab: None,
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
        }
    }

    #[test]
    fn apply_vocabulary_is_pure_passthrough_after_simplification() {
        // After collapsing the 6-layer text-mutation pipeline into a
        // single optional LLM postprocessor, this function is now
        // intentionally a no-op. Voice-to-text delivers what was said;
        // any cleanup happens downstream in `handle_speech` via the
        // LLM postprocessor (gated on `postprocess_enabled` pref).
        //
        // This test pins the contract so a future commit doesn't
        // silently re-introduce a transformation step here.
        let cfg = test_config(Some(Vocabulary::default_patterns()));
        assert_eq!(
            apply_vocabulary("open src/main.rs", &cfg),
            "open src/main.rs",
            "apply_vocabulary must not transform text — verbatim Whisper output is the contract"
        );
        assert_eq!(
            apply_vocabulary("I have an idea about Kubernetes.", &cfg),
            "I have an idea about Kubernetes.",
            "common-English words must NEVER be rewritten by the pre-LLM pass"
        );
    }

    #[test]
    fn vocabulary_passthrough_when_not_configured() {
        // Same contract regardless of whether `cfg.vocab` is set —
        // the function is a no-op either way.
        let cfg = test_config(None);
        assert_eq!(
            apply_vocabulary("open src/main.rs", &cfg),
            "open src/main.rs"
        );
    }

    #[test]
    fn cooldown_uses_millisecond_window() {
        let now = Instant::now();
        assert!(in_cooldown(now, now - Duration::from_millis(1499), 1500));
        assert!(!in_cooldown(now, now - Duration::from_millis(1500), 1500));
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
        let cfg = test_config(None);
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
        let cfg = test_config(None);
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
        let cfg = test_config(Some(Vocabulary::default_patterns()));
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

    #[test]
    fn classify_rejects_filler_phrases() {
        let cfg = test_config(None);
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
