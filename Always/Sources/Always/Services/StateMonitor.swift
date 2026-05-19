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
    @Published var isAutoEnter: Bool = false
    @Published var isTranscribing: Bool = false
    @Published var isVoiceActivity: Bool = false
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

    /// Diagnostic logger — goes only to `os.Logger`. The previous
    /// implementation also wrote `/tmp/statemonitor.log`; that file
    /// is now gone, see UDSClient's `log()` comment for rationale.
    private func log(_ message: String) {
        logger.debug("\(message, privacy: .public)")
    }

    private init() {
        self.udsClient = UDSClient()
        setupUDSEventListener()
        setupOverlaySubscription()
        setupConnectionMonitoring()
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
        guard !respawnInFlight else { return }
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
    func togglePause() {
        let newValue = !isPaused
        isPaused = newValue
        StatusOverlayController.shared.flash(state: newValue ? .paused : .resumed)
        udsClient.sendCommand("TogglePause")
    }

    /// Same as togglePause, for auto-enter.
    ///
    /// Also persists the new value to the daemon's DB so it survives a
    /// daemon restart. The UDS `ToggleAutoEnter` command only mutates
    /// in-memory state — without the `setConfig` round-trip the
    /// preference would reset to the CLI default on every launch.
    func toggleAutoEnter() {
        let newValue = !isAutoEnter
        isAutoEnter = newValue
        StatusOverlayController.shared.flash(state: newValue ? .autoEnterOn : .autoEnterOff)
        udsClient.sendCommand("ToggleAutoEnter")
        Task { [cliService] in
            _ = try? await cliService.setConfig(
                key: "stt_auto_enter",
                value: newValue ? "true" : "false"
            )
        }
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
        udsClient.sendCommandWithData(
            "SetAppPaused",
            Payload(bundle_id: bundleId, paused: paused)
        )
    }
    
    /// Track whether the daemon's most recent ongoing state still
    /// warrants a persistent overlay. Used so a flash doesn't clobber
    /// an in-progress transcription, and so the overlay restores
    /// itself when a flash auto-hides during ongoing activity.
    private func setupOverlaySubscription() {
        Publishers.CombineLatest($isTranscribing, $isVoiceActivity)
            .debounce(for: .milliseconds(50), scheduler: DispatchQueue.main)
            .sink { [weak self] isTranscribing, isVoiceActivity in
                guard let self = self else { return }
                self.log("ongoing state - trans:\(isTranscribing) voice:\(isVoiceActivity)")
                if isTranscribing {
                    StatusOverlayController.shared.show(state: .transcribing)
                } else if isVoiceActivity {
                    StatusOverlayController.shared.show(state: .voiceActivity)
                } else {
                    StatusOverlayController.shared.hide()
                }
            }
            .store(in: &cancellables)
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
        DispatchQueue.main.async {
            switch event.type {
            case .paused:
                let changed = !self.isPaused
                self.isPaused = true
                if changed {
                    StatusOverlayController.shared.flash(state: .paused)
                }
            case .resumed:
                let changed = self.isPaused
                self.isPaused = false
                if changed {
                    StatusOverlayController.shared.flash(state: .resumed)
                }
            case .pausedQuietly:
                // State-only update — no overlay flash. Origin: focus
                // change (mouse window switch, Mission Control) where
                // the user already knows what they did.
                self.isPaused = true
            case .resumedQuietly:
                self.isPaused = false
            case .autoEnterEnabled:
                let changed = !self.isAutoEnter
                self.isAutoEnter = true
                if changed {
                    StatusOverlayController.shared.flash(state: .autoEnterOn)
                }
            case .autoEnterDisabled:
                let changed = self.isAutoEnter
                self.isAutoEnter = false
                if changed {
                    StatusOverlayController.shared.flash(state: .autoEnterOff)
                }
            case .transcribingStarted:
                self.isTranscribing = true
            case .transcribingStopped:
                self.isTranscribing = false
            case .transcriptFinal:
                // The phrase is fully done. Force-clear the ongoing state
                // so the "Listening" overlay disappears immediately —
                // VoiceActivityEnded sometimes lags or is suppressed by
                // residual room noise.
                self.isTranscribing = false
                self.isVoiceActivity = false
            case .voiceActivityDetected:
                self.isVoiceActivity = true
            case .voiceActivityEnded:
                self.isVoiceActivity = false
            case .transcriptionFiltered:
                // Transcription was rejected — clear ongoing state and flash
                // a brief "Filtered" overlay so the user knows the daemon
                // heard them but suppressed the paste.
                self.isTranscribing = false
                self.isVoiceActivity = false
                let reason = event.data?["reason"] ?? ""
                StatusOverlayController.shared.flash(state: .filtered(reason: reason), duration: 1.8)
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
                self.isPaused = true
                StatusOverlayController.shared.showIdleTimeoutAnimation(seconds: secs)
            case .idleAutoResumed:
                self.isPaused = false
                StatusOverlayController.shared.flash(state: .resumed)
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
                self.log("daemon acknowledged focused app: \(event.focusedApp?.bundle_id ?? "nil")")
            case .masterPauseChanged:
                if let v = event.masterPause?.master_paused {
                    self.isMasterPaused = v
                }
            case .resumedAppsChanged:
                if let bundles = event.resumedApps?.bundles {
                    self.resumedBundleIds = Set(bundles)
                }
            default:
                break
            }
        }
    }
}
