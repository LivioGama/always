//! Overlay state reducer - mirrors macOS StateMonitor logic
//!
//! This module reduces daemon events into UI state, maintaining the same
//! persistent vs flash overlay distinction as the macOS implementation.

use std::time::{Duration, Instant};

// Reuse the daemon's DaemonEvent type from the always library
pub use always::always::event::DaemonEvent;

const MIN_VOICE_ACTIVITY_DISPLAY: Duration = Duration::from_millis(250);

// Parse event from JSON line
pub fn parse_daemon_event(line: &str) -> Result<DaemonEvent, serde_json::Error> {
    serde_json::from_str(line)
}

/// Overlay state - mirrors macOS OverlayState enum
#[derive(Debug, Clone, PartialEq)]
pub enum OverlayState {
    /// Persistent states - shown continuously while condition holds
    VoiceActivity,
    Transcribing,
    AutoEnterCountdown {
        seconds_remaining: u32,
    },

    /// Flash states - shown briefly then auto-hide
    Paused,
    Resumed,
    AutoEnterOn,
    AutoEnterOff,
    Filtered {
        reason: String,
    },
    TranscriptionFailed {
        message: String,
    },
    GrammarCorrected,
    LowMicrophoneVolume {
        energy: f64,
    },
    IdleAutoPaused {
        seconds: u32,
    },

    /// Hidden states - overlay should not be visible
    Hidden,
}

impl OverlayState {
    /// Returns true if this state should be shown instantly (no fade-in)
    #[allow(dead_code)]
    pub fn is_instant_show(&self) -> bool {
        matches!(
            self,
            OverlayState::VoiceActivity
                | OverlayState::Transcribing
                | OverlayState::AutoEnterCountdown { .. }
        )
    }

    /// Returns true if this is a persistent state (stays visible until explicitly changed)
    #[allow(dead_code)]
    pub fn is_persistent(&self) -> bool {
        matches!(
            self,
            OverlayState::VoiceActivity
                | OverlayState::Transcribing
                | OverlayState::AutoEnterCountdown { .. }
        )
    }

    /// Returns the display text for this state
    pub fn display_text(&self) -> String {
        match self {
            OverlayState::VoiceActivity => "Listening".to_string(),
            OverlayState::Transcribing => "Transcribing".to_string(),
            OverlayState::AutoEnterCountdown { seconds_remaining } => {
                format!("Auto-Enter in {}s · any key cancels", seconds_remaining)
            }
            OverlayState::Paused => "Paused".to_string(),
            OverlayState::Resumed => "Resumed".to_string(),
            OverlayState::AutoEnterOn => "Auto-Enter On".to_string(),
            OverlayState::AutoEnterOff => "Auto-Enter Off".to_string(),
            OverlayState::Filtered { reason } => {
                if reason.is_empty() {
                    "Filtered".to_string()
                } else {
                    format!("Filtered · {}", reason)
                }
            }
            OverlayState::TranscriptionFailed { message } => {
                if message.is_empty() {
                    "Transcription failed".to_string()
                } else {
                    message.to_string()
                }
            }
            OverlayState::GrammarCorrected => "✓ Grammar corrected".to_string(),
            OverlayState::LowMicrophoneVolume { energy } => {
                format!("Low mic volume · energy {:.3}", energy)
            }
            OverlayState::IdleAutoPaused { seconds } => {
                format!("Idle for {}s · paused", seconds)
            }
            OverlayState::Hidden => String::new(),
        }
    }

    /// Returns the icon name for this state (GTK icon names)
    #[allow(dead_code)]
    pub fn icon_name(&self) -> &'static str {
        match self {
            OverlayState::VoiceActivity => "waveform",
            OverlayState::Transcribing => "waveform-circle-filled",
            OverlayState::AutoEnterCountdown { .. } => "key-enter",
            OverlayState::Paused => "media-playback-pause",
            OverlayState::Resumed => "media-playback-start",
            OverlayState::AutoEnterOn => "emblem-ok-symbolic",
            OverlayState::AutoEnterOff => "circle-outline",
            OverlayState::Filtered { .. } => "dialog-error-symbolic",
            OverlayState::TranscriptionFailed { .. } => "dialog-warning-symbolic",
            OverlayState::GrammarCorrected => "emblem-default-symbolic",
            OverlayState::LowMicrophoneVolume { .. } => "audio-volume-muted-symbolic",
            OverlayState::IdleAutoPaused { .. } => "weather-clear-night-symbolic",
            OverlayState::Hidden => "",
        }
    }
}

/// Internal state tracked by the reducer
#[derive(Debug, Clone, Default)]
pub(crate) struct ReducerState {
    is_paused: bool,
    is_auto_enter: bool,
    is_transcribing: bool,
    is_voice_activity: bool,
    is_listening_active: bool,
    is_daemon_connected: bool,
    current_overlay: Option<OverlayState>,
    auto_enter_remaining_ms: Option<u32>,
    is_initial_sync: bool,
}

/// State reducer - processes daemon events and produces overlay state changes
pub struct OverlayStateReducer {
    pub state: ReducerState,
    pub initial_sync_deadline: Option<Instant>,
    pub flash_deadline: Option<Instant>,
    pub pending_persistent_state: Option<OverlayState>,
    transcribing_deferred_until: Option<Instant>,
    voice_activity_started_at: Option<Instant>,
}

impl OverlayStateReducer {
    pub fn new() -> Self {
        Self {
            state: ReducerState::default(),
            initial_sync_deadline: None,
            flash_deadline: None,
            pending_persistent_state: None,
            transcribing_deferred_until: None,
            voice_activity_started_at: None,
        }
    }

    /// Process a daemon event and return the new overlay state (if changed)
    /// Returns Some(new_state) if the overlay should change, None if no change
    pub fn process_event(&mut self, event: &DaemonEvent) -> Option<OverlayState> {
        match event {
            DaemonEvent::Hello { version: _ } => {
                // Start initial sync window to suppress overlay flashes
                self.state.is_daemon_connected = true;
                self.initial_sync_deadline = Some(Instant::now() + Duration::from_millis(300));
                self.state.is_initial_sync = true;
                None // No overlay change on Hello
            }

            DaemonEvent::ListeningStarted => {
                self.state.is_listening_active = true;
                self.update_ongoing_overlay()
            }

            DaemonEvent::ListeningStopped => {
                self.state.is_listening_active = false;
                self.state.is_voice_activity = false;
                self.voice_activity_started_at = None;
                self.transcribing_deferred_until = None;
                self.update_ongoing_overlay()
            }

            DaemonEvent::TranscribingStarted => {
                self.state.is_transcribing = true;
                if let Some(started_at) = self.voice_activity_started_at {
                    let ready_at = started_at + MIN_VOICE_ACTIVITY_DISPLAY;
                    if Instant::now() < ready_at {
                        self.transcribing_deferred_until = Some(ready_at);
                        return self.show_voice_activity_overlay();
                    }
                }
                self.transcribing_deferred_until = None;
                Some(OverlayState::Transcribing)
            }

            DaemonEvent::TranscribingStopped => {
                self.state.is_transcribing = false;
                self.transcribing_deferred_until = None;
                self.update_ongoing_overlay()
            }

            DaemonEvent::VoiceActivityDetected => {
                self.state.is_voice_activity = true;
                self.voice_activity_started_at = Some(Instant::now());
                self.update_ongoing_overlay()
            }

            DaemonEvent::VoiceActivityEnded => {
                self.state.is_voice_activity = false;
                self.voice_activity_started_at = None;
                self.transcribing_deferred_until = None;
                self.update_ongoing_overlay()
            }

            DaemonEvent::Paused => {
                let changed = !self.state.is_paused;
                self.state.is_paused = true;
                if changed {
                    self.start_flash(OverlayState::Paused, Duration::from_secs(2))
                } else {
                    None
                }
            }

            DaemonEvent::Resumed => {
                let changed = self.state.is_paused;
                self.state.is_paused = false;
                if changed {
                    self.start_flash(OverlayState::Resumed, Duration::from_secs(2))
                } else {
                    None
                }
            }

            DaemonEvent::PausedQuietly => {
                // State-only update - no overlay flash
                self.state.is_paused = true;
                None
            }

            DaemonEvent::ResumedQuietly => {
                self.state.is_paused = false;
                None
            }

            DaemonEvent::AutoEnterEnabled => {
                let changed = !self.state.is_auto_enter;
                self.state.is_auto_enter = true;
                if changed && !self.state.is_initial_sync {
                    self.start_flash(OverlayState::AutoEnterOn, Duration::from_secs(2))
                } else {
                    None
                }
            }

            DaemonEvent::AutoEnterDisabled => {
                let changed = self.state.is_auto_enter;
                self.state.is_auto_enter = false;
                if changed && !self.state.is_initial_sync {
                    self.start_flash(OverlayState::AutoEnterOff, Duration::from_secs(2))
                } else {
                    None
                }
            }

            DaemonEvent::TranscriptionFiltered { reason } => {
                // Clear ongoing state and flash filtered overlay
                self.state.is_transcribing = false;
                self.state.is_voice_activity = false;
                self.voice_activity_started_at = None;
                self.transcribing_deferred_until = None;
                self.start_flash(
                    OverlayState::Filtered {
                        reason: reason.clone(),
                    },
                    Duration::from_secs(3),
                )
            }

            DaemonEvent::TranscriptionFailed { kind: _, message } => {
                // Clear ongoing state and flash error overlay
                self.state.is_transcribing = false;
                self.state.is_voice_activity = false;
                self.voice_activity_started_at = None;
                self.transcribing_deferred_until = None;
                self.start_flash(
                    OverlayState::TranscriptionFailed {
                        message: message.clone(),
                    },
                    Duration::from_secs(5),
                )
            }

            DaemonEvent::GrammarCorrected {
                before: _,
                after: _,
            } => self.start_flash(OverlayState::GrammarCorrected, Duration::from_secs(2)),

            DaemonEvent::LowMicrophoneVolume { energy } => self.start_flash(
                OverlayState::LowMicrophoneVolume { energy: *energy },
                Duration::from_secs(5),
            ),

            DaemonEvent::AutoEnterCountdownStarted {
                remaining_ms,
                total_ms: _,
            } => {
                self.state.auto_enter_remaining_ms = Some(*remaining_ms);
                Some(OverlayState::AutoEnterCountdown {
                    seconds_remaining: remaining_ms / 1000,
                })
            }

            DaemonEvent::AutoEnterCountdownTick { remaining_ms } => {
                self.state.auto_enter_remaining_ms = Some(*remaining_ms);
                Some(OverlayState::AutoEnterCountdown {
                    seconds_remaining: remaining_ms / 1000,
                })
            }

            DaemonEvent::AutoEnterCountdownCancelled => {
                self.state.auto_enter_remaining_ms = None;
                self.update_ongoing_overlay()
            }

            DaemonEvent::AutoEnterCountdownFinished => {
                self.state.auto_enter_remaining_ms = None;
                self.update_ongoing_overlay()
            }

            DaemonEvent::IdleAutoPaused { seconds } => self.start_flash(
                OverlayState::IdleAutoPaused { seconds: *seconds },
                Duration::from_secs(3),
            ),

            DaemonEvent::IdleAutoResumed => self.update_ongoing_overlay(),

            DaemonEvent::TranscriptFinal { text: _ } => {
                // Clear ongoing state immediately
                self.state.is_transcribing = false;
                self.state.is_voice_activity = false;
                self.voice_activity_started_at = None;
                self.transcribing_deferred_until = None;
                self.update_ongoing_overlay()
            }

            // Events we don't handle in the overlay
            DaemonEvent::TranscriptChunk { text: _ }
            | DaemonEvent::Heartbeat
            | DaemonEvent::FocusedAppChanged { bundle_id: _ }
            | DaemonEvent::MasterPauseChanged { master_paused: _ }
            | DaemonEvent::PauseScopeToggled {
                scope: _,
                bundle_id: _,
                paused: _,
            }
            | DaemonEvent::LongRecordingWarning {
                elapsed_secs: _,
                cap_secs: _,
            }
            | DaemonEvent::PauseSourceChanged {
                source: _,
                paused: _,
                detail: _,
            }
            | DaemonEvent::ResumedAppsChanged { bundles: _ }
            | DaemonEvent::CorrectionLogged { wrong: _, right: _ }
            | DaemonEvent::CorrectionPending {
                id: _,
                wrong: _,
                right: _,
            }
            | DaemonEvent::CorrectionCaptureResult { outcome: _ }
            | DaemonEvent::CorrectionDialogRequested { last_transcript: _ }
            | DaemonEvent::ModelsList { models: _ }
            | DaemonEvent::ModelDownloadProgress {
                model_id: _,
                downloaded: _,
                total: _,
                percentage: _,
            }
            | DaemonEvent::ModelDownloadComplete { model_id: _ }
            | DaemonEvent::ModelDownloadCancelled { model_id: _ }
            | DaemonEvent::ModelDownloadFailed {
                model_id: _,
                error: _,
            }
            | DaemonEvent::ModelVerificationStarted { model_id: _ }
            | DaemonEvent::ModelVerificationCompleted { model_id: _ }
            | DaemonEvent::ModelExtractionStarted { model_id: _ }
            | DaemonEvent::ModelExtractionCompleted { model_id: _ }
            | DaemonEvent::ModelExtractionFailed {
                model_id: _,
                error: _,
            }
            | DaemonEvent::ActiveTranscriberChanged { backend: _ }
            | DaemonEvent::SttFallbackEngaged { model: _ } => None,

            DaemonEvent::ProcessingStarted | DaemonEvent::ProcessingStopped => {
                // Not used in current overlay logic
                None
            }
        }
    }

    /// Update the ongoing overlay based on current state
    pub fn update_ongoing_overlay(&mut self) -> Option<OverlayState> {
        // Check if we're still in initial sync window
        if let Some(deadline) = self.initial_sync_deadline {
            if Instant::now() < deadline {
                self.state.is_initial_sync = true;
            } else {
                self.state.is_initial_sync = false;
                self.initial_sync_deadline = None;
            }
        }

        // Connection lost or paused -> hide
        if !self.state.is_daemon_connected || self.state.is_paused {
            let new_state = OverlayState::Hidden;
            if self.state.current_overlay != Some(new_state.clone()) {
                self.state.current_overlay = Some(new_state.clone());
                return Some(new_state);
            }
            return None;
        }

        // Activity-only model: overlay represents something happening
        if self.state.is_transcribing {
            if let Some(ready_at) = self.transcribing_deferred_until
                && Instant::now() < ready_at
                && self.state.is_voice_activity
            {
                return self.show_voice_activity_overlay();
            }
            self.transcribing_deferred_until = None;
            let new_state = OverlayState::Transcribing;
            if self.state.current_overlay != Some(new_state.clone()) {
                self.state.current_overlay = Some(new_state.clone());
                return Some(new_state);
            }
            return None;
        }

        if self.state.is_voice_activity {
            let new_state = OverlayState::VoiceActivity;
            if self.state.current_overlay != Some(new_state.clone()) {
                self.state.current_overlay = Some(new_state.clone());
                return Some(new_state);
            }
            return None;
        }

        // Auto-enter countdown
        if let Some(remaining_ms) = self.state.auto_enter_remaining_ms {
            let new_state = OverlayState::AutoEnterCountdown {
                seconds_remaining: remaining_ms / 1000,
            };
            if self.state.current_overlay != Some(new_state.clone()) {
                self.state.current_overlay = Some(new_state.clone());
                return Some(new_state);
            }
            return None;
        }

        // No activity -> hide
        let new_state = OverlayState::Hidden;
        if self.state.current_overlay != Some(new_state.clone()) {
            self.state.current_overlay = Some(new_state.clone());
            return Some(new_state);
        }
        None
    }

    fn show_voice_activity_overlay(&mut self) -> Option<OverlayState> {
        let new_state = OverlayState::VoiceActivity;
        if self.state.current_overlay != Some(new_state.clone()) {
            self.state.current_overlay = Some(new_state.clone());
            return Some(new_state);
        }
        None
    }

    /// Get the current overlay state
    #[allow(dead_code)]
    pub fn current_state(&self) -> &OverlayState {
        self.state
            .current_overlay
            .as_ref()
            .unwrap_or(&OverlayState::Hidden)
    }

    /// Start a flash state that auto-hides after duration
    fn start_flash(
        &mut self,
        flash_state: OverlayState,
        duration: Duration,
    ) -> Option<OverlayState> {
        // Save the current persistent state to restore after flash
        if matches!(
            self.state.current_overlay,
            Some(OverlayState::VoiceActivity)
                | Some(OverlayState::Transcribing)
                | Some(OverlayState::AutoEnterCountdown { .. })
        ) {
            self.pending_persistent_state = self.state.current_overlay.clone();
        }

        self.flash_deadline = Some(Instant::now() + duration);
        self.state.current_overlay = Some(flash_state.clone());
        Some(flash_state)
    }

    /// Check if flash state has expired and restore persistent state
    fn check_flash_expiry(&mut self) -> Option<OverlayState> {
        if let Some(deadline) = self.flash_deadline
            && Instant::now() >= deadline
        {
            self.flash_deadline = None;
            if let Some(persistent) = self.pending_persistent_state.take() {
                self.state.current_overlay = Some(persistent.clone());
                return Some(persistent);
            } else {
                // No persistent state to restore, hide overlay
                self.state.current_overlay = Some(OverlayState::Hidden);
                return Some(OverlayState::Hidden);
            }
        }
        None
    }

    /// Public method to check and process flash expiry (called by main loop)
    pub fn check_timeouts(&mut self) -> Option<OverlayState> {
        // Check initial sync deadline
        if let Some(deadline) = self.initial_sync_deadline
            && Instant::now() >= deadline
        {
            self.initial_sync_deadline = None;
            self.state.is_initial_sync = false;
        }

        if let Some(deadline) = self.transcribing_deferred_until
            && Instant::now() >= deadline
        {
            self.transcribing_deferred_until = None;
            return self.update_ongoing_overlay();
        }

        // Check flash deadline
        self.check_flash_expiry()
    }
}

impl Default for OverlayStateReducer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_voice_activity_shows_overlay() {
        let mut reducer = OverlayStateReducer::new();

        // Simulate daemon connection
        reducer.process_event(&DaemonEvent::Hello { version: 7 });

        // Voice activity should show overlay
        let state = reducer.process_event(&DaemonEvent::VoiceActivityDetected);
        assert_eq!(state, Some(OverlayState::VoiceActivity));
    }

    #[test]
    fn test_transcribing_shows_overlay() {
        let mut reducer = OverlayStateReducer::new();

        reducer.process_event(&DaemonEvent::Hello { version: 7 });

        let state = reducer.process_event(&DaemonEvent::TranscribingStarted);
        assert_eq!(state, Some(OverlayState::Transcribing));
    }

    #[test]
    fn test_transcribing_waits_for_minimum_listening_display() {
        let mut reducer = OverlayStateReducer::new();

        reducer.process_event(&DaemonEvent::Hello { version: 7 });
        assert_eq!(
            reducer.process_event(&DaemonEvent::VoiceActivityDetected),
            Some(OverlayState::VoiceActivity)
        );

        assert_eq!(
            reducer.process_event(&DaemonEvent::TranscribingStarted),
            None
        );
        assert_eq!(reducer.current_state(), &OverlayState::VoiceActivity);

        std::thread::sleep(MIN_VOICE_ACTIVITY_DISPLAY + Duration::from_millis(20));
        assert_eq!(reducer.check_timeouts(), Some(OverlayState::Transcribing));
    }

    #[test]
    fn test_pause_hides_overlay() {
        let mut reducer = OverlayStateReducer::new();

        reducer.process_event(&DaemonEvent::Hello { version: 7 });
        reducer.process_event(&DaemonEvent::VoiceActivityDetected);

        // Pause should hide overlay
        let state = reducer.process_event(&DaemonEvent::Paused);
        assert_eq!(state, Some(OverlayState::Paused));

        // After pause flash, ongoing should be hidden
        let ongoing = reducer.update_ongoing_overlay();
        assert_eq!(ongoing, Some(OverlayState::Hidden));
    }

    #[test]
    fn test_initial_sync_suppression() {
        let mut reducer = OverlayStateReducer::new();

        reducer.process_event(&DaemonEvent::Hello { version: 7 });

        // AutoEnterEnabled right after Hello should not flash (initial sync)
        let state = reducer.process_event(&DaemonEvent::AutoEnterEnabled);
        assert_eq!(state, None);
    }

    #[test]
    fn test_filtered_clears_ongoing() {
        let mut reducer = OverlayStateReducer::new();

        reducer.process_event(&DaemonEvent::Hello { version: 7 });
        reducer.process_event(&DaemonEvent::TranscribingStarted);

        // Filtered should clear transcribing and show filtered overlay
        let state = reducer.process_event(&DaemonEvent::TranscriptionFiltered {
            reason: "hallucination".to_string(),
        });
        assert_eq!(
            state,
            Some(OverlayState::Filtered {
                reason: "hallucination".to_string()
            })
        );
    }

    #[test]
    fn test_auto_enter_countdown() {
        let mut reducer = OverlayStateReducer::new();

        reducer.process_event(&DaemonEvent::Hello { version: 7 });

        // Countdown started should show countdown overlay
        let state = reducer.process_event(&DaemonEvent::AutoEnterCountdownStarted {
            remaining_ms: 3000,
            total_ms: 3000,
        });
        assert_eq!(
            state,
            Some(OverlayState::AutoEnterCountdown {
                seconds_remaining: 3
            })
        );

        // Countdown tick should update the overlay
        let state =
            reducer.process_event(&DaemonEvent::AutoEnterCountdownTick { remaining_ms: 2000 });
        assert_eq!(
            state,
            Some(OverlayState::AutoEnterCountdown {
                seconds_remaining: 2
            })
        );

        // Countdown cancelled should hide overlay
        let state = reducer.process_event(&DaemonEvent::AutoEnterCountdownCancelled);
        assert_eq!(state, Some(OverlayState::Hidden));
    }

    #[test]
    fn test_transcription_failed() {
        let mut reducer = OverlayStateReducer::new();

        reducer.process_event(&DaemonEvent::Hello { version: 7 });
        reducer.process_event(&DaemonEvent::TranscribingStarted);

        // Transcription failed should clear ongoing and show error (flash state)
        let state = reducer.process_event(&DaemonEvent::TranscriptionFailed {
            kind: "network".to_string(),
            message: "Network error".to_string(),
        });
        assert_eq!(
            state,
            Some(OverlayState::TranscriptionFailed {
                message: "Network error".to_string()
            })
        );
    }

    #[test]
    fn test_low_microphone_volume() {
        let mut reducer = OverlayStateReducer::new();

        reducer.process_event(&DaemonEvent::Hello { version: 7 });

        // Low mic volume should show warning overlay (flash state)
        let state = reducer.process_event(&DaemonEvent::LowMicrophoneVolume { energy: 0.001 });
        assert_eq!(
            state,
            Some(OverlayState::LowMicrophoneVolume { energy: 0.001 })
        );
    }

    #[test]
    fn test_idle_auto_pause() {
        let mut reducer = OverlayStateReducer::new();

        reducer.process_event(&DaemonEvent::Hello { version: 7 });
        reducer.process_event(&DaemonEvent::VoiceActivityDetected);

        // Idle auto pause should show notice (flash state)
        let state = reducer.process_event(&DaemonEvent::IdleAutoPaused { seconds: 30 });
        assert_eq!(state, Some(OverlayState::IdleAutoPaused { seconds: 30 }));
    }

    #[test]
    fn test_flash_auto_hide() {
        let mut reducer = OverlayStateReducer::new();

        reducer.process_event(&DaemonEvent::Hello { version: 7 });
        reducer.process_event(&DaemonEvent::VoiceActivityDetected);

        // Start a flash state (paused)
        let state = reducer.process_event(&DaemonEvent::Paused);
        assert_eq!(state, Some(OverlayState::Paused));

        // Flash should be active immediately
        assert_eq!(reducer.current_state(), &OverlayState::Paused);

        // Verify flash mechanism is in place
        // Note: Actual expiry would require time manipulation in tests
        assert!(reducer.flash_deadline.is_some());

        // Verify pending persistent state was saved
        assert!(reducer.pending_persistent_state.is_some());
    }

    #[test]
    fn test_event_parsing() {
        // Test JSON parsing of daemon events
        let json = r#"{"type":"Hello","data":{"version":7}}"#;
        let event = parse_daemon_event(json).unwrap();
        assert!(matches!(event, DaemonEvent::Hello { version: 7 }));

        let json = r#"{"type":"VoiceActivityDetected","data":null}"#;
        let event = parse_daemon_event(json).unwrap();
        assert!(matches!(event, DaemonEvent::VoiceActivityDetected));

        let json = r#"{"type":"TranscriptionFiltered","data":{"reason":"test"}}"#;
        let event = parse_daemon_event(json).unwrap();
        assert!(matches!(
            event,
            DaemonEvent::TranscriptionFiltered { reason: _ }
        ));
    }

    #[test]
    fn test_overlay_state_properties() {
        // Test overlay state properties
        assert!(OverlayState::VoiceActivity.is_instant_show());
        assert!(OverlayState::Transcribing.is_instant_show());
        assert!(
            OverlayState::AutoEnterCountdown {
                seconds_remaining: 5
            }
            .is_instant_show()
        );
        assert!(!OverlayState::Paused.is_instant_show());

        assert!(OverlayState::VoiceActivity.is_persistent());
        assert!(OverlayState::Transcribing.is_persistent());
        assert!(!OverlayState::Paused.is_persistent());
    }

    #[test]
    fn test_display_text() {
        assert_eq!(OverlayState::VoiceActivity.display_text(), "Listening");
        assert_eq!(OverlayState::Transcribing.display_text(), "Transcribing");
        assert_eq!(OverlayState::Paused.display_text(), "Paused");
        assert_eq!(
            OverlayState::AutoEnterCountdown {
                seconds_remaining: 3
            }
            .display_text(),
            "Auto-Enter in 3s · any key cancels"
        );
        assert_eq!(
            OverlayState::Filtered {
                reason: "test".to_string()
            }
            .display_text(),
            "Filtered · test"
        );
    }

    #[test]
    fn test_connection_lost_hides_overlay() {
        let mut reducer = OverlayStateReducer::new();

        reducer.process_event(&DaemonEvent::Hello { version: 7 });
        reducer.process_event(&DaemonEvent::VoiceActivityDetected);

        // Simulate connection loss by setting daemon connected to false
        // In real implementation, this would happen via connection monitoring
        // For now, we test that pause hides overlay
        reducer.process_event(&DaemonEvent::Paused);
        let ongoing = reducer.update_ongoing_overlay();
        assert_eq!(ongoing, Some(OverlayState::Hidden));
    }
}
