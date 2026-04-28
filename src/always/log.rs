use std::fs::File;
use std::io::Write as _;
use std::path::Path;

use anyhow::Result;

use crate::always::AlwaysConfig;

pub enum Event<'a> {
    Start { cfg: &'a AlwaysConfig },
    VoiceDetected,
    Pasting { raw: &'a str, processed: &'a str, energy: f64 },
    Filtered { text: &'a str, energy: f64 },
    Silence,
    Timeout,
    DroppedLowEnergy { energy: f64 },
    DroppedNoise { raw: &'a str },
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
            Event::Start { cfg } => format!(
                "START threshold:{} silence:{}s filter:{}",
                cfg.energy_threshold, cfg.silence_secs, cfg.filter_enabled
            ),
            Event::VoiceDetected => "VOICE DETECTED".to_string(),
            Event::Pasting { raw, processed, energy } => {
                format!("TRANSCRIPT {raw} (energy: {energy:.4})\n[{ts}] PASTING    {processed}")
            }
            Event::Filtered { text, energy } => {
                format!("FILTERED  {text} (energy: {energy:.4})")
            }
            Event::Silence => "SILENCE".to_string(),
            Event::Timeout => "TIMEOUT".to_string(),
            Event::DroppedLowEnergy { energy } => {
                format!("DROPPED   (low energy: {energy:.4})")
            }
            Event::DroppedNoise { raw } => format!("DROPPED   (noise) {raw:?}"),
        };
        let _ = writeln!(self.file, "[{ts}] {message}");
    }
}
