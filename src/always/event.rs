use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

/// Wire-format protocol version. Bump on any breaking change to
/// [`DaemonEvent`] or [`DaemonCommand`]. The daemon sends a `Hello` event
/// as the first frame of every UDS connection so GUI clients can refuse
/// to talk to a daemon they were not built against.
pub const PROTOCOL_VERSION: u32 = 1;

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

    /// Send resumed event
    pub fn resumed(&self) {
        self.send(DaemonEvent::Resumed);
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
