// In-process status-cue playback for the daemon.
//
// Replaces the old `/usr/bin/afplay` subprocess: spawning afplay cost a
// process launch + file decode per cue (audible startup delay) and the
// one-at-a-time gate dropped any cue that overlapped another. Here every
// cue owns a small pool of AVAudioPlayers built from in-memory AIFF bytes
// (gain already baked in by the Rust side) and primed with prepareToPlay,
// so `play()` is a buffer start, not a decode.
//
// AVAudioPlayer runs on its own audio queue and needs no runloop, so this
// works from the daemon's plain worker threads. No AVAudioEngine is kept
// running: the output device is only active while a cue is actually
// sounding (an always-running engine would trip Always's own
// audio-output pause source).

import AVFoundation
import CoreAudio
import Foundation

private final class CuePools: @unchecked Sendable {
    let lock = NSLock()
    var pools: [Int32: [AVAudioPlayer]] = [:]
    var warmed = false
}

private let cuePools = CuePools()
/// Players per cue: a retrigger while the previous instance still rings
/// takes the next idle player instead of cutting or dropping.
private let poolSize = 3

@_cdecl("always_sound_load")
public func alwaysSoundLoad(_ id: Int32, _ data: UnsafePointer<UInt8>?, _ len: Int32) -> Int32 {
    guard let data, len > 0 else { return 0 }
    let bytes = Data(bytes: data, count: Int(len))
    var players: [AVAudioPlayer] = []
    for _ in 0..<poolSize {
        guard let p = try? AVAudioPlayer(data: bytes) else { return 0 }
        p.volume = 1.0
        p.prepareToPlay()
        players.append(p)
    }
    // Silent warm start, once per process: the very first play() in the
    // process brings up the audio-queue machinery (measured 117 ms for the
    // first cue vs 10-18 ms for later ones). Paying it here, inaudibly, at
    // load time keeps it off the first real cue.
    if !cuePools.warmed, let p = players.first {
        cuePools.warmed = true
        p.volume = 0
        p.play()
        p.stop()
        p.currentTime = 0
        p.volume = 1.0
        p.prepareToPlay()
    }
    cuePools.lock.lock()
    let old = cuePools.pools[id]
    cuePools.pools[id] = players
    cuePools.lock.unlock()
    old?.forEach { $0.stop() }
    return 1
}

@_cdecl("always_sound_play")
public func alwaysSoundPlay(_ id: Int32) -> Int32 {
    cuePools.lock.lock()
    let players = cuePools.pools[id] ?? []
    cuePools.lock.unlock()
    guard !players.isEmpty else { return 0 }
    // Prefer an idle player; if all are sounding, restart the one that is
    // furthest along (oldest instance) rather than dropping the cue.
    let player = players.first(where: { !$0.isPlaying })
        ?? players.max(by: { $0.currentTime < $1.currentTime })!
    if player.isPlaying { player.stop() }
    player.currentTime = 0
    let ok = player.play()
    return ok ? 1 : 0
}

@_cdecl("always_sound_playing_count")
public func alwaysSoundPlayingCount(_ id: Int32) -> Int32 {
    cuePools.lock.lock()
    let players = cuePools.pools[id] ?? []
    cuePools.lock.unlock()
    return Int32(players.filter { $0.isPlaying }.count)
}

@_cdecl("always_sound_output_running")
public func alwaysSoundOutputRunning() -> Int32 {
    var device = AudioObjectID(0)
    var size = UInt32(MemoryLayout<AudioObjectID>.size)
    var addr = AudioObjectPropertyAddress(
        mSelector: kAudioHardwarePropertyDefaultOutputDevice,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain)
    guard AudioObjectGetPropertyData(AudioObjectID(kAudioObjectSystemObject), &addr, 0, nil, &size, &device) == noErr
    else { return -1 }
    var running = UInt32(0)
    size = UInt32(MemoryLayout<UInt32>.size)
    addr.mSelector = kAudioDevicePropertyDeviceIsRunningSomewhere
    guard AudioObjectGetPropertyData(device, &addr, 0, nil, &size, &running) == noErr else { return -1 }
    return running != 0 ? 1 : 0
}
