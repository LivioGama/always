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

    /// Always's own processes. With echo cancellation the capture helper
    /// runs a voice-processing I/O unit whose OUTPUT side keeps the output
    /// device "running somewhere" even in silence, and cue playback is
    /// ours too — neither is "the user is playing media".
    static let ownProcessNames: Set<String> = [
        "always-capture", "always-daemon", "always-preview-asr", "Always", "always",
    ]
    private var lastNotified: Bool?
    private var pollTimer: DispatchSourceTimer?

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
        // Per-process output state can change while the device-level flag
        // stays 1 (our capture helper holds it), so re-check every second
        // while the device is running.
        let timer = DispatchSource.makeTimerSource(queue: DispatchQueue.global(qos: .utility))
        timer.schedule(deadline: .now() + 1, repeating: 1.0)
        timer.setEventHandler { [weak self] in
            guard let self else { return }
            self.notifyIfChanged(self.isPlaying(device: device))
        }
        timer.resume()
        pollTimer = timer
        // Push initial state so daemon's view is correct from t=0.
        notify(playing: isPlaying(device: device))
    }

    /// "Media is playing": the device runs AND some process other than
    /// Always's own is producing output. Falls back to the device-level
    /// flag when the per-process API is unavailable (macOS < 14).
    private func isPlaying(device: AudioDeviceID) -> Bool {
        guard isRunningSomewhere(device: device) else { return false }
        guard let foreign = foreignOutputRunning() else { return true }
        return foreign
    }

    /// nil = per-process query unsupported.
    private func foreignOutputRunning() -> Bool? {
        var addr = AudioObjectPropertyAddress(
            mSelector: kAudioHardwarePropertyProcessObjectList,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain
        )
        var size: UInt32 = 0
        let system = AudioObjectID(kAudioObjectSystemObject)
        guard AudioObjectGetPropertyDataSize(system, &addr, 0, nil, &size) == noErr, size > 0 else {
            return nil
        }
        var objects = [AudioObjectID](repeating: 0, count: Int(size) / MemoryLayout<AudioObjectID>.size)
        guard AudioObjectGetPropertyData(system, &addr, 0, nil, &size, &objects) == noErr else { return nil }
        for object in objects {
            var running: UInt32 = 0
            var rsize = UInt32(MemoryLayout<UInt32>.size)
            var raddr = AudioObjectPropertyAddress(
                mSelector: kAudioProcessPropertyIsRunningOutput,
                mScope: kAudioObjectPropertyScopeGlobal,
                mElement: kAudioObjectPropertyElementMain
            )
            guard AudioObjectGetPropertyData(object, &raddr, 0, nil, &rsize, &running) == noErr,
                  running != 0 else { continue }
            var pid: pid_t = 0
            var psize = UInt32(MemoryLayout<pid_t>.size)
            var paddr = AudioObjectPropertyAddress(
                mSelector: kAudioProcessPropertyPID,
                mScope: kAudioObjectPropertyScopeGlobal,
                mElement: kAudioObjectPropertyElementMain
            )
            guard AudioObjectGetPropertyData(object, &paddr, 0, nil, &psize, &pid) == noErr else {
                return true
            }
            if !Self.isOwnProcess(pid: pid) { return true }
        }
        return false
    }

    static func isOwnProcess(pid: pid_t) -> Bool {
        if pid == getpid() { return true }
        var buffer = [CChar](repeating: 0, count: 4096)
        guard proc_pidpath(pid, &buffer, UInt32(buffer.count)) > 0 else { return false }
        let name = (String(cString: buffer) as NSString).lastPathComponent
        return ownProcessNames.contains(name)
    }

    private func notifyIfChanged(_ playing: Bool) {
        guard lastNotified != playing else { return }
        notify(playing: playing)
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
            self.notifyIfChanged(self.isPlaying(device: device))
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
        notify(playing: isPlaying(device: device))
    }

    private func notify(playing: Bool) {
        lastNotified = playing
        logger.info("system audio playing=\(playing) — notifying daemon")
        stateMonitor?.sendCommandWithData(
            "NotifySystemAudioState",
            ["playing": playing]
        )
    }
}
