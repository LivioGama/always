import Foundation
import Combine
import os.log

class StateMonitor: ObservableObject {
    static let shared = StateMonitor()

    @Published var isPaused: Bool = false
    /// User-explicit global pause (the master kill switch). True iff the
    /// last Pause event arrived from a manual toggle, idle auto-pause,
    /// audio output auto-pause, or mic conflict — distinguishable from
    /// an `isPaused` that's just the per-app default for an unlisted
    /// bundle. The UI uses this to label the global toggle ("Resume
    /// globally" vs "Pause globally") and to choose whether the "for
    /// this app" allowlist control makes sense.
    @Published var isMasterPaused: Bool = false
    /// True when the idle watchdog paused listening (distinct from master).
    @Published var isIdleAutoPaused: Bool = false
    @Published var isAutoEnter: Bool = false
    @Published var isTranscribing: Bool = false
    @Published var isVoiceActivity: Bool = false
    /// Sticky between the daemon's `listeningStarted` and `listeningStopped`
    /// (or disconnect). Tracks daemon state only; the HUD is activity-only
    /// and should appear for voice activity or transcription, not idle waiting.
    @Published var isListeningActive: Bool = false
    /// Most recent speculative (streaming) transcript preview. Set when
    /// a TranscriptChunk arrives; cleared when a new utterance starts.
    @Published var partialTranscript: String = ""
    /// Connection state to daemon. UI can show "Reconnecting…" if degraded.
    @Published var isDaemonConnected: Bool = false
    @Published var isDaemonDegraded: Bool = false
    /// Bundle ids whose per-app override sets `paused: false` — i.e.
    /// the user's resumed-app allowlist. Pulled from
    /// `per_app_settings_json` and refreshed whenever the daemon
    /// acknowledges a `SetAppPaused` write.
    @Published var resumedBundleIds: Set<String> = []

    private var cancellables = Set<AnyCancellable>()
    private var udsClient: UDSClient
    private let cliService = CLIService()
    private let logger = Logger(subsystem: "com.always.app", category: "state-monitor")
    private var respawnInFlight = false
    private var isBootstrapping = false
    /// True for ~300ms after receiving a `Hello` event — suppresses
    /// overlay flashes for initial-state events that the daemon sends
    /// immediately after every (re)connection (AutoEnterEnabled, etc.).
    private var isInitialSync = false

    /// Diagnostic logger — goes only to `os.Logger`. The previous
    /// implementation also wrote `/tmp/statemonitor.log`; that file
    /// is now gone, see UDSClient's `log()` comment for rationale.
    private func log(_ message: String) {
        logger.debug("\(message, privacy: .public)")
    }

    private init() {
        self.udsClient = UDSClient(connectOnInit: false)
        setupUDSEventListener()
        setupOverlaySubscription()
        setupConnectionMonitoring()
        setupFocusedAppPauseSync()
        logger.info("Initialized with UDSClient and overlay subscription")

        // Wire daemon respawn: if UDSClient gives up reconnecting, the daemon
        // process is dead — start a fresh one. Debounced so multiple watchdog
        // signals don't pile up subprocesses.
        udsClient.onDaemonNeedsRespawn = { [weak self] in
            self?.respawnDaemonIfNeeded()
        }
    }

    /// Mirror UDSClient connection state into @Published props for the UI.
    private func setupConnectionMonitoring() {
        udsClient.$isConnected
            .receive(on: DispatchQueue.main)
            .sink { [weak self] connected in
                self?.isDaemonConnected = connected
                if !connected {
                    // Daemon went away — drop sticky listening state so the
                    // overlay actually disappears instead of lingering.
                    self?.isListeningActive = false
                    self?.isVoiceActivity = false
                    self?.isTranscribing = false
                }
                if connected {
                    self?.endBootstrap()
                    // Daemon restart clears in-memory focus + pause state.
                    // Re-push what the GUI already knows so listening resumes
                    // without requiring an app switch.
                    FocusedAppMonitor.shared.resyncCurrentAppToDaemon()
                    // Audio state is pushed on property changes; re-pushing on
                    // connect races focus resync and can spuriously master-pause.
                }
            }
            .store(in: &cancellables)
        udsClient.$isDegraded
            .receive(on: DispatchQueue.main)
            .sink { [weak self] degraded in
                self?.isDaemonDegraded = degraded
            }
            .store(in: &cancellables)
    }

    /// Force-restart daemon. Kills any stale process first, then starts
    /// fresh. This is the nuclear option — used when the watchdog has
    /// given up on reconnecting, meaning the old daemon is truly broken.
    private func respawnDaemonIfNeeded() {
        guard !respawnInFlight, !isBootstrapping else { return }
        respawnInFlight = true
        logger.warning("Force-restarting daemon (stale process suspected)")
        Task { [weak self] in
            defer { self?.respawnInFlight = false }
            do {
                _ = try await self?.cliService.restartDaemon()
                self?.logger.info("Daemon force-restart completed")
            } catch {
                self?.logger.error("Daemon force-restart failed: \(error.localizedDescription)")
            }
        }
    }
    
    /// Connect as soon as the UDS socket is listening (retries until live).
    func connectToDaemon() {
        udsClient.connect()
    }

    func beginBootstrap() {
        isBootstrapping = true
        udsClient.setBootstrapping(true)
    }

    func endBootstrap() {
        guard isBootstrapping else { return }
        isBootstrapping = false
        udsClient.setBootstrapping(false)
    }

    /// Stop UDS reconnect/respawn while the GUI is exiting.
    func prepareForQuit() {
        udsClient.shutdownForHostQuit()
        cancellables.removeAll()
    }

    deinit {
        // `removeAll` cancels and drops every subscription atomically.
        // Using `forEach { $0.cancel() }` left the array populated, so a
        // late publisher emit could still reach a stale sink during
        // teardown.
        cancellables.removeAll()
    }

    /// Tell the daemon (in-process) to toggle pause. The daemon mutates its
    /// own state and broadcasts the resulting Paused/Resumed event back to
    /// every subscriber, including us. Going through the daemon — instead
    /// of spawning a CLI subprocess — is what makes the overlay update.
    ///
    /// Updates @Published state and flashes the overlay optimistically so
    /// the UI feels instant. The daemon's echo arrives milliseconds later
    /// and the changed-guard in handleDaemonEvent suppresses the duplicate.
    /// Toggle the **master** pause flag (not per-app allowlist). The daemon
    /// recomputes effective pause; we predict locally so the UI matches.
    func togglePause() {
        let newMaster = !isMasterPaused
        isMasterPaused = newMaster
        if newMaster {
            isPaused = true
            isIdleAutoPaused = false
        } else {
            isIdleAutoPaused = false
            applyLocalEffectivePauseState()
        }
        StatusOverlayController.shared.flash(state: isPaused ? .paused : .resumed)
        udsClient.sendCommand("TogglePause")
    }

    /// Predict `isPaused` from master + idle + focused-app allowlist.
    /// Daemon events remain authoritative; this covers optimistic UI.
    func applyLocalEffectivePauseState() {
        if isMasterPaused || isIdleAutoPaused {
            isPaused = true
            return
        }
        guard let bundle = FocusedAppMonitor.shared.currentBundleId else {
            isPaused = true
            return
        }
        isPaused = !resumedBundleIds.contains(bundle)
    }

    private func setupFocusedAppPauseSync() {
        FocusedAppMonitor.shared.$currentBundleId
            .receive(on: DispatchQueue.main)
            .sink { [weak self] _ in
                self?.applyLocalEffectivePauseState()
            }
            .store(in: &cancellables)
    }

    /// Set auto-enter to an explicit value and persist to DB.
    func setAutoEnter(_ enabled: Bool) {
        guard enabled != isAutoEnter else { return }
        isAutoEnter = enabled
        StatusOverlayController.shared.flash(state: enabled ? .autoEnterOn : .autoEnterOff)
        struct Payload: Encodable { let enabled: Bool }
        udsClient.sendCommandWithData("SetAutoEnter", Payload(enabled: enabled))
        Task { [cliService] in
            _ = try? await cliService.setConfig(
                key: "stt_auto_enter",
                value: enabled ? "true" : "false"
            )
        }
    }

    /// Same as setAutoEnter, toggling from current state (menu shortcuts).
    func toggleAutoEnter() {
        setAutoEnter(!isAutoEnter)
    }

    /// Push sensitivity + auto-enter delay to the running daemon after
    /// Settings writes them to the DB.
    func applyRuntimePreferences(from config: Config) {
        guard isDaemonConnected else { return }
        struct Payload: Encodable {
            let auto_enter_delay_ms: UInt32
            let energy_threshold: Double
            let silence_secs: Double
            let cooldown_ms: UInt32
            let silero_threshold: Float
        }
        let payload = Payload(
            auto_enter_delay_ms: UInt32(max(0, config.autoEnterDelayMs)),
            energy_threshold: config.sttEnergyThreshold,
            silence_secs: config.sttSilence,
            cooldown_ms: UInt32(max(0, config.sttCooldownMs)),
            silero_threshold: config.sileroThreshold
        )
        udsClient.sendCommandWithData("ApplyRuntimePreferences", payload)
    }

    /// Send a parameterless command to the daemon.
    /// Exposed so other services don't need their own UDSClient instance —
    /// only one connection should exist per app process.
    func sendCommand(_ name: String) {
        udsClient.sendCommand(name)
    }

    /// Send a JSON-tagged command with a typed payload. Used for
    /// approve/reject correction flows that carry a UUID.
    func sendCommandWithData<T: Encodable>(_ name: String, _ payload: T) {
        udsClient.sendCommandWithData(name, payload)
    }

    /// Write a `paused` override for the given bundle id. `nil` clears
    /// the override (the app reverts to the default-paused fallback).
    ///
    /// This is how the per-app allowlist is edited from the UI:
    /// "Resume for this app" sends `paused: false`, "Pause for this app"
    /// sends `paused: true` (force-pause this specific app even if the
    /// default were ever flipped), and "Remove from allowlist" sends
    /// `paused: nil`.
    func setAppPaused(bundleId: String, paused: Bool?) {
        struct Payload: Encodable {
            let bundle_id: String
            let paused: Bool?
        }
        if paused == false {
            resumedBundleIds.insert(bundleId)
        } else {
            resumedBundleIds.remove(bundleId)
        }
        if bundleId == FocusedAppMonitor.shared.currentBundleId {
            applyLocalEffectivePauseState()
        }
        udsClient.sendCommandWithData(
            "SetAppPaused",
            Payload(bundle_id: bundleId, paused: paused)
        )
    }
    
    /// Recompute the overlay whenever any state that contributes to it
    /// changes. Pause states (per-app, master, idle) suppress everything
    /// so switching focus to a paused app immediately hides the listening
    /// / transcribing HUD — the user's signal that we stopped listening.
    private func setupOverlaySubscription() {
        let inputs: [AnyPublisher<Void, Never>] = [
            $isTranscribing.map { _ in () }.eraseToAnyPublisher(),
            $isVoiceActivity.map { _ in () }.eraseToAnyPublisher(),
            $isListeningActive.map { _ in () }.eraseToAnyPublisher(),
            $isPaused.map { _ in () }.eraseToAnyPublisher(),
            $isMasterPaused.map { _ in () }.eraseToAnyPublisher(),
            $isIdleAutoPaused.map { _ in () }.eraseToAnyPublisher(),
            $isDaemonConnected.map { _ in () }.eraseToAnyPublisher(),
        ]
        Publishers.MergeMany(inputs)
            .receive(on: DispatchQueue.main)
            .sink { [weak self] _ in self?.updateOverlay() }
            .store(in: &cancellables)
    }

    private func updateOverlay() {
        // Connection lost or any pause-class state active → hide.
        if !isDaemonConnected || isMasterPaused || isPaused || isIdleAutoPaused {
            StatusOverlayController.shared.hide()
            return
        }
        // Activity-only model: the overlay represents something happening
        // (you speaking or the daemon transcribing), not "daemon is alive".
        if isTranscribing {
            StatusOverlayController.shared.show(state: .transcribing)
        } else if isVoiceActivity {
            StatusOverlayController.shared.show(state: .voiceActivity)
        } else {
            StatusOverlayController.shared.hide()
        }
    }

    private func showOngoingOverlayIfNeeded() {
        updateOverlay()
    }

    private func setupUDSEventListener() {
        NotificationCenter.default.publisher(for: .daemonEvent)
            .compactMap { $0.object as? DaemonEvent }
            .sink { [weak self] event in
                self?.log("received daemon event: \(event.type.rawValue)")
                self?.handleDaemonEvent(event)
            }
            .store(in: &cancellables)
    }

    private func handleDaemonEvent(_ event: DaemonEvent) {
        switch event.type {
        case .hello:
            // Daemon sends a batch of current-state events immediately after Hello.
            // Suppress overlay flashes during this ~300ms window so reconnecting
            // doesn't pop "Auto-Enter On" / "Paused" badges at the user.
            isInitialSync = true
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.3) { [weak self] in
                self?.isInitialSync = false
            }
        case .paused:
            let changed = !isPaused
            isPaused = true
            if changed {
                StatusOverlayController.shared.flash(state: .paused)
            }
        case .resumed:
            let changed = isPaused
            isPaused = false
            if changed {
                StatusOverlayController.shared.flash(state: .resumed)
            }
        case .pausedQuietly:
            // State-only update — no overlay flash. Origin: focus
            // change (mouse window switch, Mission Control) where
            // the user already knows what they did.
            isPaused = true
        case .resumedQuietly:
            isPaused = false
        case .autoEnterEnabled:
            let changed = !isAutoEnter
            isAutoEnter = true
            if changed && !isInitialSync {
                StatusOverlayController.shared.flash(state: .autoEnterOn)
            }
        case .autoEnterDisabled:
            let changed = isAutoEnter
            isAutoEnter = false
            if changed && !isInitialSync {
                StatusOverlayController.shared.flash(state: .autoEnterOff)
            }
        case .listeningStarted:
            isListeningActive = true
            showOngoingOverlayIfNeeded()
        case .listeningStopped:
            isListeningActive = false
            isVoiceActivity = false
        case .transcribingStarted:
            isTranscribing = true
            partialTranscript = ""   // reset preview for new utterance
            StatusOverlayController.shared.show(state: .transcribing)
        case .transcribingStopped:
            isTranscribing = false
        case .transcriptChunk:
            // Speculative transcription result — update the streaming preview.
            if let text = event.data?["text"], !text.isEmpty {
                partialTranscript = text
            }
        case .transcriptFinal:
            // The phrase is fully done. Force-clear the ongoing state
            // so the "Listening" overlay disappears immediately —
            // VoiceActivityEnded sometimes lags or is suppressed by
            // residual room noise.
            isTranscribing = false
            isVoiceActivity = false
            partialTranscript = ""
        case .voiceActivityDetected:
            isVoiceActivity = true
            showOngoingOverlayIfNeeded()
            NSLog("Always: VoiceActivityDetected -> overlay")
        case .voiceActivityEnded:
            isVoiceActivity = false
            NSLog("Always: VoiceActivityEnded -> hide overlay")
        case .transcriptionFiltered:
            // Transcription was rejected — clear ongoing state and flash
            // a brief "Filtered" overlay so the user knows the daemon
            // heard them but suppressed the paste.
            isTranscribing = false
            isVoiceActivity = false
            let reason = event.data?["reason"] ?? ""
            StatusOverlayController.shared.flash(state: .filtered(reason: reason), duration: 1.8)
        case .transcriptionFailed:
            // STT/Groq error — clear ongoing state and flash a red error
            // overlay so the user isn't left on a stuck "Processing…".
            isTranscribing = false
            isVoiceActivity = false
            let message = event.data?["message"] ?? "Transcription failed"
            StatusOverlayController.shared.flash(state: .transcriptionFailed(message: message), duration: 3.0)
        case .lowMicrophoneVolume:
            // Microphone volume is too low - flash a warning overlay
            if let energy = event.data?["energy"] as? Double {
                StatusOverlayController.shared.flash(state: .lowMicrophoneVolume(energy: energy), duration: 3.0)
            }
        case .correctionLogged:
            // Per-pair confirmation (⌃⌥X applied a wrong→right
            // substitution to glossary.json). Flash the actual
            // pair text so the user sees what was learned, not
            // just that something happened.
            if let payload = event.correctionLogged {
                StatusOverlayController.shared.flash(
                    state: .correctionSaved(wrong: payload.wrong, right: payload.right),
                    duration: 2.5
                )
            }
        case .grammarCorrected:
            // Async LLM grammar patch replaced the pasted text — flash a
            // brief confirmation so the user knows their text was silently updated.
            StatusOverlayController.shared.flash(state: .grammarCorrected, duration: 1.5)
        case .correctionCaptureResult:
            // Summary outcome of a ⌃⌥X press. The "applied" case
            // is already covered by per-pair `.correctionLogged`
            // overlays above; here we only surface the
            // negative outcomes so the user knows their press
            // registered but produced no change.
            let outcome = event.data?["outcome"] ?? ""
            let label: String
            switch outcome {
            case "applied":
                return  // already handled per-pair
            case "no_recent_paste":
                label = "No recent paste to compare"
            case "no_change":
                label = "Selection matches paste"
            case "no_correction_pairs":
                label = "No clear corrections found"
            case "error":
                label = "Capture failed"
            default:
                label = "Capture: \(outcome)"
            }
            StatusOverlayController.shared.flash(
                state: .correctionEmpty(reason: label),
                duration: 1.8
            )
        case .autoEnterCountdownStarted, .autoEnterCountdownTick:
            let ms = event.countdownStart?.remaining_ms ?? event.countdownTick?.remaining_ms ?? 0
            let seconds = max(0, Int((ms + 999) / 1000))
            // Persistent overlay: replaces flash. Stays visible
            // until Finished/Cancelled clears it.
            StatusOverlayController.shared.show(state: .autoEnterCountdown(secondsRemaining: seconds))
        case .autoEnterCountdownCancelled, .autoEnterCountdownFinished:
            StatusOverlayController.shared.hide()
        case .idleAutoPaused:
            let secs = Int(event.idleAutoPaused?.seconds ?? 0)
            isIdleAutoPaused = true
            isPaused = true
            StatusOverlayController.shared.showIdleTimeoutAnimation(seconds: secs)
        case .idleAutoResumed:
            isIdleAutoPaused = false
            applyLocalEffectivePauseState()
            if !isPaused {
                StatusOverlayController.shared.flash(state: .resumed)
            }
        case .correctionDialogRequested:
            let last = event.correctionDialogRequest?.last_transcript ?? ""
            CorrectionDialog.shared.present(lastTranscript: last) { intended in
                self.udsClient.sendCommandWithData(
                    "LogCorrection",
                    ["intended": intended]
                )
            }
        case .focusedAppChanged:
            // Idempotent echo — daemon confirms it accepted our app
            // bundle id push. No UI action needed; logged in
            // statemonitor.log for debugging.
            log("daemon acknowledged focused app: \(event.focusedApp?.bundle_id ?? "nil")")
        case .masterPauseChanged:
            if let v = event.masterPause?.master_paused {
                isMasterPaused = v
                if v {
                    isPaused = true
                } else {
                    isIdleAutoPaused = false
                    applyLocalEffectivePauseState()
                }
            }
        case .resumedAppsChanged:
            if let bundles = event.resumedApps?.bundles {
                resumedBundleIds = Set(bundles)
                applyLocalEffectivePauseState()
            }
        default:
            break
        }
    }
}
