import Foundation
import Darwin
import Combine
import os.log

// Wire-format protocol version. MUST match `PROTOCOL_VERSION` in
// `src/always/event.rs`. Bumping either side without the other will
// cause the client to refuse the connection.
let UDS_PROTOCOL_VERSION: UInt32 = 8

// Event types matching Rust DaemonEvent enum
enum DaemonEventType: String, Codable {
    case hello = "Hello"
    case listeningStarted = "ListeningStarted"
    case listeningStopped = "ListeningStopped"
    case processingStarted = "ProcessingStarted"
    case processingStopped = "ProcessingStopped"
    case transcribingStarted = "TranscribingStarted"
    case transcribingStopped = "TranscribingStopped"
    case transcriptChunk = "TranscriptChunk"
    case transcriptFinal = "TranscriptFinal"
    case paused = "Paused"
    case resumed = "Resumed"
    // Quiet pair: state-only updates from focus-driven per-app rules.
    // GUI updates `isPaused` but MUST NOT flash the overlay — the user
    // initiated the focus change with their mouse, they don't need a
    // pause/play badge confirming it.
    case pausedQuietly = "PausedQuietly"
    case resumedQuietly = "ResumedQuietly"
    case autoEnterEnabled = "AutoEnterEnabled"
    case autoEnterDisabled = "AutoEnterDisabled"
    case voiceActivityDetected = "VoiceActivityDetected"
    case voiceActivityEnded = "VoiceActivityEnded"
    case transcriptionFiltered = "TranscriptionFiltered"
    case heartbeat = "Heartbeat"
    // Glossary / corrections pipeline. The daemon emits these when it
    // either auto-applies a correction (Logged) or queues a candidate
    // pulled from a recent paste edit (Pending) for the user to confirm.
    case correctionLogged = "CorrectionLogged"
    case correctionPending = "CorrectionPending"
    case correctionCaptureResult = "CorrectionCaptureResult"
    // Auto-enter countdown lifecycle (delay > 0). The Mac app shows
    // a visible countdown overlay between Started and Finished/Cancelled.
    case autoEnterCountdownStarted = "AutoEnterCountdownStarted"
    case autoEnterCountdownTick = "AutoEnterCountdownTick"
    case autoEnterCountdownCancelled = "AutoEnterCountdownCancelled"
    case autoEnterCountdownFinished = "AutoEnterCountdownFinished"
    // Daemon went/came from idle auto-pause.
    case idleAutoPaused = "IdleAutoPaused"
    case idleAutoResumed = "IdleAutoResumed"
    // App focus broadcast from daemon back to us (idempotent echo).
    case focusedAppChanged = "FocusedAppChanged"
    // Master force-pause flag changed. Distinct from Paused/Resumed
    // (which track effective state). UI uses this to label the global
    // toggle.
    case masterPauseChanged = "MasterPauseChanged"
    // Snapshot of bundle ids whose per-app `paused` override is
    // `false` (the user's resumed-app allowlist).
    case resumedAppsChanged = "ResumedAppsChanged"
    // Daemon asks app to show the correction dialog.
    case correctionDialogRequested = "CorrectionDialogRequested"
    // Local-model registry (v4+). The Models tab subscribes; every
    // other view ignores them.
    case modelsList = "ModelsList"
    case modelDownloadProgress = "ModelDownloadProgress"
    case modelDownloadComplete = "ModelDownloadComplete"
    case modelDownloadCancelled = "ModelDownloadCancelled"
    case modelDownloadFailed = "ModelDownloadFailed"
    case modelVerificationStarted = "ModelVerificationStarted"
    case modelVerificationCompleted = "ModelVerificationCompleted"
    case modelExtractionStarted = "ModelExtractionStarted"
    case modelExtractionCompleted = "ModelExtractionCompleted"
    case modelExtractionFailed = "ModelExtractionFailed"
    case activeTranscriberChanged = "ActiveTranscriberChanged"
    /// Speech was heard but energy was too low — mic input volume may need raising.
    case lowMicrophoneVolume = "LowMicrophoneVolume"
    /// Async grammar correction silently replaced the pasted text.
    case grammarCorrected = "GrammarCorrected"
    /// Transcription failed (Groq API error: bad key, quota, or network).
    case transcriptionFailed = "TranscriptionFailed"
    /// Groq circuit breaker opened — daemon switched to the named local
    /// model so dictation keeps working offline.
    case sttFallbackEngaged = "SttFallbackEngaged"
}

// Event data structures
struct TranscriptChunkData: Codable {
    let text: String
}

struct TranscriptFinalData: Codable {
    let text: String
}

struct GrammarCorrectedData: Codable {
    let before: String
    let after: String
}

struct HelloData: Codable {
    let version: UInt32
}

// Sent when the daemon has just applied a known correction (the wrong
// form was already in the glossary) — used purely for UI feedback.
struct CorrectionLoggedData: Codable {
    let wrong: String
    let right: String
}

// Sent when the daemon detected a *new* candidate correction (e.g. user
// edited a freshly-pasted phrase) and wants the user to approve or reject
// it before adding to the persistent glossary. The `id` is what the UI
// echoes back in ApproveCorrection / RejectCorrection.
struct CorrectionPendingData: Codable {
    let id: String
    let wrong: String
    let right: String
}

// Countdown lifecycle payloads. Daemon sends remaining_ms (and total_ms
// on Started); both are unsigned ints so we decode through Int64 to
// be safe with Swift's signed default.
struct AutoEnterCountdownStartData: Codable {
    let remaining_ms: UInt32
    let total_ms: UInt32
}

struct AutoEnterCountdownTickData: Codable {
    let remaining_ms: UInt32
}

struct IdleAutoPausedData: Codable {
    let seconds: UInt32
}

struct FocusedAppChangedData: Codable {
    let bundle_id: String?
}

struct MasterPauseChangedData: Codable {
    let master_paused: Bool
}

struct ResumedAppsChangedData: Codable {
    let bundles: [String]
}

struct CorrectionDialogRequestedData: Codable {
    let last_transcript: String
}

struct LowMicrophoneVolumeData: Codable {
    let energy: Double
}

// Models-tab payloads. Defined in `Models/ModelInfo.swift`:
//   - ModelsListData
//   - ModelDownloadProgressData
//   - ModelIdData            (used for complete/cancelled/verification*/extraction[Started|Completed])
//   - ModelErrorData         (used for download_failed / extraction_failed)
//   - ActiveTranscriberChangedData
// These are decoded by the per-event switch below into typed fields on
// `DaemonEvent`. Keeping the structs in `Models/` rather than here
// avoids growing this 600-line file every time the catalog gains a
// field.

// Main event structure - matches Rust serde tagged enum format
// Rust uses #[serde(tag = "type", content = "data")]
// This produces JSON like: {"type":"ListeningStarted"} or {"type":"TranscriptFinal","data":{"text":"hello"}}
struct DaemonEvent: Codable {
    let type: DaemonEventType

    // Data is nil for events without payloads, or contains the string payload
    // for text-bearing events (TranscriptChunk, TranscriptFinal, …).
    let data: [String: String]?

    /// Populated only for `Hello`. Carries the wire-format protocol
    /// version; the client refuses to talk to a daemon whose version it
    /// was not built against.
    let helloVersion: UInt32?

    /// Populated only for `CorrectionLogged`. We keep this typed
    /// (rather than reusing the loose `[String:String]` blob) so callers
    /// don't have to re-validate keys at every consumption site.
    let correctionLogged: CorrectionLoggedData?

    /// Populated only for `CorrectionPending`. Same rationale as
    /// `correctionLogged`; the `id` field round-trips back to the daemon
    /// when the user approves/rejects.
    let correctionPending: CorrectionPendingData?

    /// Populated for the auto-enter countdown lifecycle events.
    let countdownStart: AutoEnterCountdownStartData?
    let countdownTick: AutoEnterCountdownTickData?
    /// Populated for `IdleAutoPaused`.
    let idleAutoPaused: IdleAutoPausedData?
    /// Populated for `FocusedAppChanged`.
    let focusedApp: FocusedAppChangedData?
    /// Populated for `MasterPauseChanged`.
    let masterPause: MasterPauseChangedData?
    /// Populated for `ResumedAppsChanged`.
    let resumedApps: ResumedAppsChangedData?
    /// Populated for `CorrectionDialogRequested`.
    let correctionDialogRequest: CorrectionDialogRequestedData?

    /// Populated only for `ModelsList`. Carries the full catalog
    /// snapshot the daemon publishes after every mutation.
    let modelsList: ModelsListData?
    /// Populated only for `ModelDownloadProgress`.
    let modelDownloadProgress: ModelDownloadProgressData?
    /// Populated for the family of single-id model events: complete /
    /// cancelled / verification / extraction (start+done).
    let modelId: ModelIdData?
    /// Populated for `ModelDownloadFailed` / `ModelExtractionFailed`.
    let modelError: ModelErrorData?
    /// Populated for `ActiveTranscriberChanged`. `backend` is the
    /// canonical wire form — `groq` or `local:<model_id>`.
    let activeTranscriber: ActiveTranscriberChangedData?
    /// Populated for `LowMicrophoneVolume`.
    let lowMicrophoneVolume: LowMicrophoneVolumeData?
    /// Populated for `GrammarCorrected`.
    let grammarCorrected: GrammarCorrectedData?

    enum CodingKeys: String, CodingKey {
        case type
        case data
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let type = try container.decode(DaemonEventType.self, forKey: .type)
        self.type = type
        // Default-nil everything; concrete cases below set what they need.
        var data: [String: String]? = nil
        var helloVersion: UInt32? = nil
        var correctionLogged: CorrectionLoggedData? = nil
        var correctionPending: CorrectionPendingData? = nil
        var countdownStart: AutoEnterCountdownStartData? = nil
        var countdownTick: AutoEnterCountdownTickData? = nil
        var idleAutoPaused: IdleAutoPausedData? = nil
        var focusedApp: FocusedAppChangedData? = nil
        var masterPause: MasterPauseChangedData? = nil
        var resumedApps: ResumedAppsChangedData? = nil
        var correctionDialogRequest: CorrectionDialogRequestedData? = nil
        var modelsList: ModelsListData? = nil
        var modelDownloadProgress: ModelDownloadProgressData? = nil
        var modelId: ModelIdData? = nil
        var modelError: ModelErrorData? = nil
        var activeTranscriber: ActiveTranscriberChangedData? = nil
        var lowMicrophoneVolume: LowMicrophoneVolumeData? = nil
        var grammarCorrected: GrammarCorrectedData? = nil

        switch type {
        case .hello:
            helloVersion = try container.decodeIfPresent(HelloData.self, forKey: .data)?.version
        case .correctionLogged:
            correctionLogged = try container.decodeIfPresent(CorrectionLoggedData.self, forKey: .data)
        case .correctionPending:
            correctionPending = try container.decodeIfPresent(CorrectionPendingData.self, forKey: .data)
        case .autoEnterCountdownStarted:
            countdownStart = try container.decodeIfPresent(AutoEnterCountdownStartData.self, forKey: .data)
        case .autoEnterCountdownTick:
            countdownTick = try container.decodeIfPresent(AutoEnterCountdownTickData.self, forKey: .data)
        case .idleAutoPaused:
            idleAutoPaused = try container.decodeIfPresent(IdleAutoPausedData.self, forKey: .data)
        case .focusedAppChanged:
            focusedApp = try container.decodeIfPresent(FocusedAppChangedData.self, forKey: .data)
        case .masterPauseChanged:
            masterPause = try container.decodeIfPresent(MasterPauseChangedData.self, forKey: .data)
        case .resumedAppsChanged:
            resumedApps = try container.decodeIfPresent(ResumedAppsChangedData.self, forKey: .data)
        case .correctionDialogRequested:
            correctionDialogRequest = try container.decodeIfPresent(CorrectionDialogRequestedData.self, forKey: .data)
        case .modelsList:
            modelsList = try container.decodeIfPresent(ModelsListData.self, forKey: .data)
        case .modelDownloadProgress:
            modelDownloadProgress = try container.decodeIfPresent(ModelDownloadProgressData.self, forKey: .data)
        case .modelDownloadComplete, .modelDownloadCancelled,
             .modelVerificationStarted, .modelVerificationCompleted,
             .modelExtractionStarted, .modelExtractionCompleted:
            modelId = try container.decodeIfPresent(ModelIdData.self, forKey: .data)
        case .modelDownloadFailed, .modelExtractionFailed:
            modelError = try container.decodeIfPresent(ModelErrorData.self, forKey: .data)
        case .activeTranscriberChanged:
            activeTranscriber = try container.decodeIfPresent(ActiveTranscriberChangedData.self, forKey: .data)
        case .lowMicrophoneVolume:
            lowMicrophoneVolume = try container.decodeIfPresent(LowMicrophoneVolumeData.self, forKey: .data)
        case .grammarCorrected:
            grammarCorrected = try container.decodeIfPresent(GrammarCorrectedData.self, forKey: .data)
        default:
            // Fall-through path: text-bearing or empty events. Keep using
            // the loose dict so existing call sites (e.g. transcript chunk
            // text extraction) continue to work unchanged.
            data = try container.decodeIfPresent([String: String].self, forKey: .data)
        }

        self.data = data
        self.helloVersion = helloVersion
        self.correctionLogged = correctionLogged
        self.correctionPending = correctionPending
        self.countdownStart = countdownStart
        self.countdownTick = countdownTick
        self.idleAutoPaused = idleAutoPaused
        self.focusedApp = focusedApp
        self.masterPause = masterPause
        self.resumedApps = resumedApps
        self.correctionDialogRequest = correctionDialogRequest
        self.modelsList = modelsList
        self.modelDownloadProgress = modelDownloadProgress
        self.modelId = modelId
        self.modelError = modelError
        self.activeTranscriber = activeTranscriber
        self.lowMicrophoneVolume = lowMicrophoneVolume
        self.grammarCorrected = grammarCorrected
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(type, forKey: .type)
        switch type {
        case .hello:
            if let v = helloVersion {
                try container.encode(HelloData(version: v), forKey: .data)
            }
        case .correctionLogged:
            try container.encodeIfPresent(correctionLogged, forKey: .data)
        case .correctionPending:
            try container.encodeIfPresent(correctionPending, forKey: .data)
        case .autoEnterCountdownStarted:
            try container.encodeIfPresent(countdownStart, forKey: .data)
        case .autoEnterCountdownTick:
            try container.encodeIfPresent(countdownTick, forKey: .data)
        case .idleAutoPaused:
            try container.encodeIfPresent(idleAutoPaused, forKey: .data)
        case .focusedAppChanged:
            try container.encodeIfPresent(focusedApp, forKey: .data)
        case .masterPauseChanged:
            try container.encodeIfPresent(masterPause, forKey: .data)
        case .resumedAppsChanged:
            try container.encodeIfPresent(resumedApps, forKey: .data)
        case .correctionDialogRequested:
            try container.encodeIfPresent(correctionDialogRequest, forKey: .data)
        case .modelsList:
            try container.encodeIfPresent(modelsList, forKey: .data)
        case .modelDownloadProgress:
            try container.encodeIfPresent(modelDownloadProgress, forKey: .data)
        case .modelDownloadComplete, .modelDownloadCancelled,
             .modelVerificationStarted, .modelVerificationCompleted,
             .modelExtractionStarted, .modelExtractionCompleted:
            try container.encodeIfPresent(modelId, forKey: .data)
        case .modelDownloadFailed, .modelExtractionFailed:
            try container.encodeIfPresent(modelError, forKey: .data)
        case .activeTranscriberChanged:
            try container.encodeIfPresent(activeTranscriber, forKey: .data)
        case .lowMicrophoneVolume:
            try container.encodeIfPresent(lowMicrophoneVolume, forKey: .data)
        case .grammarCorrected:
            try container.encodeIfPresent(grammarCorrected, forKey: .data)
        default:
            try container.encodeIfPresent(data, forKey: .data)
        }
    }

    init(
        type: DaemonEventType,
        data: [String: String]? = nil,
        helloVersion: UInt32? = nil,
        correctionLogged: CorrectionLoggedData? = nil,
        correctionPending: CorrectionPendingData? = nil,
        countdownStart: AutoEnterCountdownStartData? = nil,
        countdownTick: AutoEnterCountdownTickData? = nil,
        idleAutoPaused: IdleAutoPausedData? = nil,
        focusedApp: FocusedAppChangedData? = nil,
        masterPause: MasterPauseChangedData? = nil,
        resumedApps: ResumedAppsChangedData? = nil,
        correctionDialogRequest: CorrectionDialogRequestedData? = nil,
        modelsList: ModelsListData? = nil,
        modelDownloadProgress: ModelDownloadProgressData? = nil,
        modelId: ModelIdData? = nil,
        modelError: ModelErrorData? = nil,
        activeTranscriber: ActiveTranscriberChangedData? = nil,
        lowMicrophoneVolume: LowMicrophoneVolumeData? = nil,
        grammarCorrected: GrammarCorrectedData? = nil
    ) {
        self.type = type
        self.data = data
        self.helloVersion = helloVersion
        self.correctionLogged = correctionLogged
        self.correctionPending = correctionPending
        self.countdownStart = countdownStart
        self.countdownTick = countdownTick
        self.idleAutoPaused = idleAutoPaused
        self.focusedApp = focusedApp
        self.masterPause = masterPause
        self.resumedApps = resumedApps
        self.correctionDialogRequest = correctionDialogRequest
        self.modelsList = modelsList
        self.modelDownloadProgress = modelDownloadProgress
        self.modelId = modelId
        self.modelError = modelError
        self.activeTranscriber = activeTranscriber
        self.lowMicrophoneVolume = lowMicrophoneVolume
        self.grammarCorrected = grammarCorrected
    }
}

// UDS client for connecting to daemon
class UDSClient: ObservableObject {
    @Published var isConnected: Bool = false
    @Published var connectionError: String? = nil
    /// True when watchdog has detected the daemon may be dead/stalled.
    /// UI can show a "Reconnecting…" indicator.
    @Published var isDegraded: Bool = false

    private let socketPath: String
    private var socketFD: Int32 = -1
    private var queue: DispatchQueue
    private var reconnectScheduled = false
    private var reconnectAttempts: Int = 0
    /// Latch so `onDaemonNeedsRespawn` fires exactly ONCE per outage
    /// instead of on every reconnect past the threshold (3,4,5,…). Reset
    /// only on a successful connect.
    private var respawnRequested = false
    private var receiveSource: DispatchSourceRead?
    private var watchdogTimer: DispatchSourceTimer?
    private var lastEventTime: Date = Date()
    private let logger = Logger(subsystem: "com.always.app", category: "uds-client")

    /// Called when watchdog suspects daemon process is dead AND reconnect
    /// has failed repeatedly. The callback should respawn the daemon
    /// (e.g. via CLIService.startDaemon()).
    var onDaemonNeedsRespawn: (() -> Void)?
    private var isHostQuitting = false
    /// During app launch bootstrap, failed connects are expected — don't flip degraded.
    private var isBootstrapping = false

    // Watchdog tuning
    private let watchdogCheckInterval: TimeInterval = 5.0
    /// If no event (incl. heartbeat) received in this window, treat connection as stalled.
    /// Daemon emits Heartbeat every 5s, so 15s = 3 missed heartbeats.
    private let heartbeatTimeout: TimeInterval = 15.0
    /// After this many failed reconnects in a row, ask caller to respawn daemon.
    /// Lowered from 5 → 3 so stale daemons are killed faster.
    private let maxReconnectAttemptsBeforeRespawn: Int = 3
    /// Reset on any successful decode; incremented on each decode failure.
    /// When it hits `maxConsecutiveDecodeFailures` we force a reconnect so
    /// the Hello handshake re-validates wire-format compatibility.
    private var consecutiveDecodeFailures: Int = 0
    private let maxConsecutiveDecodeFailures: Int = 5
    
    /// Get the default socket path based on the platform
    static func defaultSocketPath() -> String {
        #if os(macOS)
        let home = FileManager.default.homeDirectoryForCurrentUser
        return home
            .appendingPathComponent("Library")
            .appendingPathComponent("Caches")
            .appendingPathComponent("Always")
            .appendingPathComponent("always.sock")
            .path
        #else
        // Linux: Use XDG_RUNTIME_DIR or fallback to /tmp
        if let runtimeDir = ProcessInfo.processInfo.environment["XDG_RUNTIME_DIR"] {
            return "\(runtimeDir)/always.sock"
        } else {
            return "/tmp/always.sock"
        }
        #endif
    }
    
    /// Single funnel for client diagnostics. Goes only to `os.Logger`
    /// — earlier builds dual-wrote to `/tmp/udsclient.log` which we
    /// removed because (a) a writable world-readable temp file in a
    /// signed app is a smell and (b) the file grew unboundedly across
    /// sessions. View live logs with:
    ///     log stream --predicate 'subsystem == "com.always.app"' --info
    private func log(_ message: String) {
        logger.debug("\(message, privacy: .public)")
    }

    init(socketPath: String? = nil, connectOnInit: Bool = false) {
        self.socketPath = socketPath ?? UDSClient.defaultSocketPath()
        self.queue = DispatchQueue(label: "com.always.udsclient")
        logger.info("Initializing with socket path: \(self.socketPath)")
        if connectOnInit {
            connect()
        }
    }
    
    deinit {
        // Synchronous teardown — can't `queue.async { self }` during
        // dealloc (the weak self would already be nil and leak the fd).
        // We're the sole owner at this point, so touching the fd directly
        // is race-free. Cancelling the source runs its cancel handler,
        // which closes the fd; otherwise close the bare fd ourselves.
        if let source = receiveSource {
            source.cancel()
            receiveSource = nil
            socketFD = -1
        } else if socketFD >= 0 {
            close(socketFD)
            socketFD = -1
        }
        watchdogTimer?.cancel()
        watchdogTimer = nil
    }
    
    /// Public entrypoint. The socket fd and its read source are owned
    /// exclusively by the private `queue`, so hop onto it before touching
    /// any of that lifecycle state. Callers may invoke this from any
    /// thread (main, the read source, the watchdog).
    func connect() {
        queue.async { [weak self] in self?.connectOnQueue() }
    }

    /// Runs ONLY on `queue`. All `socketFD` / `receiveSource` /
    /// `receiveBuffer` / `lastEventTime` access is confined here so there
    /// is never concurrent fd mutation across the main queue and the read
    /// queue (the "overlay silently breaks after daemon restart" race).
    private func connectOnQueue() {
        // Queue-confined connection-state gate. `isConnected` is a UI-only
        // `@Published` flag updated async on main, so it can lag the real
        // socket state — use the fd as the source of truth here.
        guard socketFD < 0 else { return }

        log("Attempting to connect to \(self.socketPath)")

        // Use POSIX socket APIs for Unix domain sockets
        let sock = socket(AF_UNIX, SOCK_STREAM, 0)
        guard sock >= 0 else {
            let err = errno
            let errMsg = "Failed to create socket: \(String(cString: strerror(err)))"
            logger.error("\(errMsg)")
            log(errMsg)
            scheduleReconnect()
            return
        }

        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)

        // Copy path into sun_path (max 104 bytes on macOS)
        let pathBytes = socketPath.utf8
        guard pathBytes.count < MemoryLayout.size(ofValue: addr.sun_path) else {
            logger.error("Socket path too long: \(self.socketPath)")
            close(sock)
            scheduleReconnect()
            return
        }

        _ = withUnsafeMutablePointer(to: &addr.sun_path) { pathPtr in
            memcpy(pathPtr, socketPath, pathBytes.count)
        }

        // Connect to socket
        let connectResult = withUnsafeBytes(of: &addr) { addrBytes in
            Darwin.connect(sock, addrBytes.baseAddress!.assumingMemoryBound(to: sockaddr.self), socklen_t(MemoryLayout<sockaddr_un>.size))
        }

        guard connectResult == 0 else {
            let err = errno
            let errMsg = "Failed to connect to socket: \(String(cString: strerror(err)))"
            logger.error("\(errMsg)")
            log(errMsg)
            close(sock)
            scheduleReconnect()
            return
        }

        self.socketFD = sock
        // `lastEventTime` is queue-confined (set here, updated in
        // `handleEvent`, read in the watchdog — all on `queue`).
        self.lastEventTime = Date()
        log("Connected to daemon via Unix socket fd=\(sock)")
        DispatchQueue.main.async {
            // `reconnectAttempts` / `respawnRequested` are written & read
            // by `scheduleReconnect` on main, so reset them there too to
            // keep that backoff/respawn state single-queue.
            self.reconnectAttempts = 0
            self.respawnRequested = false
            self.isConnected = true
            self.isDegraded = false
            self.connectionError = nil
            self.logger.info("Connected to daemon")
        }

        startReceiving()
        startWatchdog()
    }

    /// Public entrypoint. Hops onto `queue` so the fd teardown never races
    /// a concurrent `connect`/`recv`/`send` on another thread.
    func disconnect() {
        queue.async { [weak self] in self?.disconnectOnQueue() }
    }

    /// Runs ONLY on `queue`. Cancels the read source FIRST and lets its
    /// cancel handler `close()` the exact fd it owns — GCD requires the fd
    /// stay open until the cancel handler runs, so we never `close()` here
    /// directly. We just drop our reference and reset the fd to -1.
    private func disconnectOnQueue() {
        if receiveSource != nil {
            // The source's cancel handler owns `close(fd)`.
            receiveSource?.cancel()
            receiveSource = nil
            socketFD = -1
        } else if socketFD >= 0 {
            // No live read source (connect failed before startReceiving, or
            // we never started it) — close the bare fd ourselves.
            close(socketFD)
            socketFD = -1
        }
        stopWatchdog()

        DispatchQueue.main.async {
            self.isConnected = false
        }
    }

    /// GUI is exiting — do not reconnect or request daemon respawn.
    func shutdownForHostQuit() {
        isHostQuitting = true
        onDaemonNeedsRespawn = nil
        reconnectScheduled = false
        disconnect()
    }

    func setBootstrapping(_ bootstrapping: Bool) {
        DispatchQueue.main.async {
            self.isBootstrapping = bootstrapping
            if bootstrapping {
                self.isDegraded = false
            } else if !self.isConnected {
                self.isDegraded = true
            }
        }
    }

    private func scheduleReconnect() {
        DispatchQueue.main.async { [weak self] in
            guard let self = self else { return }
            if self.isHostQuitting { return }
            if self.reconnectScheduled { return }
            self.reconnectScheduled = true
            self.reconnectAttempts += 1
            if !self.isBootstrapping {
                self.isDegraded = true
            }

            // During bootstrap, retry aggressively — the daemon child is still binding UDS.
            let delay: Double
            if self.isBootstrapping {
                delay = self.reconnectAttempts <= 20 ? 0.005 : 0.02
            } else {
                switch self.reconnectAttempts {
                case 1: delay = 0.0
                case 2: delay = 0.01
                case 3: delay = 0.02
                case 4: delay = 0.05
                case 5: delay = 0.1
                case 6: delay = 0.2
                case 7: delay = 0.4
                default: delay = min(30.0, pow(2.0, Double(self.reconnectAttempts - 5)))
                }
            }
            self.log("Scheduling reconnect attempt #\(self.reconnectAttempts) in \(delay)s")

            // During bootstrap the AppDelegate task owns daemon spawn — do
            // not kill/restart a daemon that is still coming up. Latch on
            // `respawnRequested` so we ask for a respawn exactly ONCE per
            // outage instead of on every reconnect cycle past the
            // threshold (which spammed `restartDaemon()` every backoff
            // tick for as long as the daemon stayed down). The latch is
            // cleared only on a successful connect (`connectOnQueue`).
            if self.reconnectAttempts >= self.maxReconnectAttemptsBeforeRespawn,
               !self.isBootstrapping,
               !self.respawnRequested {
                self.respawnRequested = true
                self.log("Reconnect attempts exhausted — requesting daemon respawn")
                self.onDaemonNeedsRespawn?()
            }

            DispatchQueue.main.asyncAfter(deadline: .now() + delay) { [weak self] in
                guard let self = self else { return }
                self.reconnectScheduled = false
                self.connect()
            }
        }
    }

    /// Watchdog: if no event (heartbeat or otherwise) arrives within
    /// `heartbeatTimeout`, the daemon is presumed dead/stalled. Force a
    /// reconnect, which will trigger respawn after enough failures.
    private func startWatchdog() {
        stopWatchdog()
        let timer = DispatchSource.makeTimerSource(queue: queue)
        timer.schedule(deadline: .now() + watchdogCheckInterval,
                       repeating: watchdogCheckInterval)
        timer.setEventHandler { [weak self] in
            guard let self = self else { return }
            let elapsed = Date().timeIntervalSince(self.lastEventTime)
            if elapsed > self.heartbeatTimeout {
                self.log("Watchdog: no event for \(Int(elapsed))s — forcing reconnect")
                DispatchQueue.main.async { self.isDegraded = true }
                self.disconnect()
                self.scheduleReconnect()
            }
        }
        timer.resume()
        watchdogTimer = timer
    }

    private func stopWatchdog() {
        watchdogTimer?.cancel()
        watchdogTimer = nil
    }
    
    /// Accumulates bytes across multiple `recv()` calls. A single daemon
    /// event (e.g. `ModelsList` with ~16 models) easily exceeds a single
    /// kernel-buffer chunk, so JSON arrives split. We hold everything up
    /// to the last `\n` we've seen and parse only complete lines.
    private var receiveBuffer = Data()

    /// Runs ONLY on `queue` (called from `connectOnQueue`). Creates the
    /// read source over the live fd. The fd is captured in a local so the
    /// source's cancel handler closes EXACTLY the fd it owns — GCD requires
    /// the fd stay open until the cancel handler runs, which is why
    /// `disconnectOnQueue` cancels the source instead of `close()`-ing
    /// directly.
    private func startReceiving() {
        guard socketFD >= 0 else {
            log("startReceiving() called but socketFD invalid")
            return
        }
        let fd = socketFD
        log("startReceiving() called with fd=\(fd)")
        logger.debug("startReceiving() called")
        receiveBuffer.removeAll(keepingCapacity: true)

        let source = DispatchSource.makeReadSource(fileDescriptor: fd, queue: queue)
        source.setEventHandler { [weak self] in
            guard let self = self else { return }

            var buffer = [UInt8](repeating: 0, count: 65536)
            // Read from the fd this source owns, not `self.socketFD` (which
            // may already be reset to -1 by a disconnect in flight).
            let bytesRead = recv(fd, &buffer, buffer.count, 0)
            self.log("recv() returned \(bytesRead) bytes")

            if bytesRead < 0 {
                let err = errno
                let errMsg = "Receive error: \(String(cString: strerror(err)))"
                self.logger.error("\(errMsg)")
                self.log(errMsg)
                self.disconnect()
                self.scheduleReconnect()
                return
            }

            if bytesRead == 0 {
                self.log("Connection closed by server")
                self.logger.info("Connection closed by server")
                self.disconnect()
                self.scheduleReconnect()
                return
            }

            self.receiveBuffer.append(buffer, count: bytesRead)
            self.drainCompletedLines()
        }

        source.setCancelHandler { [weak self] in
            // Close the exact fd this source owned — never any reassigned
            // value of `socketFD`. This is the single place the read fd is
            // closed.
            close(fd)
            self?.logger.debug("Receive source cancelled (fd=\(fd) closed)")
        }

        receiveSource = source
        source.resume()
    }

    /// Pull complete `\n`-terminated lines out of `receiveBuffer` and
    /// dispatch each one. Anything after the last `\n` stays in the
    /// buffer for the next `recv()`.
    private func drainCompletedLines() {
        let newline: UInt8 = 0x0A
        guard let lastNewline = receiveBuffer.lastIndex(of: newline) else { return }
        let complete = receiveBuffer.subdata(in: receiveBuffer.startIndex..<(lastNewline + 1))
        receiveBuffer.removeSubrange(receiveBuffer.startIndex...lastNewline)
        if let jsonString = String(data: complete, encoding: .utf8) {
            processMessage(jsonString)
        }
    }

    private func processMessage(_ jsonString: String) {
        // Handle multiple JSON lines in one message.
        let lines = jsonString.components(separatedBy: "\n").filter { !$0.isEmpty }

        for line in lines {
            guard let data = line.data(using: .utf8) else { continue }
            do {
                let event = try JSONDecoder().decode(DaemonEvent.self, from: data)
                consecutiveDecodeFailures = 0
                handleEvent(event)
            } catch {
                // Log the offending payload (truncated) so the daemon-side
                // wire-format drift is diagnosable. Previously we
                // swallowed decode errors entirely and the only symptom
                // was "overlay stuck" / "menu silent" — exactly the
                // class of bug that motivated bumping the UDS protocol
                // version. After enough consecutive failures we force a
                // reconnect so the Hello handshake re-validates.
                let snippet = line.count > 256 ? String(line.prefix(256)) + "…" : line
                logger.error("decode_event_failed: \(error.localizedDescription, privacy: .public) — payload: \(snippet, privacy: .public)")
                consecutiveDecodeFailures += 1
                if consecutiveDecodeFailures >= maxConsecutiveDecodeFailures {
                    logger.error("decode_event_failed: \(self.consecutiveDecodeFailures) consecutive failures, forcing reconnect")
                    disconnect()
                    scheduleReconnect()
                    return
                }
            }
        }
    }
    
    private func handleEvent(_ event: DaemonEvent) {
        // Any event (incl. Heartbeat) proves the daemon is alive.
        lastEventTime = Date()
        logger.debug("handleEvent - type: \(event.type.rawValue, privacy: .public)")

        // Hello carries the wire-format protocol version. If it doesn't
        // match what this build expects, refuse to talk to the daemon —
        // an outdated app and a new daemon will silently disagree on
        // event shapes otherwise.
        if event.type == .hello {
            let actual = event.helloVersion ?? 0
            if actual != UDS_PROTOCOL_VERSION {
                let msg = "UDS protocol mismatch — expected v\(UDS_PROTOCOL_VERSION), daemon sent v\(actual). Disconnecting."
                logger.error("\(msg, privacy: .public)")
                log(msg)
                DispatchQueue.main.async {
                    self.connectionError = msg
                }
                disconnect()
                return
            }
            log("Daemon protocol version v\(actual) accepted")
            return
        }

        // Heartbeat is purely a liveness signal — don't spam NotificationCenter.
        if event.type == .heartbeat { return }

        DispatchQueue.main.async {
            NotificationCenter.default.post(name: .daemonEvent, object: event)
        }
    }

    /// Send a JSON command line to the daemon over the UDS socket.
    /// The daemon executes it in-process and broadcasts the resulting
    /// DaemonEvent back to all connected subscribers.
    func sendCommand(_ commandType: String) {
        // `isConnected` is the UI-facing connection flag; the real fd
        // validity is re-checked on `queue` inside `writeLine` (the fd is
        // queue-confined and must not be read from the caller's thread).
        guard isConnected else {
            self.logger.warning("sendCommand(\(commandType)) skipped — not connected")
            return
        }

        let json = "{\"type\":\"\(commandType)\"}\n"
        writeLine(json, commandType: commandType)
    }

    /// Send a JSON command with a typed `data` payload. The daemon's
    /// command enum uses `#[serde(tag = "type", content = "data")]` so
    /// the wire format is `{"type":"NAME","data":{...}}`. Anything
    /// `Encodable` works as the payload — typically a small dictionary
    /// like `["id": "<uuid>"]`.
    ///
    /// We share `writeLine` with `sendCommand` rather than duplicating
    /// the socket-write/error path so all command-emit paths log
    /// identically and respect the same connection guard.
    func sendCommandWithData<T: Encodable>(_ commandType: String, _ payload: T) {
        // See `sendCommand`: fd validity is re-checked on `queue` in
        // `writeLine`; here we only gate on the UI connection flag.
        guard isConnected else {
            self.logger.warning("sendCommandWithData(\(commandType)) skipped — not connected")
            return
        }

        let envelope = CommandEnvelope(type: commandType, data: payload)
        do {
            let encoder = JSONEncoder()
            // Stable key order helps when grepping logs for replays.
            encoder.outputFormatting = [.sortedKeys]
            let body = try encoder.encode(envelope)
            guard var line = String(data: body, encoding: .utf8) else {
                logger.error("sendCommandWithData(\(commandType)): payload not UTF-8")
                return
            }
            line.append("\n")
            writeLine(line, commandType: commandType)
        } catch {
            logger.error("sendCommandWithData(\(commandType)) encode failed: \(error.localizedDescription)")
        }
    }

    /// Single funnel for socket writes. Logs success/failure consistently
    /// so misrouted commands are easy to spot in the daemon side-channel
    /// log. Hops onto `queue` so the fd is read/written only there — never
    /// concurrently with a connect/disconnect on another thread.
    private func writeLine(_ line: String, commandType: String) {
        guard let data = line.data(using: .utf8) else { return }

        queue.async { [weak self] in
            guard let self = self else { return }
            let fd = self.socketFD
            guard fd >= 0 else {
                self.logger.warning("send(\(commandType)) skipped — fd invalid")
                return
            }

            let bytesWritten = data.withUnsafeBytes { buffer in
                send(fd, buffer.baseAddress!, buffer.count, 0)
            }

            if bytesWritten < 0 {
                let err = errno
                self.logger.error("send(\(commandType)) failed: \(String(cString: strerror(err)))")
            } else {
                self.logger.debug("Sent command: \(commandType)")
            }
        }
    }
}

// Notification name for daemon events
extension Notification.Name {
    static let daemonEvent = Notification.Name("daemonEvent")
}

// File-private envelope mirroring the Rust DaemonCommand enum's
// `#[serde(tag = "type", content = "data")]` shape. Lifted out of
// `sendCommandWithData` because Swift forbids generic types nested in
// generic functions.
private struct CommandEnvelope<P: Encodable>: Encodable {
    let type: String
    let data: P
}
