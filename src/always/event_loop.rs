use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::runtime::Runtime;

use crate::always::log::{Event, Logger};
use crate::always::{
    AlwaysConfig, daemon, event, filter, keyboard, mic_monitor, paste, pause, uds_server, vad,
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

        if let Err(e) = process_one(cfg, &mut log, &mut last_process) {
            tracing::error!(error = %e, "voice_processing_error");
            log.write(Event::Error {
                message: &format!("Voice processing error: {:#}", e),
            });
            // Don't exit - continue running
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}

fn process_one(cfg: &AlwaysConfig, log: &mut Logger, last_process: &mut Instant) -> Result<()> {
    match vad::record_utterance(cfg, log).context("failed to record/transcribe utterance")? {
        vad::RecordResult::Speech {
            text,
            energy,
            transcription,
        } => {
            handle_speech(cfg, log, &text, energy, &transcription, last_process)?;
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

fn handle_speech(
    cfg: &AlwaysConfig,
    log: &mut Logger,
    text: &str,
    energy: f64,
    transcription: &crate::stt::TranscriptionResult,
    last_process: &mut Instant,
) -> Result<()> {
    let now = Instant::now();
    if in_cooldown(now, *last_process, cfg.cooldown_ms) {
        return Ok(());
    }
    *last_process = now;

    log.write(Event::Transcribed { text, energy });

    let filter_result = filter::should_accept_with_reason(text, cfg);
    if !matches!(filter_result, filter::FilterReason::None) {
        let filter_reason = filter_result.to_log_string();
        tracing::info!(text, filter_reason, "filtered");
        log.write(Event::Filtered {
            text,
            energy,
            reason: filter_result,
        });
        pause::set_last_filtered(text);
        event::global_broadcaster().voice_activity_ended();
        event::global_broadcaster().transcription_filtered(filter_reason);
        return Ok(());
    }

    // Hallucination detection (post-API, uses Whisper segment metrics)
    if let Some(reason) = crate::always::hallucination::is_hallucination(transcription) {
        tracing::info!(text, reason, "hallucination filtered");
        log.write(Event::Filtered {
            text,
            energy,
            reason: filter::FilterReason::HardPhrase(reason.to_string()),
        });
        pause::set_last_filtered(text);
        event::global_broadcaster().voice_activity_ended();
        event::global_broadcaster().transcription_filtered(reason);
        return Ok(());
    }

    let accepted_text = text;
    let transformed = apply_vocabulary(accepted_text, cfg);
    tracing::debug!(transformed, energy, "pasting");
    log.write(Event::Pasting {
        raw: text,
        processed: &transformed,
        energy,
    });

    // Send transcript event instead of writing to file
    event::global_broadcaster().transcript_final(transformed.clone());

    // Always copy to clipboard regardless of Command key state
    paste::copy_to_clipboard(format!("{} ", transformed))?;

    // Skip paste (and auto-enter) if the user is currently holding Command —
    // they're likely mid-shortcut (e.g. ⌘+Tab) and an interjecting paste
    // would corrupt their action. Transcript still broadcasted/logged above.
    if keyboard::is_cmd_held() {
        tracing::debug!("skipped_paste_cmd_held");
        log.write(Event::Error {
            message: "Skipped paste: Command key held",
        });
        return Ok(());
    }

    // Use the global auto-enter state instead of config
    let auto_enter = pause::is_auto_enter_enabled();
    paste::paste_text(auto_enter)?;
    Ok(())
}

fn in_cooldown(now: Instant, last_process: Instant, cooldown_ms: u32) -> bool {
    now.duration_since(last_process).as_millis() < cooldown_ms as u128
}

fn apply_vocabulary(text: &str, cfg: &AlwaysConfig) -> String {
    let mut result = text.to_string();

    // Apply base vocabulary
    if let Some(ref vocab) = cfg.vocab {
        result = vocab.apply(&result);
    }

    // Apply learned corrections
    if let Some(ref post_processor) = cfg.post_processor {
        result = post_processor.apply_learned_corrections(&result);
        result = post_processor.code_aware_pattern_match(&result);
    }

    result
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
        }
    }

    #[test]
    fn vocabulary_is_applied_to_text_about_to_paste() {
        let cfg = test_config(Some(Vocabulary::default_patterns()));
        assert_eq!(
            apply_vocabulary("open src/main.rs", &cfg),
            "open `src/main.rs`"
        );
    }

    #[test]
    fn vocabulary_passthrough_when_not_configured() {
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
}
