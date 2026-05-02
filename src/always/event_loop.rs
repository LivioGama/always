use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::runtime::Runtime;

use crate::always::log::{Event, Logger};
use crate::always::{
    AlwaysConfig, daemon, event, filter, keyboard, mic_monitor, notification, paste, pause,
    smart_filter, uds_server, vad,
};

pub fn run(cfg: &AlwaysConfig) -> Result<()> {
    let _pid = daemon::PidGuard::install()?;
    let mut log = Logger::open(&cfg.log_path)?;
    log.write(Event::Start { cfg });
    print_banner(&cfg);

    // Initialize the auto-enter state from config
    pause::init_auto_enter(cfg.auto_enter);

    // Start keyboard listener for shortcuts
    keyboard::start_keyboard_listener()?;

    // Show startup notification
    if let Err(e) = notification::notify("ALWAYS", "🎤 Voice activation started", false) {
        eprintln!("Failed to show startup notification: {}", e);
    }

    // Start UDS server in background and track the task
    let rt = Runtime::new()?;
    let _uds_handle = rt.spawn(async {
        if let Err(e) = uds_server::start_server().await {
            eprintln!("UDS server error: {}", e);
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
                    eprintln!(
                        "🎙️  Microphone in use by: {} - Auto-pausing Always",
                        app_list
                    );

                    // Log the auto-pause event
                    log.write(Event::MicrophoneAutoPaused { apps: &app_list });

                    if let Err(e) = notification::notify(
                        "ALWAYS",
                        &format!("🔇 Auto-paused: {} using microphone", app_list),
                        false,
                    ) {
                        eprintln!("Failed to show auto-pause notification: {}", e);
                    }
                }
                pause::set_paused(true);
                auto_paused_for_mic = true;
            }
            Ok(false) if auto_paused_for_mic => {
                // Microphone is no longer in use - resume Always
                eprintln!("🎙️  Microphone available - Auto-resuming Always");

                // Log the auto-resume event
                log.write(Event::MicrophoneAutoResumed);

                if let Err(e) =
                    notification::notify("ALWAYS", "🎤 Auto-resumed: microphone available", false)
                {
                    eprintln!("Failed to show auto-resume notification: {}", e);
                }

                pause::set_paused(false);
                auto_paused_for_mic = false;
            }
            Err(e) => {
                eprintln!("Warning: Failed to check microphone usage: {}", e);
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

        if let Err(e) = process_one(&cfg, &mut log, &mut last_process, &rt) {
            eprintln!("⚠️  Error in voice processing (continuing): {:#}", e);
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
    rt: &Runtime,
) -> Result<()> {
    match vad::record_utterance(cfg, log).context("failed to record/transcribe utterance")? {
        vad::RecordResult::Speech { text, energy } => {
            handle_speech(cfg, log, &text, energy, last_process, rt)?;
        }
        vad::RecordResult::Silence => {
            // Don't log silence events - they're too frequent and not useful
        }
        vad::RecordResult::Timeout => log.write(Event::Timeout),
        vad::RecordResult::DroppedLowEnergy { energy } => {
            log.write(Event::DroppedLowEnergy { energy });
        }
        vad::RecordResult::DroppedNoise { raw } => {
            eprintln!("noise {raw:?}");
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
    last_process: &mut Instant,
    rt: &Runtime,
) -> Result<()> {
    let now = Instant::now();
    if in_cooldown(now, *last_process, cfg.cooldown_ms) {
        return Ok(());
    }
    *last_process = now;

    let (accepted, filter_reason, corrected_text) =
        rt.block_on(smart_filter::should_accept_with_ai(text, cfg));
    if !accepted {
        eprintln!("filtered {text} - {filter_reason}");
        log.write(Event::Filtered {
            text,
            energy,
            reason: filter::FilterReason::HardPhrase(filter_reason),
        });
        return Ok(());
    }

    let accepted_text = corrected_text.as_deref().unwrap_or(text);
    let transformed = apply_vocabulary(accepted_text, cfg);
    eprintln!("{transformed} (energy: {energy:.4})");
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
        eprintln!("⊘ Skipped paste: Command key held");
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
    eprintln!(
        "Always-on mode enabled. Shortcuts: Ctrl+C=stop, Ctrl+Shift+P=pause, Ctrl+Shift+A=auto-enter"
    );
    eprintln!("Log: {}", cfg.log_path.display());
    eprintln!(
        "Settings -> energy_threshold: {} silence: {}s auto_enter: {} filter: {}",
        cfg.energy_threshold, cfg.silence_secs, cfg.auto_enter, cfg.filter_enabled
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
