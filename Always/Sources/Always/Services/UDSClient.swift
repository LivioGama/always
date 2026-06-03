import Foundation
import Darwin
import Combine
import os.log

// Wire-format protocol version. MUST match `PROTOCOL_VERSION` in
// `src/always/event.rs`. Bumping either side without the other will
// cause the client to refuse the connection.
let UDS_PROTOCOL_VERSION: UInt32 = 5

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
}

// Event data structures
struct TranscriptChunkData: Codable {
    let text: String
}

struct TranscriptFinalData: Codable {
    let text: String
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
        lowMicrophoneVolume: LowMicrophoneVolumeData? = nil
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
    private var receiveSource: DispatchSourceRead?
    private var watchdogTimer: DispatchSourceTimer?
    private var lastEventTime: Date = Date()
    private let logger = Logger(subsystem: "com.always.app", category: "uds-client")

    /// Called when watchdog suspects daemon process is dead AND reconnect
    /// has failed repeatedly. The callback should respawn the daemon
    /// (e.g. via CLIService.startDaemon()).
    var onDaemonNeedsRespawn: (() -> Void)?
    private var isHostQuitting = false

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
        disconnect()
    }
    
    func connect() {
        guard !isConnected else { return }

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
        self.reconnectAttempts = 0
        self.lastEventTime = Date()
        log("Connected to daemon via Unix socket fd=\(sock)")
        DispatchQueue.main.async {
            self.isConnected = true
            self.isDegraded = false
            self.connectionError = nil
            self.logger.info("Connected to daemon")
        }

        startReceiving()
        startWatchdog()
    }
    
    func disconnect() {
        if socketFD >= 0 {
            close(socketFD)
            socketFD = -1
        }
        receiveSource?.cancel()
        receiveSource = nil
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

    private func scheduleReconnect() {
        DispatchQueue.main.async { [weak self] in
            guard let self = self else { return }
            if self.isHostQuitting { return }
            if self.reconnectScheduled { return }
            self.reconnectScheduled = true
            self.reconnectAttempts += 1
            self.isDegraded = true

            // Exponential backoff: 1, 2, 4, 8, 16, max 30 seconds.
            let delay = min(30.0, pow(2.0, Double(self.reconnectAttempts - 1)))
            self.log("Scheduling reconnect attempt #\(self.reconnectAttempts) in \(delay)s")

            // After repeated failures, the daemon process is probably gone.
            // Ask the host (CLIService) to respawn it before retrying again.
            if self.reconnectAttempts >= self.maxReconnectAttemptsBeforeRespawn {
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
    
    private func startReceiving() {
        guard socketFD >= 0 else {
            log("startReceiving() called but socketFD invalid")
            return
        }
        log("startReceiving() called with fd=\(socketFD)")
        logger.debug("startReceiving() called")

        let source = DispatchSource.makeReadSource(fileDescriptor: socketFD, queue: queue)
        source.setEventHandler { [weak self] in
            guard let self = self else { return }

            var buffer = [UInt8](repeating: 0, count: 65536)
            let bytesRead = recv(self.socketFD, &buffer, buffer.count, 0)
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

            let data = Data(bytes: buffer, count: bytesRead)
            if let jsonString = String(data: data, encoding: .utf8) {
                self.log("Received data: \(jsonString)")
                self.logger.debug("Received data: \(jsonString, privacy: .public)")
                self.processMessage(jsonString)
            }
        }

        source.setCancelHandler { [weak self] in
            self?.logger.debug("Receive source cancelled")
        }

        receiveSource = source
        source.resume()
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
        guard socketFD >= 0, isConnected else {
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
        guard socketFD >= 0, isConnected else {
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
    /// log.
    private func writeLine(_ line: String, commandType: String) {
        guard let data = line.data(using: .utf8) else { return }

        let bytesWritten = data.withUnsafeBytes { buffer in
            send(socketFD, buffer.baseAddress!, buffer.count, 0)
        }

        if bytesWritten < 0 {
            let err = errno
            logger.error("send(\(commandType)) failed: \(String(cString: strerror(err)))")
        } else {
            logger.debug("Sent command: \(commandType)")
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
