// always-capture — microphone recorder for the Always daemon, a drop-in
// replacement for `rec` (SoX): 16 kHz mono signed 16-bit little-endian PCM
// on stdout, diagnostics on stderr, one process for the daemon's lifetime.
//
// What it adds over `rec`: Apple's voice-processing I/O (VPIO) acoustic echo
// cancellation, so audio the Mac itself is playing (a YouTube video, music)
// is subtracted from the microphone signal BEFORE the daemon's VAD, speaker
// gate and speech recognizers ever see it.
//
// Echo cancellation only costs CPU when there is an echo to cancel: while no
// other process on the Mac is playing audio (and no status cue is rendering)
// the voice-processing DSP is BYPASSED, and it is re-engaged the moment any
// process starts output — event-driven via CoreAudio process objects, no
// polling. Measured 2026-10-14 on the user's MacBook Air (60 s windows, same
// room): VPIO active 15.1 CPU-s/min, VPIO bypassed 3.0 CPU-s/min.
//
// Usage: always-capture [--no-vp] [--aec-always] [--agc] [--duck]
//   --no-vp  plain capture (no echo cancellation) — measurement baseline
//   --aec-always  never bypass the echo canceller (pre-2026-10-14 behaviour)
//   --agc    keep VPIO's automatic gain control on (default off: the daemon's
//            energy thresholds are calibrated on un-gained mic levels)
//   --duck   let VPIO duck other apps' audio (default: no ducking — Always must
//            never make the user's video quieter)
// Exits when stdout closes (EPIPE) or the parent process dies.

import AVFoundation
import CoreAudio
import Foundation

signal(SIGPIPE, SIG_IGN)
let args = Set(CommandLine.arguments.dropFirst())
let useVP = !args.contains("--no-vp")
let parentPid = getppid()

func log(_ s: String) {
    // fputs, not FileHandle.write: the latter raises (and abort()s) on EPIPE
    // when the daemon has gone (same crash as Iris's pcm-play/brief-fm).
    _ = "always-capture: \(s)\n".withCString { fputs($0, stderr) }
}

let engine = AVAudioEngine()
let input = engine.inputNode

if useVP {
    do {
        try input.setVoiceProcessingEnabled(true)
    } catch {
        log("voice processing unavailable: \(error) — exiting so the daemon falls back to rec")
        exit(4)
    }
    input.isVoiceProcessingAGCEnabled = args.contains("--agc")
    if #available(macOS 14.0, *) {
        input.voiceProcessingOtherAudioDuckingConfiguration =
            AVAudioVoiceProcessingOtherAudioDuckingConfiguration(
                enableAdvancedDucking: false,
                duckingLevel: args.contains("--duck") ? .default : .min)
    }
}

let inFormat = input.outputFormat(forBus: 0)
log("input format: \(inFormat) vp=\(useVP) presentation_latency_ms=\(Int(input.presentationLatency * 1000))")
guard inFormat.sampleRate > 0, inFormat.channelCount > 0 else {
    log("no input device")
    exit(5)
}
// VPIO on macOS can expose several channels (the processed signal is channel
// 0); mix down only channel 0 rather than averaging in raw mic channels.
let monoIn = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: inFormat.sampleRate,
                           channels: 1, interleaved: false)!
let outFormat = AVAudioFormat(commonFormat: .pcmFormatInt16, sampleRate: 16_000,
                              channels: 1, interleaved: true)!
guard let converter = AVAudioConverter(from: monoIn, to: outFormat) else {
    log("no converter")
    exit(6)
}

let stdoutFd = FileHandle.standardOutput.fileDescriptor
func writeAll(_ ptr: UnsafeRawPointer, _ count: Int) {
    var off = 0
    while off < count {
        let n = write(stdoutFd, ptr + off, count - off)
        if n <= 0 {
            if n < 0 && errno == EINTR { continue }
            exit(0)  // daemon closed the pipe
        }
        off += n
    }
}

// Tap buffers are reused across callbacks (the tap fires ~40×/s; allocating
// two PCM buffers per callback was pure malloc churn). Only the tap thread
// touches them.
var monoBuf: AVAudioPCMBuffer?
var outBuf: AVAudioPCMBuffer?
input.installTap(onBus: 0, bufferSize: 1024, format: inFormat) { buffer, _ in
    let frames = Int(buffer.frameLength)
    guard frames > 0, let src = buffer.floatChannelData?[0] else { return }
    if monoBuf == nil || Int(monoBuf!.frameCapacity) < frames {
        monoBuf = AVAudioPCMBuffer(pcmFormat: monoIn, frameCapacity: AVAudioFrameCount(max(frames, 4096)))
    }
    guard let mono = monoBuf else { return }
    mono.frameLength = AVAudioFrameCount(frames)
    mono.floatChannelData![0].update(from: src, count: frames)
    let cap = AVAudioFrameCount(Double(frames) * 16_000 / inFormat.sampleRate) + 32
    if outBuf == nil || outBuf!.frameCapacity < cap {
        outBuf = AVAudioPCMBuffer(pcmFormat: outFormat, frameCapacity: max(cap, 2048))
    }
    guard let out = outBuf else { return }
    out.frameLength = 0
    var fed = false
    var err: NSError?
    converter.convert(to: out, error: &err) { _, status in
        if fed { status.pointee = .noDataNow; return nil }
        fed = true
        status.pointee = .haveData
        return mono
    }
    if let err { log("convert error: \(err)"); return }
    let n = Int(out.frameLength)
    if n > 0 { writeAll(out.int16ChannelData![0], n * 2) }
}

// ---- Status cues through THIS engine's output -------------------------------
// Audio rendered by the voice-processing unit's own output is never ducked by
// it (it is the "far end" a voice app must hear) and is the echo-cancellation
// reference, so cues neither get ducked nor leak into the mic. The daemon
// sends `play <path>` lines on stdin (2026-10-14: cues played from the daemon
// process were inaudible while this unit ran).
let cueFormat = AVAudioFormat(standardFormatWithSampleRate: 48_000, channels: 2)!
let cuePlayers: [AVAudioPlayerNode] = (0..<6).map { _ in AVAudioPlayerNode() }
for p in cuePlayers {
    engine.attach(p)
    engine.connect(p, to: engine.mainMixerNode, format: cueFormat)
}
var cueCache: [String: AVAudioPCMBuffer] = [:]
var nextCuePlayer = 0
let cueQueue = DispatchQueue(label: "always-capture.cues")

func loadCue(_ path: String) -> AVAudioPCMBuffer? {
    if let b = cueCache[path] { return b }
    guard let file = try? AVAudioFile(forReading: URL(fileURLWithPath: path)),
          let src = AVAudioPCMBuffer(pcmFormat: file.processingFormat, frameCapacity: AVAudioFrameCount(file.length)),
          (try? file.read(into: src)) != nil else { return nil }
    var buf = src
    if src.format != cueFormat {
        guard let conv = AVAudioConverter(from: src.format, to: cueFormat),
              let dst = AVAudioPCMBuffer(pcmFormat: cueFormat,
                  frameCapacity: AVAudioFrameCount(Double(src.frameLength) * 48_000 / src.format.sampleRate) + 64)
        else { return nil }
        var fed = false
        var err: NSError?
        conv.convert(to: dst, error: &err) { _, st in
            if fed { st.pointee = .endOfStream; return nil }
            fed = true; st.pointee = .haveData; return src
        }
        if err != nil { return nil }
        buf = dst
    }
    cueCache[path] = buf
    return buf
}

func playCue(_ path: String) {
    let started = DispatchTime.now().uptimeNanoseconds
    guard let buf = loadCue(path) else { log("cue_failed load \(path)"); return }
    let p = cuePlayers[nextCuePlayer % cuePlayers.count]
    nextCuePlayer += 1
    // A cue rendered while the canceller is bypassed would leak into the mic:
    // engage it for the cue's duration (+ echo tail) first.
    let cueSecs = Double(buf.frameLength) / cueFormat.sampleRate
    DispatchQueue.main.sync { aecHoldFor(seconds: cueSecs + 0.5) }
    p.stop()
    p.scheduleBuffer(buf, at: nil, options: [], completionHandler: nil)
    p.play()
    let ms = Double(DispatchTime.now().uptimeNanoseconds - started) / 1e6
    log(String(format: "cue_played %@ playing=%d ms=%.1f", (path as NSString).lastPathComponent, p.isPlaying ? 1 : 0, ms))
}

/// Dry run (launch smoke test): decode + render the cue OFFLINE through a
/// player node into a buffer — no output device, nothing audible — and log
/// the rendered frame count and peak, proving the file plays.
func dryRenderCue(_ path: String) {
    guard let buf = loadCue(path) else { log("cue_failed load \(path)"); return }
    let off = AVAudioEngine()
    let node = AVAudioPlayerNode()
    off.attach(node)
    off.connect(node, to: off.mainMixerNode, format: cueFormat)
    do {
        try off.enableManualRenderingMode(.offline, format: cueFormat, maximumFrameCount: 4096)
        try off.start()
    } catch { log("cue_failed dry \(error)"); return }
    node.scheduleBuffer(buf, at: nil, options: [], completionHandler: nil)
    node.play()
    let out = AVAudioPCMBuffer(pcmFormat: off.manualRenderingFormat, frameCapacity: 4096)!
    var frames = 0
    var peak: Float = 0
    while frames < Int(buf.frameLength) {
        guard (try? off.renderOffline(4096, to: out)) == .success else { break }
        let n = Int(out.frameLength)
        if let ch = out.floatChannelData?[0] { for i in 0..<n { peak = max(peak, abs(ch[i])) } }
        frames += n
    }
    off.stop()
    log(String(format: "cue_rendered %@ frames=%d peak=%.4f", (path as NSString).lastPathComponent, frames, peak))
}

Thread {
    while let line = readLine(strippingNewline: true) {
        let parts = line.split(separator: " ", maxSplits: 1).map(String.init)
        guard parts.count == 2 else { continue }
        switch parts[0] {
        case "play": cueQueue.async { playCue(parts[1]) }
        case "load": cueQueue.async { _ = loadCue(parts[1]) }
        case "dry": cueQueue.async { dryRenderCue(parts[1]) }
        default: break
        }
    }
    // stdin closed: the daemon is gone or stopped sending cues; capture
    // continues until stdout closes.
}.start()

func applyDucking() {
    guard useVP else { return }
    if #available(macOS 14.0, *) {
        input.voiceProcessingOtherAudioDuckingConfiguration =
            AVAudioVoiceProcessingOtherAudioDuckingConfiguration(
                enableAdvancedDucking: false,
                duckingLevel: args.contains("--duck") ? .default : .min)
        let c = input.voiceProcessingOtherAudioDuckingConfiguration
        log("ducking advanced=\(c.enableAdvancedDucking) level=\(c.duckingLevel.rawValue)")
    }
}

// ---- Echo canceller only while something is playing -------------------------
// All state below is touched on the main queue only (CoreAudio listener blocks
// are delivered there; cue holds hop there).
let aecAlways = args.contains("--aec-always")
let selfPid = getpid()
var aecEngaged = true                 // VPIO starts un-bypassed
var aecHoldUntil = DispatchTime.now() // cue hold
var aecOffWork: DispatchWorkItem?
var aecWatched = Set<AudioObjectID>()
/// Keep the canceller engaged this long after the last output stops: apps
/// stop/start their output between tracks, and the room still rings briefly.
let aecHangover = 2.0
/// After MEDIA output stops (a video or music app, not our own TTS/cues),
/// stay engaged much longer: paused media resumes, and the canceller can
/// only engage in REACTION to output starting — so a resume after bypass
/// put the first seconds of the video into the mic uncancelled, still
/// converging. Measured 2026-10-14: a YouTube resume at 11:25:14Z ~71 s
/// after its pause put the narrator's first 1.5 s through as the user
/// (0.581) and it was pasted, while every narrator window under a converged
/// canceller scored 0.15–0.21. That day's WebKit pause→resume gaps: 30 s,
/// 88 s, 71 s — all inside 120 s. Cost: VPIO stays on (≈15 vs 3 CPU-s/min)
/// for up to 2 min after media pauses. Override:
/// ALWAYS_CAPTURE_MEDIA_HANGOVER (seconds).
let aecMediaHangover = Double(ProcessInfo.processInfo.environment["ALWAYS_CAPTURE_MEDIA_HANGOVER"] ?? "") ?? 120.0
/// Short-lived players whose output ends for good when they exit: the
/// ordinary hangover suffices (Iris's speech player; system sound players).
let aecShortOutputNames: Set<String> = ["pcm-play", "afplay", "always-daemon"]
var aecLastOutputWasMedia = false

func caAddr(_ sel: AudioObjectPropertySelector) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress(mSelector: sel, mScope: kAudioObjectPropertyScopeGlobal,
                               mElement: kAudioObjectPropertyElementMain)
}

@available(macOS 14.2, *)
func audioProcesses() -> [AudioObjectID] {
    var addr = caAddr(kAudioHardwarePropertyProcessObjectList)
    var size: UInt32 = 0
    guard AudioObjectGetPropertyDataSize(AudioObjectID(kAudioObjectSystemObject), &addr, 0, nil, &size) == noErr,
          size > 0 else { return [] }
    var ids = [AudioObjectID](repeating: 0, count: Int(size) / MemoryLayout<AudioObjectID>.size)
    guard AudioObjectGetPropertyData(AudioObjectID(kAudioObjectSystemObject), &addr, 0, nil, &size, &ids) == noErr
    else { return [] }
    return Array(ids.prefix(Int(size) / MemoryLayout<AudioObjectID>.size))
}

func caU32(_ obj: AudioObjectID, _ sel: AudioObjectPropertySelector) -> UInt32? {
    var addr = caAddr(sel)
    var v: UInt32 = 0
    var size = UInt32(MemoryLayout<UInt32>.size)
    return AudioObjectGetPropertyData(obj, &addr, 0, nil, &size, &v) == noErr ? v : nil
}

func caPid(_ obj: AudioObjectID) -> pid_t? {
    var addr = caAddr(kAudioProcessPropertyPID)
    var v: pid_t = 0
    var size = UInt32(MemoryLayout<pid_t>.size)
    return AudioObjectGetPropertyData(obj, &addr, 0, nil, &size, &v) == noErr ? v : nil
}

/// System daemons that keep an output stream running permanently without
/// playing anything audible (measured 2026-10-14: `corespeechd` — Siri's
/// voice trigger — reports running input AND output around the clock).
/// Counting them would pin the canceller on forever.
let aecIgnoredNames: Set<String> = Set(["corespeechd"]).union(
    (ProcessInfo.processInfo.environment["ALWAYS_CAPTURE_AEC_IGNORE"] ?? "")
        .split(separator: ",").map(String.init))  // extra names, for side-by-side tests

func procName(_ pid: pid_t) -> String {
    var buf = [CChar](repeating: 0, count: 256)
    return proc_name(pid, &buf, UInt32(buf.count)) > 0 ? String(cString: buf) : ""
}

/// PIDs of OTHER processes currently running audio output.
@available(macOS 14.2, *)
func otherOutputPids() -> [pid_t] {
    audioProcesses().compactMap { obj in
        guard let pid = caPid(obj), pid != selfPid,
              caU32(obj, kAudioProcessPropertyIsRunningOutput) == 1,
              !aecIgnoredNames.contains(procName(pid)) else { return nil }
        return pid
    }
}

func setAecEngaged(_ on: Bool, _ why: String) {
    guard on != aecEngaged else { return }
    aecEngaged = on
    input.isVoiceProcessingBypassed = !on
    // The tap format must not change with the state (the converter and the
    // 16 kHz mono s16 stream are built once); logged so a test can prove it.
    let f = input.outputFormat(forBus: 0)
    log("aec \(on ? "engaged" : "bypassed") (\(why)) tap=\(f.channelCount)ch@\(Int(f.sampleRate))")
}

/// Main queue only.
func aecReevaluate() {
    guard useVP, !aecAlways else { return }
    guard #available(macOS 14.2, *) else { return }
    let playing = otherOutputPids()
    let held = DispatchTime.now() < aecHoldUntil
    if !playing.isEmpty || held {
        if !playing.isEmpty {
            aecLastOutputWasMedia = playing.contains { !aecShortOutputNames.contains(procName($0)) }
        }
        aecOffWork?.cancel(); aecOffWork = nil
        setAecEngaged(true, held && playing.isEmpty ? "cue" : "output pids \(playing.map { "\($0):\(procName($0))" })")
        if held && playing.isEmpty {
            // Re-check when the hold expires. The item must clear itself
            // first: left set, the `aecOffWork == nil` guard below refused
            // to arm the bypass timer, pinning the canceller on after every
            // cue until some other process happened to play (measured
            // 2026-10-14: engaged 08:55:58 → 09:00:23 after one cue).
            let w = DispatchWorkItem { aecOffWork = nil; aecReevaluate() }
            aecOffWork = w
            DispatchQueue.main.asyncAfter(deadline: aecHoldUntil + 0.05, execute: w)
        }
        return
    }
    guard aecEngaged, aecOffWork == nil else { return }
    let hangover = aecLastOutputWasMedia ? aecMediaHangover : aecHangover
    let w = DispatchWorkItem {
        aecOffWork = nil
        if #available(macOS 14.2, *), otherOutputPids().isEmpty, DispatchTime.now() >= aecHoldUntil {
            aecLastOutputWasMedia = false
            setAecEngaged(false, "no other output for \(Int(hangover))s")
        }
    }
    aecOffWork = w
    DispatchQueue.main.asyncAfter(deadline: .now() + hangover, execute: w)
}

/// Main queue only.
func aecHoldFor(seconds: Double) {
    guard useVP, !aecAlways else { return }
    let until = DispatchTime.now() + seconds
    if until > aecHoldUntil { aecHoldUntil = until }
    aecReevaluate()
}

/// Listen for processes appearing/disappearing and for each one's
/// "running output" flag. Main queue only.
@available(macOS 14.2, *)
func aecWatchProcesses() {
    let procs = audioProcesses()
    aecWatched.formIntersection(procs)
    for obj in procs where !aecWatched.contains(obj) {
        // IsRunningOutput is READ to decide, but its change is NOT
        // notified (measured 2026-10-14, macOS 27: an afplay object
        // registers with out=0, flips to out=1 ~0.6 s later, and a
        // listener on 'piro' never fires — only 'pir?' (IsRunning) and
        // 'pdv#' (Devices) do). Engagement therefore waited for some
        // unrelated process-list change: a YouTube resume was engaged
        // 4.8 s late (11:25:10→14.9Z) and its narrator pasted as the user.
        var ok = false
        for sel in [kAudioProcessPropertyIsRunningOutput, kAudioProcessPropertyIsRunning,
                    kAudioProcessPropertyDevices] {
            var addr = caAddr(sel)
            if AudioObjectAddPropertyListenerBlock(obj, &addr, .main, { _, _ in aecReevaluate() }) == noErr {
                ok = true
            }
        }
        if ok { aecWatched.insert(obj) }
    }
}

func startAecGate() {
    guard useVP else { return }
    if aecAlways { log("aec always engaged (--aec-always)"); return }
    guard #available(macOS 14.2, *) else { log("aec gate unavailable (< macOS 14.2): always engaged"); return }
    var addr = caAddr(kAudioHardwarePropertyProcessObjectList)
    AudioObjectAddPropertyListenerBlock(AudioObjectID(kAudioObjectSystemObject), &addr, .main) { _, _ in
        aecWatchProcesses()
        aecReevaluate()
    }
    aecWatchProcesses()
    log("aec gate on: other output pids now \(otherOutputPids().map { "\($0):\(procName($0))" }) (bypassed after \(aecHangover)s of none)")
    aecReevaluate()
}

do {
    engine.prepare()
    try engine.start()
} catch {
    log("engine start failed: \(error)")
    exit(7)
}
// Re-applied once running: the ducking configuration is only guaranteed to
// take effect on a running voice-processing unit.
applyDucking()
startAecGate()
log("capturing")

// Parent-death watchdog: the daemon may be SIGKILLed with our stdout pipe
// still referenced elsewhere; never outlive it. Kernel exit notification
// (kqueue NOTE_EXIT) instead of a 1 Hz getppid() poll.
let parentWatch = DispatchSource.makeProcessSource(identifier: parentPid, eventMask: .exit, queue: .main)
parentWatch.setEventHandler { exit(0) }
parentWatch.resume()
if getppid() != parentPid { exit(0) }  // parent died before the source armed
NotificationCenter.default.addObserver(
    forName: .AVAudioEngineConfigurationChange, object: engine, queue: nil
) { _ in
    // Device change / route change: exit so the daemon respawns us bound to
    // the new default input (same as its RespawnRecorder path for rec).
    log("engine configuration changed — exiting for respawn")
    exit(0)
}
RunLoop.main.run()
