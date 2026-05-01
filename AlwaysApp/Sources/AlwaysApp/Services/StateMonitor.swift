import Foundation
import Combine

class StateMonitor: ObservableObject {
    static let shared = StateMonitor()

    @Published var isPaused: Bool = false
    @Published var isAutoEnter: Bool = false
    @Published var isTranscribing: Bool = false
    @Published var isVoiceActivity: Bool = false
    private var cancellables = Set<AnyCancellable>()
    private var udsClient: UDSClient

    private func log(_ message: String) {
        let timestamp = Date().description
        let line = "[\(timestamp)] StateMonitor: \(message)\n"
        let path = "/tmp/statemonitor.log"
        if let data = line.data(using: .utf8) {
            if FileManager.default.fileExists(atPath: path) {
                if let fileHandle = FileHandle(forWritingAtPath: path) {
                    fileHandle.seekToEndOfFile()
                    fileHandle.write(data)
                    fileHandle.closeFile()
                }
            } else {
                try? data.write(to: URL(fileURLWithPath: path))
            }
        }
        NSLog("StateMonitor: \(message)")
    }

    private init() {
        self.udsClient = UDSClient()
        setupUDSEventListener()
        setupOverlaySubscription()
        log("Initialized with UDSClient and overlay subscription")
    }
    
    deinit {
        cancellables.forEach { $0.cancel() }
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
    func toggleAutoEnter() {
        let newValue = !isAutoEnter
        isAutoEnter = newValue
        StatusOverlayController.shared.flash(state: newValue ? .autoEnterOn : .autoEnterOff)
        udsClient.sendCommand("ToggleAutoEnter")
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
            default:
                break
            }
        }
    }
}
