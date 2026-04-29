use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::always::log::{Event, Logger};
use crate::always::{AlwaysConfig, daemon, filter, keyboard, notification, paste, pause, vad};
use crate::db;

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

    let mut last_process = Instant::now() - Duration::from_secs(10);
    let mut last_config_reload = Instant::now() - Duration::from_secs(10);

    loop {
        if pause::is_paused() {
            // When paused, sleep for a short time to avoid busy waiting
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }

        // Reload auto_enter config every second
        if Instant::now().duration_since(last_config_reload).as_secs() >= 1 {
            if let Ok(conn) = db::open() {
                if let Ok(prefs) = db::get_preferences(&conn) {
                    let new_auto_enter = prefs.stt_auto_enter.unwrap_or(false);
                    pause::set_auto_enter_enabled(new_auto_enter);
                }
            }
            last_config_reload = Instant::now();
        }

        process_one(&cfg, &mut log, &mut last_process)?;
    }
}

fn process_one(cfg: &AlwaysConfig, log: &mut Logger, last_process: &mut Instant) -> Result<()> {
    match vad::record_utterance(cfg, log).context("failed to record/transcribe utterance")? {
        vad::RecordResult::Speech { text, energy } => {
            handle_speech(cfg, log, &text, energy, last_process)?;
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
) -> Result<()> {
    let now = Instant::now();
    if in_cooldown(now, *last_process, cfg.cooldown_ms) {
        return Ok(());
    }
    *last_process = now;

    if !filter::should_accept(text, cfg) {
        eprintln!("filtered {text}");
        log.write(Event::Filtered { text, energy });
        return Ok(());
    }

    let transformed = apply_vocabulary(text, cfg);
    eprintln!("{transformed} (energy: {energy:.4})");
    log.write(Event::Pasting {
        raw: text,
        processed: &transformed,
        energy,
    });

    // Use the global auto-enter state instead of config
    let auto_enter = pause::is_auto_enter_enabled();
    paste::paste(&format!("{} ", transformed), auto_enter)?;
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
    eprintln!("Always-on mode enabled. Shortcuts: Ctrl+C=stop, Ctrl+Shift+P=pause, Ctrl+Shift+A=auto-enter");
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
        use crate::always::config::{VocabConfig, PostprocessConfig};

        AlwaysConfig {
            lang: "en".to_string(),
            timeout_secs: 30,
            silence_secs: 2.0,
            auto_enter: false,
            filter_enabled: true,
            energy_threshold: 0.05,
            onset_ms: 200,
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
