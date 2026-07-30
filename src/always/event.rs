use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

/// Minimum gap between low-mic overlay hints so a quiet room doesn't
/// spam the HUD on every utterance.
const LOW_MIC_OVERLAY_COOLDOWN: Duration = Duration::from_secs(45);
static LAST_LOW_MIC_OVERLAY: LazyLock<Mutex<Option<Instant>>> = LazyLock::new(|| Mutex::new(None));

/// Wire-format protocol version. Bump on any breaking change to
/// [`DaemonEvent`] or [`DaemonCommand`]. The daemon sends a `Hello` event
/// as the first frame of every UDS connection so GUI clients can refuse
/// to talk to a daemon they were not built against.
///
/// **v3 (2026-05-17):** Pause/Resume now mean "effective" pause (master
/// OR per-app rule). Added [`DaemonEvent::MasterPauseChanged`] and
/// [`DaemonEvent::ResumedAppsChanged`] so the UI can render the
/// allowlist + master kill switch separately.
///
/// **v4 (2026-05-19):** Local-model registry. New
/// [`DaemonCommand`] variants for the Settings → Models tab
/// (`ListModels`, `DownloadModel`, `CancelModelDownload`,
/// `DeleteModel`, `SetActiveTranscriber`) and matching
/// [`DaemonEvent`]s for catalog snapshot + download / verification /
/// extraction progress + active-backend changes.
///
/// **v5 (2026-05-20):** Live preference sync from Settings.
/// `SetAutoEnter` + `ApplyRuntimePreferences` so sensitivity and
/// auto-enter delay apply without a daemon restart.
///
/// **v6 (2026-05-25):** Low microphone volume warning event.
/// `LowMicrophoneVolume` notifies GUI when mic energy is barely above threshold.
///
/// **v9 (2026-06-13):** Pause-chord scope feedback. The pause hotkey is
/// now strictly per-app (master pause moved to its own chord) and the
/// daemon emits [`DaemonEvent::PauseScopeToggled`] so the GUI can flash
/// which scope was toggled.
///
/// **v10 (2026-07-10):** "My Voice" speaker verification. New
/// enrollment commands ([`DaemonCommand::StartVoiceEnrollment`] et al.)
/// and the `VoiceEnrollment*` / [`DaemonEvent::VoiceProfileStatus`]
/// event family driving the Settings → My Voice tab.
pub const PROTOCOL_VERSION: u32 = 10;

/// Event types for daemon-to-GUI communication
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum DaemonEvent {
    /// Sent as the very first frame after a client connects. Carries the
    /// daemon's protocol version. The Mac app rejects the connection if
    /// the version is not the one it was built with.
    Hello {
        version: u32,
    },
    /// Ground truth about the daemon's global-shortcut event tap. The
    /// GUI's own IOHIDCheckAccess reflects the GUI's TCC attribution —
    /// this event reports whether the DAEMON's listen-only CGEventTap is
    /// actually authorized, so the banner can say "shortcuts inactive"
    /// instead of leaving hotkeys silently dead. Included in every
    /// client's initial state burst and re-emitted on change.
    ShortcutListenerStatus {
        input_monitoring_granted: bool,
    },
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
    TranscriptChunk {
        text: String,
    },
    /// Final transcript result
    TranscriptFinal {
        text: String,
    },
    /// Grammar correction was applied — carries before/after for overlay feedback
    GrammarCorrected {
        before: String,
        after: String,
    },
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
    TranscriptionFiltered {
        reason: String,
    },
    /// A `(wrong → right)` correction pair was just applied to
    /// `~/.always/glossary.json` (typically by the user pressing the
    /// correction-capture hotkey or approving a queued candidate).
    /// The GUI uses this to flash a brief toast.
    CorrectionLogged {
        wrong: String,
        right: String,
    },
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
    AutoEnterCountdownStarted {
        remaining_ms: u32,
        total_ms: u32,
    },
    /// Tick of the auto-enter countdown.
    AutoEnterCountdownTick {
        remaining_ms: u32,
    },
    /// Auto-enter countdown cancelled by user (key press) or override.
    AutoEnterCountdownCancelled,
    /// Auto-enter countdown reached zero — Return was just synthesized.
    AutoEnterCountdownFinished,
    /// Daemon auto-paused after going `seconds` with no voice activity.
    IdleAutoPaused {
        seconds: u32,
    },
    /// Daemon auto-resumed after the idle-pause condition cleared.
    IdleAutoResumed,
    /// Focused application changed (macOS only).
    FocusedAppChanged {
        bundle_id: Option<String>,
    },
    /// Master pause flag flipped — the user (or the audio/idle/mic
    /// watchdogs) explicitly toggled the global force-pause switch.
    /// `Paused`/`Resumed` continue to track *effective* state; this
    /// event is what the UI uses to label the global pause toggle
    /// ("Pause globally" vs "Resume globally").
    MasterPauseChanged {
        master_paused: bool,
    },
    /// A pause hotkey fired and the daemon resolved it to a concrete
    /// scope. Lets the GUI flash exactly what was toggled ("Resumed in
    /// Safari" / "Paused everywhere") so the chord never feels like it
    /// did something random. `scope` is `"master"` or `"app"`;
    /// `bundle_id` is set for app scope.
    PauseScopeToggled {
        scope: String,
        bundle_id: Option<String>,
        paused: bool,
    },
    /// Continuous speech crossed the warning threshold — the GUI flashes
    /// a heads-up that the recording hard-caps at `cap_secs`.
    LongRecordingWarning {
        elapsed_secs: u32,
        cap_secs: u32,
    },
    /// A watchdog pause source flipped. `source` is `"mic_conflict"` or
    /// `"audio_output"`; `detail` names the offending app when known
    /// ("Zoom"). Lets the UI say WHY it's paused instead of a generic
    /// badge. Distinct from `MasterPauseChanged`, which is user intent.
    PauseSourceChanged {
        source: String,
        paused: bool,
        detail: Option<String>,
    },
    /// Snapshot of the resumed-app allowlist (bundle ids whose
    /// `paused` override is set to `false`). Sent on connect and
    /// whenever a `SetAppPaused` command mutates the list.
    ResumedAppsChanged {
        bundles: Vec<String>,
    },
    /// Daemon wants the GUI to open the correction dialog. Carries the
    /// most recently-pasted transcript so the dialog can offer a
    /// best-guess match for the wrong word once the user types the
    /// intended one.
    CorrectionDialogRequested {
        last_transcript: String,
    },
    /// Snapshot of the local-model catalog (every entry with its
    /// current `is_downloaded` / `is_downloading` / `partial_size`
    /// fields). Broadcast in response to [`DaemonCommand::ListModels`]
    /// and whenever the catalog mutates so all connected clients see
    /// the same view.
    ModelsList {
        models: Vec<crate::managers::model_registry::ModelInfo>,
    },
    /// Streaming progress for an in-flight download. Throttled to
    /// ~10 events/sec at the registry layer.
    ModelDownloadProgress {
        model_id: String,
        downloaded: u64,
        total: u64,
        percentage: f64,
    },
    ModelDownloadComplete {
        model_id: String,
    },
    ModelDownloadCancelled {
        model_id: String,
    },
    ModelDownloadFailed {
        model_id: String,
        error: String,
    },
    ModelVerificationStarted {
        model_id: String,
    },
    ModelVerificationCompleted {
        model_id: String,
    },
    ModelExtractionStarted {
        model_id: String,
    },
    ModelExtractionCompleted {
        model_id: String,
    },
    ModelExtractionFailed {
        model_id: String,
        error: String,
    },
    /// Microphone volume appears to be too low for reliable detection.
    /// The daemon is detecting voice but at very low energy levels.
    LowMicrophoneVolume {
        energy: f64,
    },
    /// Active STT backend changed (user picked a different model in
    /// Settings → Models, or deleted the currently-active one). The
    /// `backend` field is the canonical wire form — `groq` or
    /// `local:<model_id>`.
    ActiveTranscriberChanged {
        backend: String,
    },
    /// Transcription failed (e.g. Groq API error: bad key, quota, or
    /// network). Surfaced to the GUI as a red error overlay so the user
    /// isn't left on a stuck "Processing…". `kind` is a stable machine tag
    /// (`auth` | `quota` | `network` | `error`); `message` is short human text.
    TranscriptionFailed {
        kind: String,
        message: String,
    },
    /// The Groq circuit breaker opened and the daemon loaded the named
    /// local model to keep transcribing offline. Emitted once per daemon
    /// run (the engine stays loaded afterwards); the GUI flashes a brief
    /// "using local model" notice so degradation is never silent.
    SttFallbackEngaged {
        model: String,
    },
    /// "My Voice" enrollment recording started for `step`
    /// (`normal` | `lower` | `louder`). The daemon is now capturing the
    /// user's guided sample.
    VoiceEnrollmentStarted {
        step: String,
    },
    /// Live level meter + progress during an enrollment recording.
    /// Throttled to ~10 Hz. `voiced_ms` counts actual voiced audio
    /// accumulated toward `target_ms`.
    VoiceEnrollmentLevel {
        energy: f64,
        voiced_ms: u32,
        target_ms: u32,
    },
    /// One enrollment step's sample was captured and folded into the
    /// voiceprint. A `VoiceProfileStatus` follows with the new totals.
    VoiceEnrollmentSampleCaptured {
        step: String,
    },
    /// Enrollment recording failed or was cancelled. `message` is short
    /// human text ("cancelled", "no speech detected", download errors).
    VoiceEnrollmentFailed {
        step: String,
        message: String,
    },
    /// Snapshot of the "My Voice" profile: which steps are recorded,
    /// whether the profile is complete (`enrolled`), and whether the
    /// runtime gate pref is on. Included in the initial state burst and
    /// re-emitted on every mutation.
    VoiceProfileStatus {
        enrolled: bool,
        enabled: bool,
        steps: Vec<String>,
    },
}

/// Upper bound (in chars) on any single user-/network-supplied string
/// field before it is serialized for broadcast. A pathologically long
/// transcript, error string, or model catalog entry would otherwise
/// balloon the line buffer of *every* connected client. Truncating here
/// keeps a single oversized event from degrading the whole UDS fan-out.
const MAX_EVENT_FIELD_CHARS: usize = 8192;

/// Marker appended to a field that was truncated, so a reader can tell
/// the value was elided rather than legitimately ending mid-word.
const ELISION_MARKER: &str = "…[truncated]";

/// Truncate `s` to at most [`MAX_EVENT_FIELD_CHARS`] characters on a char
/// boundary, appending [`ELISION_MARKER`] when truncation occurred.
/// Returns `None` when `s` is already within bounds so callers can skip
/// the allocation for the common case.
fn cap_field(s: &str) -> Option<String> {
    // `chars().count()` is O(n) but only paid for strings that are
    // suspiciously long (the rare case we care about); short strings hit
    // the cheap `len()` early-out below.
    if s.len() <= MAX_EVENT_FIELD_CHARS {
        // `len()` (bytes) <= cap (chars) implies char count <= cap, since
        // each char is >= 1 byte. Cheap, always-correct fast path.
        return None;
    }
    if s.chars().count() <= MAX_EVENT_FIELD_CHARS {
        return None;
    }
    let mut out: String = s.chars().take(MAX_EVENT_FIELD_CHARS).collect();
    out.push_str(ELISION_MARKER);
    Some(out)
}

impl DaemonEvent {
    /// Convert event to JSON line.
    ///
    /// Over-long string fields are capped to `MAX_EVENT_FIELD_CHARS`
    /// before serialization so one giant transcript / error / catalog
    /// entry cannot balloon every connected client's line buffer. The
    /// public enum shape and the serialization format for normal-size
    /// events are unchanged.
    pub fn to_json_line(&self) -> Result<String, serde_json::Error> {
        let json = match self.capped() {
            Some(capped) => serde_json::to_string(&capped)?,
            None => serde_json::to_string(self)?,
        };
        Ok(json + "\n")
    }

    /// Return a size-capped clone of `self` when any oversized string
    /// field needs truncation, or `None` when the event is already within
    /// bounds (the overwhelming common case — no clone, no allocation).
    fn capped(&self) -> Option<DaemonEvent> {
        match self {
            DaemonEvent::TranscriptChunk { text } => {
                cap_field(text).map(|text| DaemonEvent::TranscriptChunk { text })
            }
            DaemonEvent::TranscriptFinal { text } => {
                cap_field(text).map(|text| DaemonEvent::TranscriptFinal { text })
            }
            DaemonEvent::GrammarCorrected { before, after } => {
                let cb = cap_field(before);
                let ca = cap_field(after);
                if cb.is_some() || ca.is_some() {
                    Some(DaemonEvent::GrammarCorrected {
                        before: cb.unwrap_or_else(|| before.clone()),
                        after: ca.unwrap_or_else(|| after.clone()),
                    })
                } else {
                    None
                }
            }
            DaemonEvent::TranscriptionFiltered { reason } => {
                cap_field(reason).map(|reason| DaemonEvent::TranscriptionFiltered { reason })
            }
            DaemonEvent::TranscriptionFailed { kind, message } => {
                cap_field(message).map(|message| DaemonEvent::TranscriptionFailed {
                    kind: kind.clone(),
                    message,
                })
            }
            DaemonEvent::CorrectionDialogRequested { last_transcript } => {
                cap_field(last_transcript).map(|last_transcript| {
                    DaemonEvent::CorrectionDialogRequested { last_transcript }
                })
            }
            DaemonEvent::ModelDownloadFailed { model_id, error } => {
                cap_field(error).map(|error| DaemonEvent::ModelDownloadFailed {
                    model_id: model_id.clone(),
                    error,
                })
            }
            DaemonEvent::ModelExtractionFailed { model_id, error } => {
                cap_field(error).map(|error| DaemonEvent::ModelExtractionFailed {
                    model_id: model_id.clone(),
                    error,
                })
            }
            _ => None,
        }
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
    /// Set auto-enter on/off to an explicit value (Settings toggle).
    SetAutoEnter {
        enabled: bool,
    },
    /// "Consume mode" — route transcription to the daemon's stream consumers
    /// (any UDS client / the transcript-stream file) INSTEAD of the
    /// clipboard+paste+enter path. This is the generic hook that lets an
    /// external controller drive Always for its own use case: while enabled
    /// the daemon transcribes regardless of per-app/master pause (there is no
    /// focused app to paste into) and suppresses all pasting. Cleared when the
    /// last client disconnects so normal dictation resumes. Idempotent.
    SetConsumeMode {
        enabled: bool,
    },
    /// Hot-reload sensitivity + auto-enter delay without restart.
    ApplyRuntimePreferences {
        auto_enter_delay_ms: u32,
        energy_threshold: f64,
        silence_secs: f64,
        cooldown_ms: u32,
        silero_threshold: f32,
        /// Optional so payloads from older GUI builds still decode.
        #[serde(default)]
        adaptive_silence: Option<bool>,
        /// Optional so payloads from older GUI builds still decode.
        #[serde(default)]
        audible_status_sound: Option<String>,
    },
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
    SetPaused {
        paused: bool,
        reason: Option<String>,
    },
    /// Cancel the active auto-enter countdown (e.g. Mac app saw a
    /// keystroke land in the focused app).
    CancelAutoEnterCountdown,
    /// macOS Swift app reports the user switched focused application.
    NotifyFocusedAppChanged {
        bundle_id: Option<String>,
    },
    /// macOS Swift app reports the system audio-output device started
    /// or stopped producing sound. Daemon may auto-pause/resume.
    NotifySystemAudioState {
        playing: bool,
    },
    /// User submitted the correction dialog with the intended spelling.
    /// Daemon diffs against `last_pasted`/`last_transcript`, finds the
    /// closest wrong-word match, and updates the glossary (add entry,
    /// remove over-fired entry, or bump weight).
    LogCorrection {
        intended: String,
    },
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
    /// Settings → Models requested a catalog snapshot. Daemon
    /// responds with [`DaemonEvent::ModelsList`].
    ListModels,
    /// Begin downloading `model_id`. Idempotent — already-downloaded
    /// or in-progress IDs are no-ops. Progress events stream on the
    /// `ModelDownload*` channel until completion or cancel.
    DownloadModel {
        model_id: String,
    },
    /// Cancel an in-flight download. Partial file is kept so the next
    /// `DownloadModel` for the same id resumes.
    CancelModelDownload {
        model_id: String,
    },
    /// Remove a downloaded model from disk.
    DeleteModel {
        model_id: String,
    },
    /// Switch the active STT backend. `backend` is the canonical wire
    /// form parsed by
    /// [`crate::stt_dispatch::TranscriberBackendChoice`] — `groq` or
    /// `local:<model_id>`. Daemon emits
    /// [`DaemonEvent::ActiveTranscriberChanged`] on success.
    SetActiveTranscriber {
        backend: String,
    },
    /// Change the transcription language live. `lang` is an ISO 639-1 code
    /// or "auto". Daemon updates `cfg.lang`, persists it, and rebuilds the
    /// active transcriber so the change takes effect without a restart.
    /// Critical for engines like Canary that bake the language into the
    /// decode prompt (a stale language silently mistranscribes).
    SetLanguage {
        lang: String,
    },
    /// Settings → My Voice: record the guided sample for `step`
    /// (`normal` | `lower` | `louder`). The daemon interrupts idle
    /// listening, records until enough voiced audio accumulates, and
    /// streams `VoiceEnrollment*` progress events.
    StartVoiceEnrollment {
        step: String,
    },
    /// Abort an in-flight (or queued) enrollment recording.
    CancelVoiceEnrollment,
    /// Delete the enrolled voiceprint entirely. The runtime gate
    /// degrades to ungated behavior until re-enrollment.
    DeleteVoiceProfile,
    /// Flip the "only listen to my voice" gate pref. Persisted to the
    /// DB and hot-applied to the running config.
    SetVoiceProfileEnabled {
        enabled: bool,
    },
    /// Client (Settings tab open / reconnect) wants a fresh
    /// [`DaemonEvent::VoiceProfileStatus`] snapshot.
    GetVoiceProfileStatus,
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
    /// True while a `VoiceActivityDetected` has been broadcast without a
    /// matching `VoiceActivityEnded`. Lets every utterance exit path emit
    /// `voice_activity_ended()` unconditionally — the broadcaster swallows
    /// the call when nothing is active, so idle `Silence` cycles don't
    /// flood the UDS clients with redundant terminal events.
    voice_active: Arc<AtomicBool>,
    /// Same transition guard for `TranscribingStarted`/`TranscribingStopped`.
    transcribing_active: Arc<AtomicBool>,
}

impl EventBroadcaster {
    /// Create a new event broadcaster
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(100);
        Self {
            tx,
            voice_active: Arc::new(AtomicBool::new(false)),
            transcribing_active: Arc::new(AtomicBool::new(false)),
        }
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

    /// Send transcribing started event. Always broadcast (re-emits double
    /// as a keep-alive heartbeat for the GUI's stale-state watchdog).
    pub fn transcribing_started(&self) {
        if !self.transcribing_active.swap(true, Ordering::SeqCst) {
            crate::always::status_sound::cue(
                crate::always::status_sound::StatusSound::Transcribing,
            );
        }
        self.send(DaemonEvent::TranscribingStarted);
    }

    /// Send transcribing stopped event — only when transcribing was
    /// announced and not yet stopped (safe to call from any exit path).
    pub fn transcribing_stopped(&self) {
        if self.transcribing_active.swap(false, Ordering::SeqCst) {
            self.send(DaemonEvent::TranscribingStopped);
        }
    }

    /// Send partial transcript chunk (speculative / streaming preview)
    pub fn transcript_chunk(&self, text: String) {
        self.send(DaemonEvent::TranscriptChunk { text });
    }

    /// Send transcript final event
    pub fn transcript_final(&self, text: String) {
        crate::always::status_sound::cue(crate::always::status_sound::StatusSound::Success);
        self.send(DaemonEvent::TranscriptFinal { text });
    }

    /// Notify the UI that async grammar correction replaced the pasted text
    pub fn grammar_corrected(&self, before: String, after: String) {
        self.send(DaemonEvent::GrammarCorrected { before, after });
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

    /// Send voice activity detected event. Always broadcast (re-emits
    /// double as a keep-alive heartbeat for the GUI's stale-state watchdog).
    pub fn voice_activity_detected(&self) {
        if !self.voice_active.swap(true, Ordering::SeqCst) {
            crate::always::status_sound::cue(crate::always::status_sound::StatusSound::Listening);
        }
        self.send(DaemonEvent::VoiceActivityDetected);
    }

    /// Send voice activity ended event — only when voice activity was
    /// announced and not yet ended (safe to call from any exit path).
    pub fn voice_activity_ended(&self) {
        if self.voice_active.swap(false, Ordering::SeqCst) {
            self.send(DaemonEvent::VoiceActivityEnded);
        }
    }

    /// Report whether the daemon's keyboard event tap is authorized
    /// (Input Monitoring). See [`DaemonEvent::ShortcutListenerStatus`].
    pub fn shortcut_listener_status(&self, input_monitoring_granted: bool) {
        self.send(DaemonEvent::ShortcutListenerStatus {
            input_monitoring_granted,
        });
    }

    /// Send transcription-filtered event with the human-readable reason.
    pub fn transcription_filtered(&self, reason: impl Into<String>) {
        self.send(DaemonEvent::TranscriptionFiltered {
            reason: reason.into(),
        });
    }

    /// Send a transcription-failed event (Groq/STT error) so the GUI can
    /// flash a red error overlay instead of leaving a stuck "Processing…".
    pub fn transcription_failed(&self, kind: impl Into<String>, message: impl Into<String>) {
        crate::always::status_sound::cue(crate::always::status_sound::StatusSound::Failure);
        self.send(DaemonEvent::TranscriptionFailed {
            kind: kind.into(),
            message: message.into(),
        });
    }

    /// Announce that the Groq breaker opened and the named local model
    /// took over transcription. See [`DaemonEvent::SttFallbackEngaged`].
    pub fn stt_fallback_engaged(&self, model: impl Into<String>) {
        self.send(DaemonEvent::SttFallbackEngaged {
            model: model.into(),
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

    pub fn low_microphone_volume(&self, energy: f64) {
        self.send(DaemonEvent::LowMicrophoneVolume { energy });
    }

    /// Rate-limited wrapper for [`Self::low_microphone_volume`].
    pub fn low_microphone_volume_maybe(&self, energy: f64) {
        let mut last = LAST_LOW_MIC_OVERLAY.lock();
        if last.is_some_and(|t| t.elapsed() < LOW_MIC_OVERLAY_COOLDOWN) {
            return;
        }
        *last = Some(Instant::now());
        self.low_microphone_volume(energy);
    }

    pub fn master_pause_changed(&self, master_paused: bool) {
        self.send(DaemonEvent::MasterPauseChanged { master_paused });
    }

    /// Announce which scope a pause hotkey just toggled. `bundle_id` is
    /// `Some` for per-app toggles, `None` for the master switch.
    pub fn pause_scope_toggled(&self, scope: &str, bundle_id: Option<String>, paused: bool) {
        self.send(DaemonEvent::PauseScopeToggled {
            scope: scope.to_string(),
            bundle_id,
            paused,
        });
    }

    /// One-shot warning that a continuous recording is approaching the
    /// hard cap (emitted once per utterance by the VAD loop).
    pub fn long_recording_warning(&self, elapsed_secs: u32, cap_secs: u32) {
        self.send(DaemonEvent::LongRecordingWarning {
            elapsed_secs,
            cap_secs,
        });
    }

    pub fn voice_enrollment_started(&self, step: &str) {
        self.send(DaemonEvent::VoiceEnrollmentStarted {
            step: step.to_string(),
        });
    }

    pub fn voice_enrollment_level(&self, energy: f64, voiced_ms: u32, target_ms: u32) {
        self.send(DaemonEvent::VoiceEnrollmentLevel {
            energy,
            voiced_ms,
            target_ms,
        });
    }

    pub fn voice_enrollment_sample_captured(&self, step: &str) {
        self.send(DaemonEvent::VoiceEnrollmentSampleCaptured {
            step: step.to_string(),
        });
    }

    pub fn voice_enrollment_failed(&self, step: &str, message: impl Into<String>) {
        self.send(DaemonEvent::VoiceEnrollmentFailed {
            step: step.to_string(),
            message: message.into(),
        });
    }

    pub fn voice_profile_status(&self, enrolled: bool, enabled: bool, steps: Vec<String>) {
        self.send(DaemonEvent::VoiceProfileStatus {
            enrolled,
            enabled,
            steps,
        });
    }

    /// A watchdog pause source flipped (mic conflict / audio output).
    pub fn pause_source_changed(&self, source: &str, paused: bool, detail: Option<String>) {
        self.send(DaemonEvent::PauseSourceChanged {
            source: source.to_string(),
            paused,
            detail,
        });
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_consume_mode_command_parses_from_iris_wire_format() {
        // The exact JSON Iris writes to the socket. Must decode to the command.
        let on =
            DaemonCommand::from_json_line(r#"{"type":"SetConsumeMode","data":{"enabled":true}}"#)
                .expect("enable must parse");
        assert!(matches!(
            on,
            DaemonCommand::SetConsumeMode { enabled: true }
        ));
        let off =
            DaemonCommand::from_json_line(r#"{"type":"SetConsumeMode","data":{"enabled":false}}"#)
                .expect("disable must parse");
        assert!(matches!(
            off,
            DaemonCommand::SetConsumeMode { enabled: false }
        ));
    }

    fn drain(rx: &mut broadcast::Receiver<DaemonEvent>) -> Vec<DaemonEvent> {
        let mut out = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            out.push(ev);
        }
        out
    }

    #[test]
    fn voice_activity_ended_is_swallowed_when_nothing_active() {
        let b = EventBroadcaster::new();
        let mut rx = b.subscribe();
        // Idle utterance cycles (Silence results) call ended unconditionally —
        // nothing must reach the wire.
        b.voice_activity_ended();
        b.voice_activity_ended();
        assert!(drain(&mut rx).is_empty());
    }

    #[test]
    fn voice_activity_ended_fires_once_per_detected() {
        let b = EventBroadcaster::new();
        let mut rx = b.subscribe();
        b.voice_activity_detected();
        b.voice_activity_ended();
        b.voice_activity_ended(); // duplicate terminal from a second exit path
        let events = drain(&mut rx);
        assert!(matches!(events[0], DaemonEvent::VoiceActivityDetected));
        assert!(matches!(events[1], DaemonEvent::VoiceActivityEnded));
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn voice_activity_detected_reemits_as_heartbeat() {
        let b = EventBroadcaster::new();
        let mut rx = b.subscribe();
        // Lease heartbeat re-emits detected while an utterance is live;
        // every re-emit must reach the GUI to refresh its watchdog.
        b.voice_activity_detected();
        b.voice_activity_detected();
        b.voice_activity_detected();
        b.voice_activity_ended();
        let events = drain(&mut rx);
        assert_eq!(events.len(), 4);
        assert!(matches!(events[3], DaemonEvent::VoiceActivityEnded));
    }

    #[test]
    fn transcribing_stopped_is_transition_guarded() {
        let b = EventBroadcaster::new();
        let mut rx = b.subscribe();
        b.transcribing_stopped(); // never started — swallowed
        b.transcribing_started();
        b.transcribing_stopped();
        b.transcribing_stopped(); // duplicate — swallowed
        let events = drain(&mut rx);
        assert!(matches!(events[0], DaemonEvent::TranscribingStarted));
        assert!(matches!(events[1], DaemonEvent::TranscribingStopped));
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn transcript_chunks_do_not_change_lifecycle_state() {
        let b = EventBroadcaster::new();
        let mut rx = b.subscribe();
        b.transcript_chunk("first rolling chunk".to_string());
        b.transcript_chunk("second rolling chunk".to_string());
        b.transcribing_stopped();
        b.voice_activity_ended();

        let events = drain(&mut rx);
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], DaemonEvent::TranscriptChunk { .. }));
        assert!(matches!(events[1], DaemonEvent::TranscriptChunk { .. }));
    }
}
