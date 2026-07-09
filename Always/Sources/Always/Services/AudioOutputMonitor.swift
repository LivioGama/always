import Foundation
import CoreAudio
import os.log

/// Watches the default-output audio device and reports start/stop of
/// playback to the daemon as `NotifySystemAudioState { playing }`.
///
/// macOS exposes `kAudioDevicePropertyDeviceIsRunningSomewhere`, which
/// flips to 1 when any process is actively producing sound on that
/// device (Spotify, Zoom, browser, etc.) and back to 0 when everything
/// goes quiet. We listen with a property-change callback rather than
/// poll, so the daemon pause is essentially instantaneous and CPU
/// overhead is zero between events.
final class AudioOutputMonitor {
    static let shared = AudioOutputMonitor()

    private let logger = Logger(subsystem: "com.always.app", category: "audio-output")
    private var deviceID: AudioDeviceID = kAudioObjectUnknown
    private var listenerInstalled = false
    private weak var stateMonitor: StateMonitor?

    private init() {}

    /// Attach to `StateMonitor` so we can send the UDS command back
    /// through the shared client (no second connection).
    func start(stateMonitor: StateMonitor) {
        self.stateMonitor = stateMonitor
        guard let device = defaultOutputDevice() else {
            logger.warning("No default output device — audio monitor inactive")
            return
        }
        self.deviceID = device
        installListener(on: device)
        // Push initial state so daemon's view is correct from t=0.
        notify(playing: isRunningSomewhere(device: device))
    }

    private func defaultOutputDevice() -> AudioDeviceID? {
        var deviceID: AudioDeviceID = kAudioObjectUnknown
        var size = UInt32(MemoryLayout<AudioDeviceID>.size)
        var addr = AudioObjectPropertyAddress(
            mSelector: kAudioHardwarePropertyDefaultOutputDevice,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain
        )
        let status = AudioObjectGetPropertyData(
            AudioObjectID(kAudioObjectSystemObject), &addr, 0, nil, &size, &deviceID
        )
        return status == noErr ? deviceID : nil
    }

    private func isRunningSomewhere(device: AudioDeviceID) -> Bool {
        var running: UInt32 = 0
        var size = UInt32(MemoryLayout<UInt32>.size)
        var addr = AudioObjectPropertyAddress(
            mSelector: kAudioDevicePropertyDeviceIsRunningSomewhere,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain
        )
        let status = AudioObjectGetPropertyData(device, &addr, 0, nil, &size, &running)
        return status == noErr && running != 0
    }

    private func installListener(on device: AudioDeviceID) {
        var addr = AudioObjectPropertyAddress(
            mSelector: kAudioDevicePropertyDeviceIsRunningSomewhere,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain
        )
        let block: AudioObjectPropertyListenerBlock = { [weak self] _, _ in
            guard let self = self else { return }
            let playing = self.isRunningSomewhere(device: device)
            self.notify(playing: playing)
        }
        let status = AudioObjectAddPropertyListenerBlock(
            device, &addr, DispatchQueue.global(qos: .utility), block
        )
        if status == noErr {
            listenerInstalled = true
            logger.info("Audio output listener installed on device \(device)")
        } else {
            logger.error("AudioObjectAddPropertyListenerBlock failed: \(status)")
        }
    }

    /// Re-push output-device playback state after UDS reconnect.
    func resyncToDaemon() {
        let device = deviceID != kAudioObjectUnknown ? deviceID : defaultOutputDevice()
        guard let device else { return }
        notify(playing: isRunningSomewhere(device: device))
    }

    private func notify(playing: Bool) {
        logger.info("system audio playing=\(playing) — notifying daemon")
        stateMonitor?.sendCommandWithData(
            "NotifySystemAudioState",
            ["playing": playing]
        )
    }
}
