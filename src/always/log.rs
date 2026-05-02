use crate::always::{AlwaysConfig, filter::FilterReason};
use crate::always::telemetry::should_log_transcripts;

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

/// Logger now uses tracing infrastructure instead of file I/O
/// The Event enum is kept for API compatibility but emission is handled via tracing
pub struct Logger;

impl Logger {
    /// Create a new logger (no-op with tracing, kept for API compatibility)
    pub fn open(_path: &std::path::Path) -> anyhow::Result<Self> {
        // With tracing, log file management is handled by the telemetry module
        Ok(Self)
    }

    /// Emit an event using structured tracing
    pub fn write(&mut self, event: Event<'_>) {
        match event {
            Event::Start { cfg } => {
                tracing::info!(
                    energy_threshold = cfg.energy_threshold,
                    silence_secs = cfg.silence_secs,
                    filter_enabled = cfg.filter_enabled,
                    auto_enter = cfg.auto_enter,
                    "daemon_started"
                );
            }
            Event::Stop => {
                tracing::info!("daemon_stopped");
            }
            Event::VoiceDetected => {
                tracing::debug!("voice_detected");
            }
            Event::Pasting {
                raw,
                processed,
                energy,
            } => {
                let log_transcripts = should_log_transcripts();
                tracing::info!(
                    chars = raw.len(),
                    energy,
                    processed_chars = processed.len(),
                    raw_text = if log_transcripts { Some(raw) } else { None },
                    processed_text = if log_transcripts { Some(processed) } else { None },
                    "transcription_pasted"
                );
            }
            Event::Filtered { text, energy, reason } => {
                let log_transcripts = should_log_transcripts();
                tracing::info!(
                    chars = text.len(),
                    energy,
                    reason = reason.to_log_string(),
                    text = if log_transcripts { Some(text) } else { None },
                    "transcription_filtered"
                );
            }
            Event::Silence => {
                tracing::debug!("silence_detected");
            }
            Event::Timeout => {
                tracing::debug!("recording_timeout");
            }
            Event::DroppedLowEnergy { energy } => {
                tracing::debug!(energy, "dropped_low_energy");
            }
            Event::DroppedNoise { raw } => {
                let log_transcripts = should_log_transcripts();
                tracing::debug!(
                    chars = raw.len(),
                    text = if log_transcripts { Some(raw) } else { None },
                    "dropped_noise"
                );
            }
            Event::PauseToggled { paused } => {
                tracing::info!(paused, "pause_toggled");
            }
            Event::AutoEnterToggled { enabled } => {
                tracing::info!(enabled, "auto_enter_toggled");
            }
            Event::MicrophoneAutoPaused { apps } => {
                tracing::info!(apps, "microphone_auto_paused");
            }
            Event::MicrophoneAutoResumed => {
                tracing::info!("microphone_auto_resumed");
            }
            Event::Error { message } => {
                tracing::error!(message, "daemon_error");
            }
        }
    }
}
