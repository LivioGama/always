import Combine
import Foundation
import os.log

/// Owns the Settings → My Voice tab state. Observes daemon UDS events
/// and exposes published enrollment/gate state the SwiftUI panel binds
/// to. Same shape as `ModelManagerClient`: global singleton, subscribes
/// to `Notification.Name.daemonEvent`, sends commands back through
/// `StateMonitor.shared`.
///
/// All recording happens in the DAEMON (it owns the microphone) — this
/// client only drives it: `StartVoiceEnrollment` kicks off a guided
/// sample, `VoiceEnrollmentLevel` events feed the live meter, and
/// `VoiceProfileStatus` snapshots keep the step checklist truthful.
@MainActor
final class VoiceEnrollmentClient: ObservableObject {
    static let shared = VoiceEnrollmentClient()

    private let logger = Logger(subsystem: "com.always.app", category: "voice-enrollment")

    // UserDefaults keys for caching enrollment status
    private enum CacheKeys {
        static let recordedSteps = "voiceEnrollmentRecordedSteps"
        static let isEnrolled = "voiceEnrollmentIsEnrolled"
        static let isEnabled = "voiceEnrollmentIsEnabled"
    }

    /// The guided steps, in display order. Raw values match the wire
    /// format (`EnrollStep` in the daemon).
    enum Step: String, CaseIterable, Identifiable {
        case normal
        case lower
        case louder

        var id: String { rawValue }

        var title: String {
            switch self {
            case .normal: return "Normal voice"
            case .lower: return "Quieter voice"
            case .louder: return "Louder voice"
            }
        }

        var prompt: String {
            switch self {
            case .normal:
                return "Speak the way you normally dictate."
            case .lower:
                return "Speak softly, like dictating in public."
            case .louder:
                return "Speak up, like talking over background noise."
            }
        }
    }

    /// Steps the daemon has a stored sample for.
    @Published var recordedSteps: Set<String> = []
    /// All three steps recorded — the gate can be enabled.
    @Published var isEnrolled: Bool = false
    /// The "only listen to my voice" gate pref, as the daemon knows it.
    @Published var isEnabled: Bool = false
    /// Step currently recording (nil = idle). Set optimistically on
    /// `record(_:)`, confirmed by `VoiceEnrollmentStarted`, cleared on
    /// captured/failed.
    @Published var recordingStep: Step? = nil
    /// Live mic level 0…1-ish while recording (RMS, same scale as the
    /// daemon's energy thresholds — normal speech lands ~0.002–0.05,
    /// so the meter view rescales).
    @Published var level: Double = 0
    /// Voiced progress toward the sample target, 0…1.
    @Published var progress: Double = 0
    /// Terminal error from the last recording attempt, cleared on retry.
    @Published var lastError: String? = nil
    /// True once any status snapshot has arrived (drives the panel's
    /// waiting state).
    @Published var statusReceived: Bool = false

    private var observer: NSObjectProtocol?
    private var cancellables = Set<AnyCancellable>()
    private var statusRequestTime: Date?

    private init() {
        // Load cached status immediately for instant UI display
        loadCachedStatus()

        observer = NotificationCenter.default.addObserver(
            forName: .daemonEvent,
            object: nil,
            queue: .main
        ) { [weak self] note in
            guard let event = note.object as? DaemonEvent else { return }
            MainActor.assumeIsolated {
                self?.handle(event)
            }
        }

        // Re-request the profile snapshot on every (re)connect so a tab
        // opened mid-reconnect never sticks on the waiting state. The
        // initial burst also carries VoiceProfileStatus; this is the
        // belt to that suspender.
        StateMonitor.shared.$isDaemonConnected
            .removeDuplicates()
            .filter { $0 }
            .sink { [weak self] _ in
                self?.requestStatus()
            }
            .store(in: &cancellables)
    }

    deinit {
        if let observer {
            NotificationCenter.default.removeObserver(observer)
        }
    }

    // MARK: Cache management

    private func loadCachedStatus() {
        let defaults = UserDefaults.standard
        if let stepsData = defaults.data(forKey: CacheKeys.recordedSteps),
           let steps = try? JSONDecoder().decode([String].self, from: stepsData) {
            recordedSteps = Set(steps)
        }
        isEnrolled = defaults.bool(forKey: CacheKeys.isEnrolled)
        isEnabled = defaults.bool(forKey: CacheKeys.isEnabled)
        logger.info("Loaded cached voice enrollment status: enrolled=\(self.isEnrolled, privacy: .public), enabled=\(self.isEnabled, privacy: .public), steps=\(self.recordedSteps.count, privacy: .public)")
    }

    private func saveCachedStatus() {
        let defaults = UserDefaults.standard
        if let stepsData = try? JSONEncoder().encode(Array(recordedSteps)) {
            defaults.set(stepsData, forKey: CacheKeys.recordedSteps)
        }
        defaults.set(isEnrolled, forKey: CacheKeys.isEnrolled)
        defaults.set(isEnabled, forKey: CacheKeys.isEnabled)
    }

    // MARK: Commands (UI → daemon)

    func requestStatus() {
        statusRequestTime = Date()
        logger.info("VoiceProfileStatus requested at \(self.statusRequestTime!.timeIntervalSince1970, privacy: .public)")
        StateMonitor.shared.sendCommand("GetVoiceProfileStatus")
    }

    func record(_ step: Step) {
        lastError = nil
        recordingStep = step
        level = 0
        progress = 0
        StateMonitor.shared.sendCommandWithData(
            "StartVoiceEnrollment", ["step": step.rawValue]
        )
    }

    func cancelRecording() {
        StateMonitor.shared.sendCommand("CancelVoiceEnrollment")
        recordingStep = nil
        level = 0
        progress = 0
    }

    func setEnabled(_ enabled: Bool) {
        isEnabled = enabled // optimistic; VoiceProfileStatus confirms
        saveCachedStatus() // cache optimistic update
        StateMonitor.shared.sendCommandWithData(
            "SetVoiceProfileEnabled", ["enabled": enabled]
        )
    }

    func deleteProfile() {
        // Optimistically clear cache
        recordedSteps = []
        isEnrolled = false
        isEnabled = false
        saveCachedStatus()
        StateMonitor.shared.sendCommand("DeleteVoiceProfile")
    }

    // MARK: Event handler (daemon → UI)

    private func handle(_ event: DaemonEvent) {
        switch event.type {
        case .voiceProfileStatus:
            if let status = event.voiceProfileStatus {
                statusReceived = true
                recordedSteps = Set(status.steps)
                isEnrolled = status.enrolled
                isEnabled = status.enabled

                // Save to cache for instant display on next launch
                saveCachedStatus()

                if let requestTime = statusRequestTime {
                    let delayMs = Date().timeIntervalSince(requestTime) * 1000
                    logger.info("VoiceProfileStatus received after \(delayMs, privacy: .public)ms")
                    statusRequestTime = nil
                }
            }
        case .voiceEnrollmentStarted:
            if let raw = event.data?["step"], let step = Step(rawValue: raw) {
                recordingStep = step
                lastError = nil
            }
        case .voiceEnrollmentLevel:
            if let levelData = event.voiceEnrollmentLevel {
                level = levelData.energy
                progress =
                    levelData.target_ms > 0
                    ? min(1.0, Double(levelData.voiced_ms) / Double(levelData.target_ms))
                    : 0
            }
        case .voiceEnrollmentSampleCaptured:
            // Optimistically add the step to recordedSteps
            // The actual list will be confirmed by VoiceProfileStatus
            if let step = recordingStep {
                recordedSteps.insert(step.rawValue)
                saveCachedStatus()
            }
            recordingStep = nil
            level = 0
            progress = 0
        case .voiceEnrollmentFailed:
            recordingStep = nil
            level = 0
            progress = 0
            let message = event.data?["message"] ?? "recording failed"
            // A user-initiated cancel isn't an error worth flashing.
            if message != "cancelled" {
                lastError = message
                logger.warning("voice enrollment failed: \(message, privacy: .public)")
            }
        default:
            break
        }
    }
}
