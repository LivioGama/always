//! JSON output mode for integration with voice systems.
//! Outputs RecordResult as JSON lines for structured parsing.

use anyhow::{Context, Result};
use serde_json::json;

use crate::always::log::{Event, Logger};
use crate::always::{AlwaysConfig, vad};

pub fn listen_json(cfg: &AlwaysConfig) -> Result<()> {
    let mut log = Logger::open(&cfg.log_path)?;
    log.write(Event::Start { cfg });

    eprintln!("JSON listener mode enabled. Outputting RecordResult as JSON lines.");

    loop {
        match vad::record_utterance(cfg, &mut log).context("failed to record/transcribe utterance")? {
            vad::RecordResult::Speech { text, energy } => {
                let output = json!({
                    "type": "speech",
                    "text": text,
                    "energy": energy,
                    "timestamp": chrono::Utc::now().to_rfc3339()
                });
                println!("{}", output);
            }
            vad::RecordResult::Silence => {
                let output = json!({
                    "type": "silence",
                    "timestamp": chrono::Utc::now().to_rfc3339()
                });
                println!("{}", output);
            }
            vad::RecordResult::Timeout => {
                let output = json!({
                    "type": "timeout",
                    "timestamp": chrono::Utc::now().to_rfc3339()
                });
                println!("{}", output);
            }
            vad::RecordResult::DroppedLowEnergy { energy } => {
                let output = json!({
                    "type": "dropped_low_energy",
                    "energy": energy,
                    "timestamp": chrono::Utc::now().to_rfc3339()
                });
                println!("{}", output);
            }
            vad::RecordResult::DroppedNoise { raw } => {
                let output = json!({
                    "type": "dropped_noise",
                    "raw": raw,
                    "timestamp": chrono::Utc::now().to_rfc3339()
                });
                println!("{}", output);
            }
        }
    }
}
