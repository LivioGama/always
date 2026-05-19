use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

/// Wire-format protocol version. Bump on any breaking change to
/// [`DaemonEvent`] or [`DaemonCommand`]. The daemon sends a `Hello` event
/// as the first frame of every UDS connection so GUI clients can refuse
/// to talk to a daemon they were not built against.
///
/// **v3 (2026-05-17):** Pause/Resume now mean "effective" pause (master
/// OR per-app rule). Added [`DaemonEvent::MasterPauseChanged`] and
/// [`DaemonEvent::ResumedAppsChanged`] so the UI can render the
/// allowlist + master kill switch separately.
pub const PROTOCOL_VERSION: u32 = 3;

/// Event types for daemon-to-GUI communication
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum DaemonEvent {
    /// Sent as the very first frame after a client connects. Carries the
    /// daemon's protocol version. The Mac app rejects the connection if
    /// the version is not the one it was built with.
    Hello { version: u32 },
    /// Daemon has started listening for voice input
    ListeningStarted,
    /// Daemon has stopped listening
    ListeningStopped,
    /// Daemon is processing audio (VAD detected speech)
    ProcessingStarted,
    /// Daemon has finished processing audio
    ProcessingStopped,
    /// Transcription has started
    TranscribingStarted,
    /// Transcription has stopped
    TranscribingStopped,
    /// Partial transcript (streaming update)
    TranscriptChunk { text: String },
    /// Final transcript result
    TranscriptFinal { text: String },
    /// Daemon is paused
    Paused,
    /// Daemon is resumed
    Resumed,
    /// Daemon is paused as a side-effect of focus change (per-app rule).
    /// Functionally identical to `Paused` for state tracking, but the GUI
    /// MUST NOT flash the overlay — focus changes that the user initiated
    /// with a mouse / window switcher should not advertise themselves.
    PausedQuietly,
    /// Daemon is resumed as a side-effect of focus change (per-app rule).
    /// See `PausedQuietly` — no overlay flash.
    ResumedQuietly,
    /// Auto-enter mode enabled
    AutoEnterEnabled,
    /// Auto-enter mode disabled
    AutoEnterDisabled,
    /// Voice activity detected (early energy detection)
    VoiceActivityDetected,
    /// Voice activity ended
    VoiceActivityEnded,
    /// Transcription was rejected by the filter or hallucination detector.
    /// Carries a short, human-readable reason so the GUI can display it.
    TranscriptionFiltered { reason: String },
    /// A `(wrong → right)` correction pair was just applied to
    /// `~/.always/glossary.json` (typically by the user pressing the
    /// correction-capture hotkey or approving a queued candidate).
    /// The GUI uses this to flash a brief toast.
    CorrectionLogged { wrong: String, right: String },
    /// A passive clipboard re-copy looked like a correction but was
    /// not auto-applied — it sits in the pending-corrections queue
    /// awaiting user approval. Carries the queue entry's UUID as a
    /// string so the GUI can later send `ApproveCorrection`/`RejectCorrection`.
    CorrectionPending {
        id: String,
        wrong: String,
        right: String,
    },
    /// Outcome of an active manual-correction capture (⌃⌥X). Lets
    /// the GUI flash a status-bar overlay confirming what happened —
    /// `applied`, `no_recent_paste`, `no_change`, `no_correction_pairs`,
    /// or `error`. Per-pair detail flows separately via
    /// [`DaemonEvent::CorrectionLogged`].
    CorrectionCaptureResult {
        outcome: String,
    },
    /// Heartbeat for connection health
    Heartbeat,
    /// Auto-enter delay started — countdown overlay is now active.
    /// `total_ms` is the full delay; `remaining_ms == total_ms` on start.
    AutoEnterCountdownStarted { remaining_ms: u32, total_ms: u32 },
    /// Tick of the auto-enter countdown.
    AutoEnterCountdownTick { remaining_ms: u32 },
    /// Auto-enter countdown cancelled by user (key press) or override.
    AutoEnterCountdownCancelled,
    /// Auto-enter countdown reached zero — Return was just synthesized.
    AutoEnterCountdownFinished,
    /// Daemon auto-paused after going `seconds` with no voice activity.
    IdleAutoPaused { seconds: u32 },
    /// Daemon auto-resumed after the idle-pause condition cleared.
    IdleAutoResumed,
    /// Focused application changed (macOS only).
    FocusedAppChanged { bundle_id: Option<String> },
    /// Master pause flag flipped — the user (or the audio/idle/mic
    /// watchdogs) explicitly toggled the global force-pause switch.
    /// `Paused`/`Resumed` continue to track *effective* state; this
    /// event is what the UI uses to label the global pause toggle
    /// ("Pause globally" vs "Resume globally").
    MasterPauseChanged { master_paused: bool },
    /// Snapshot of the resumed-app allowlist (bundle ids whose
    /// `paused` override is set to `false`). Sent on connect and
    /// whenever a `SetAppPaused` command mutates the list.
    ResumedAppsChanged { bundles: Vec<String> },
    /// Daemon wants the GUI to open the correction dialog. Carries the
    /// most recently-pasted transcript so the dialog can offer a
    /// best-guess match for the wrong word once the user types the
    /// intended one.
    CorrectionDialogRequested { last_transcript: String },
}

impl DaemonEvent {
    /// Convert event to JSON line
    pub fn to_json_line(&self) -> Result<String, serde_json::Error> {
        let json = serde_json::to_string(self)?;
        Ok(json + "\n")
    }

    /// Parse event from JSON line
    pub fn from_json_line(line: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(line)
    }
}

/// Commands that connected clients (the Mac app) can send to the daemon
/// over the UDS socket. Executing them inside the daemon process is what
/// allows the resulting events to reach all subscribers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum DaemonCommand {
    TogglePause,
    ToggleAutoEnter,
    /// Approve a pending correction in the queue and apply it to the
    /// glossary. The daemon emits `CorrectionLogged` on success.
    ApproveCorrection {
        id: String,
    },
    /// Drop a pending correction without applying.
    RejectCorrection {
        id: String,
    },
    /// Manually trigger the active capture path (read user's selection,
    /// diff vs `last_pasted`, apply). Useful for clients without the
    /// global hotkey installed.
    CaptureCorrection,
    /// Explicit pause set/clear with optional reason string (for logs).
    /// Used by the Swift audio-output monitor and per-app overrides.
    SetPaused { paused: bool, reason: Option<String> },
    /// Cancel the active auto-enter countdown (e.g. Mac app saw a
    /// keystroke land in the focused app).
    CancelAutoEnterCountdown,
    /// macOS Swift app reports the user switched focused application.
    NotifyFocusedAppChanged { bundle_id: Option<String> },
    /// macOS Swift app reports the system audio-output device started
    /// or stopped producing sound. Daemon may auto-pause/resume.
    NotifySystemAudioState { playing: bool },
    /// User submitted the correction dialog with the intended spelling.
    /// Daemon diffs against `last_pasted`/`last_transcript`, finds the
    /// closest wrong-word match, and updates the glossary (add entry,
    /// remove over-fired entry, or bump weight).
    LogCorrection { intended: String },
    /// Set (or clear) the per-app `paused` override for `bundle_id`.
    /// `paused = Some(false)` → app is on the resumed-allowlist.
    /// `paused = Some(true)` → app is force-paused even though it
    /// would otherwise inherit the (paused-by-default) global rule.
    /// `paused = None` → remove the override entirely; the app reverts
    /// to the default (paused).
    SetAppPaused {
        bundle_id: String,
        paused: Option<bool>,
    },
}

impl DaemonCommand {
    pub fn from_json_line(line: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(line)
    }
}

/// Event broadcaster for sending events to connected clients
#[derive(Clone)]
pub struct EventBroadcaster {
    tx: broadcast::Sender<DaemonEvent>,
}

impl EventBroadcaster {
    /// Create a new event broadcaster
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(100);
        Self { tx }
    }

    /// Broadcast an event to all subscribers
    pub fn send(&self, event: DaemonEvent) {
        let _ = self.tx.send(event);
    }

    /// Subscribe to events
    pub fn subscribe(&self) -> broadcast::Receiver<DaemonEvent> {
        self.tx.subscribe()
    }

    /// Send listening started event
    pub fn listening_started(&self) {
        self.send(DaemonEvent::ListeningStarted);
    }

    /// Send listening stopped event
    pub fn listening_stopped(&self) {
        self.send(DaemonEvent::ListeningStopped);
    }

    /// Send processing started event
    pub fn processing_started(&self) {
        self.send(DaemonEvent::ProcessingStarted);
    }

    /// Send processing stopped event
    pub fn processing_stopped(&self) {
        self.send(DaemonEvent::ProcessingStopped);
    }

    /// Send transcribing started event
    pub fn transcribing_started(&self) {
        self.send(DaemonEvent::TranscribingStarted);
    }

    /// Send transcribing stopped event
    pub fn transcribing_stopped(&self) {
        self.send(DaemonEvent::TranscribingStopped);
    }

    /// Send transcript final event
    pub fn transcript_final(&self, text: String) {
        self.send(DaemonEvent::TranscriptFinal { text });
    }

    /// Send paused event
    pub fn paused(&self) {
        self.send(DaemonEvent::Paused);
    }

    /// Same as `paused` but tells the GUI to update state silently — no
    /// overlay flash. Used by focus-change-driven per-app pause so a
    /// manual mouse window switch doesn't pop pause/play badges.
    pub fn paused_quietly(&self) {
        self.send(DaemonEvent::PausedQuietly);
    }

    /// Send resumed event
    pub fn resumed(&self) {
        self.send(DaemonEvent::Resumed);
    }

    /// Silent counterpart to `resumed` — see `paused_quietly`.
    pub fn resumed_quietly(&self) {
        self.send(DaemonEvent::ResumedQuietly);
    }

    /// Send auto-enter enabled event
    pub fn auto_enter_enabled(&self) {
        self.send(DaemonEvent::AutoEnterEnabled);
    }

    /// Send auto-enter disabled event
    pub fn auto_enter_disabled(&self) {
        self.send(DaemonEvent::AutoEnterDisabled);
    }

    /// Send voice activity detected event
    pub fn voice_activity_detected(&self) {
        self.send(DaemonEvent::VoiceActivityDetected);
    }

    /// Send voice activity ended event
    pub fn voice_activity_ended(&self) {
        self.send(DaemonEvent::VoiceActivityEnded);
    }

    /// Send transcription-filtered event with the human-readable reason.
    pub fn transcription_filtered(&self, reason: impl Into<String>) {
        self.send(DaemonEvent::TranscriptionFiltered {
            reason: reason.into(),
        });
    }

    /// Send the protocol-version handshake. Must be the first frame on
    /// every new UDS connection.
    pub fn hello(&self) {
        self.send(DaemonEvent::Hello {
            version: PROTOCOL_VERSION,
        });
    }

    /// Send `CorrectionLogged` event (typically from the hotkey-driven
    /// capture path or after an approval).
    pub fn correction_logged(&self, wrong: impl Into<String>, right: impl Into<String>) {
        self.send(DaemonEvent::CorrectionLogged {
            wrong: wrong.into(),
            right: right.into(),
        });
    }

    /// Send `CorrectionPending` event (from passive watcher).
    pub fn correction_pending(
        &self,
        id: impl Into<String>,
        wrong: impl Into<String>,
        right: impl Into<String>,
    ) {
        self.send(DaemonEvent::CorrectionPending {
            id: id.into(),
            wrong: wrong.into(),
            right: right.into(),
        });
    }

    /// Broadcast the outcome of an active correction-capture press
    /// (⌃⌥X). `outcome` is one of: `applied`, `no_recent_paste`,
    /// `no_change`, `no_correction_pairs`, `error`.
    pub fn correction_capture_result(&self, outcome: impl Into<String>) {
        self.send(DaemonEvent::CorrectionCaptureResult {
            outcome: outcome.into(),
        });
    }

    pub fn auto_enter_countdown_started(&self, remaining_ms: u32, total_ms: u32) {
        self.send(DaemonEvent::AutoEnterCountdownStarted {
            remaining_ms,
            total_ms,
        });
    }

    pub fn auto_enter_countdown_tick(&self, remaining_ms: u32) {
        self.send(DaemonEvent::AutoEnterCountdownTick { remaining_ms });
    }

    pub fn auto_enter_countdown_cancelled(&self) {
        self.send(DaemonEvent::AutoEnterCountdownCancelled);
    }

    pub fn auto_enter_countdown_finished(&self) {
        self.send(DaemonEvent::AutoEnterCountdownFinished);
    }

    pub fn idle_auto_paused(&self, seconds: u32) {
        self.send(DaemonEvent::IdleAutoPaused { seconds });
    }

    pub fn idle_auto_resumed(&self) {
        self.send(DaemonEvent::IdleAutoResumed);
    }

    pub fn focused_app_changed(&self, bundle_id: Option<String>) {
        self.send(DaemonEvent::FocusedAppChanged { bundle_id });
    }

    pub fn master_pause_changed(&self, master_paused: bool) {
        self.send(DaemonEvent::MasterPauseChanged { master_paused });
    }

    pub fn resumed_apps_changed(&self, bundles: Vec<String>) {
        self.send(DaemonEvent::ResumedAppsChanged { bundles });
    }

    pub fn correction_dialog_requested(&self, last_transcript: impl Into<String>) {
        self.send(DaemonEvent::CorrectionDialogRequested {
            last_transcript: last_transcript.into(),
        });
    }
}

impl Default for EventBroadcaster {
    fn default() -> Self {
        Self::new()
    }
}

/// Global event broadcaster instance
static GLOBAL_BROADCASTER: std::sync::LazyLock<EventBroadcaster> =
    std::sync::LazyLock::new(EventBroadcaster::new);

/// Get the global event broadcaster
pub fn global_broadcaster() -> &'static EventBroadcaster {
    &GLOBAL_BROADCASTER
}
