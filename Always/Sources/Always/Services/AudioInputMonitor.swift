import Foundation
import CoreAudio
import os.log

/// Watches the default-input audio device and tells the daemon to
/// respawn `rec` when it changes, so the user can switch microphones
/// in macOS System Settings without relaunching Always.
///
/// The daemon's `rec` (SoX) child opens the default input device at
/// spawn time and does not follow a system default-input switch on its
/// own. We listen on the system object for
/// `kAudioHardwarePropertyDefaultInputDevice` (not on a specific
/// device) so a callback fires exactly when the user picks a new mic.
/// On each change we send `RespawnRecorder` over UDS; the daemon kills
/// the old `rec` and spawns a fresh one bound to the new default.
///
/// Unlike `AudioOutputMonitor` there is no state to push on
/// (re)connect — the recorder is already bound to whatever the current
/// default was at daemon start, and the daemon will pick up the live
/// default the next time it respawns for any reason. We only signal
/// *changes* observed while the GUI is running.
final class AudioInputMonitor {
    static let shared = AudioInputMonitor()

    private let logger = Logger(subsystem: "com.always.app", category: "audio-input")
    private var listenerInstalled = false
    private weak var stateMonitor: StateMonitor?

    private init() {}

    /// Attach to `StateMonitor` so we can send the UDS command back
    /// through the shared client (no second connection).
    func start(stateMonitor: StateMonitor) {
        self.stateMonitor = stateMonitor
        installListener()
    }

    /// Install a property listener on the system object for the
    /// default-input selector. We listen on the system object (not a
    /// device) because the *device itself* changes when the user
    /// switches mics — a listener on the old device would never fire.
    private func installListener() {
        guard !listenerInstalled else { return }
        var addr = AudioObjectPropertyAddress(
            mSelector: kAudioHardwarePropertyDefaultInputDevice,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain
        )
        let block: AudioObjectPropertyListenerBlock = { [weak self] _, _ in
            guard let self = self else { return }
            self.logger.info("Default input device changed — notifying daemon to respawn recorder")
            self.stateMonitor?.sendCommand("RespawnRecorder")
        }
        let status = AudioObjectAddPropertyListenerBlock(
            AudioObjectID(kAudioObjectSystemObject), &addr,
            DispatchQueue.global(qos: .utility), block
        )
        if status == noErr {
            listenerInstalled = true
            logger.info("Audio input listener installed on system object")
        } else {
            logger.error("AudioObjectAddPropertyListenerBlock (input) failed: \(status)")
        }
    }
}
