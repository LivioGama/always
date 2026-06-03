use std::fs::File;
use std::io::Write as _;
use std::path::Path;

use anyhow::Result;

use crate::always::{AlwaysConfig, filter::FilterReason};

pub enum Event<'a> {
    Start {
        cfg: &'a AlwaysConfig,
    },
    Stop,
    VoiceDetected,
    Pasting {
        raw: &'a str,
        processed: &'a str,
        energy: f64,
    },
    Filtered {
        text: &'a str,
        energy: f64,
        reason: FilterReason,
    },
    Silence,
    Timeout,
    DroppedLowEnergy {
        energy: f64,
    },
    DroppedNoise {
        raw: &'a str,
    },
    PauseToggled {
        paused: bool,
    },
    AutoEnterToggled {
        enabled: bool,
    },
    MicrophoneAutoPaused {
        apps: &'a str,
    },
    MicrophoneAutoResumed,
    Error {
        message: &'a str,
    },
}

pub struct Logger {
    file: File,
}

impl Logger {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        Ok(Self { file })
    }

    pub fn write(&mut self, event: Event<'_>) {
        let ts = chrono::Local::now().format("%H:%M:%S");
        let message = match event {
            Event::Start { cfg } => {
                let separator = "─".repeat(80);
                format!(
                    "{}\n│ 🚀 DAEMON STARTED\n│ ⚙️  Energy Threshold: {} • Silence: {}s • Filter: {} • Auto-Enter: {}\n{}",
                    separator,
                    cfg.energy_threshold,
                    cfg.silence_secs,
                    if cfg.filter_enabled { "ON" } else { "OFF" },
                    if cfg.auto_enter { "ON" } else { "OFF" },
                    separator
                )
            }
            Event::Stop => {
                let separator = "─".repeat(80);
                format!("{}\n│ 🛑 DAEMON STOPPED\n{}", separator, separator)
            }
            Event::VoiceDetected => "🎤 Voice detected...".to_string(),
            Event::Pasting {
                raw,
                processed,
                energy,
            } => {
                format!(
                    "✓ Transcribed: \"{raw}\"\n[{ts}] │ Energy: {energy:.4}\n[{ts}] ✎ Pasted: {processed}"
                )
            }
            Event::Filtered { text, energy, reason } => {
                format!("⊘ Filtered: \"{text}\" (energy: {energy:.4}) - {}", reason.to_log_string())
            }
            Event::Silence => "⏸  Silence detected".to_string(),
            Event::Timeout => "⏱  Recording timeout".to_string(),
            Event::DroppedLowEnergy { energy } => {
                format!("✗ Dropped: Energy too low ({energy:.4})")
            }
            Event::DroppedNoise { raw } => format!("✗ Dropped: Background noise \"{raw:?}\""),
            Event::PauseToggled { paused } => {
                if paused {
                    "⏸️  Listening PAUSED (Ctrl+Shift+P to resume)".to_string()
                } else {
                    "▶️  Listening RESUMED".to_string()
                }
            }
            Event::AutoEnterToggled { enabled } => {
                if enabled {
                    "⏎  Auto-Enter ENABLED".to_string()
                } else {
                    "⏎  Auto-Enter DISABLED".to_string()
                }
            }
            Event::MicrophoneAutoPaused { apps } => {
                format!("🔇 Auto-paused: {} using microphone", apps)
            }
            Event::MicrophoneAutoResumed => {
                "🎤 Auto-resumed: microphone available".to_string()
            }
            Event::Error { message } => {
                format!("⚠️  ERROR: {}", message)
            }
        };
        let _ = writeln!(self.file, "[{ts}] {message}");
    }
}
