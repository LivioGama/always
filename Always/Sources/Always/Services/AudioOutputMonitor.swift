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
    /// Last state actually sent, so the poll below only emits on change.
    private var lastPlaying: Bool?
    private var pollTimer: DispatchSourceTimer?

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
        startPolling()
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
        // Force a re-push: after a UDS reconnect the daemon's view is reset,
        // so the change-guard in `notify` must not suppress the first send.
        lastPlaying = nil
        let device = deviceID != kAudioObjectUnknown ? deviceID : defaultOutputDevice()
        guard let device else { return }
        notify(playing: isRunningSomewhere(device: device))
    }

    /// Poll as well as listen.
    ///
    /// The property listener alone was not enough: the daemon received ZERO
    /// `NotifySystemAudioState` commands in a full day of use, and YouTube
    /// audio was transcribed and pasted as if the user had spoken it. Two
    /// reasons the callback can never fire:
    ///
    ///  1. `kAudioDevicePropertyDeviceIsRunningSomewhere` does not flip for
    ///     every producer on every macOS version.
    ///  2. `deviceID` is captured ONCE at startup. Switching output (AirPods,
    ///     headphones, an external display's speakers) leaves the listener
    ///     bound to a device that is no longer the default, so it goes quiet
    ///     permanently.
    ///
    /// A 2s poll that re-resolves the default device each tick fixes both,
    /// costs nothing measurable, and only emits on an actual change.
    private func startPolling() {
        let t = DispatchSource.makeTimerSource(queue: DispatchQueue.global(qos: .utility))
        t.schedule(deadline: .now() + 2, repeating: 2)
        t.setEventHandler { [weak self] in
            guard let self = self else { return }
            guard let device = self.defaultOutputDevice() else { return }
            if device != self.deviceID {
                // Output device changed: rebind the listener to the new one.
                self.deviceID = device
                self.installListener(on: device)
            }
            self.notify(playing: self.isRunningSomewhere(device: device))
        }
        t.resume()
        pollTimer = t
    }

    private func notify(playing: Bool) {
        guard playing != lastPlaying else { return }   // only on change
        lastPlaying = playing
        logger.info("system audio playing=\(playing) — notifying daemon")
        stateMonitor?.sendCommandWithData(
            "NotifySystemAudioState",
            ["playing": playing]
        )
    }
}
