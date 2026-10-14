// always-capture — microphone recorder for the Always daemon, a drop-in
// replacement for `rec` (SoX): 16 kHz mono signed 16-bit little-endian PCM
// on stdout, diagnostics on stderr, one process for the daemon's lifetime.
//
// What it adds over `rec`: Apple's voice-processing I/O (VPIO) acoustic echo
// cancellation, so audio the Mac itself is playing (a YouTube video, music)
// is subtracted from the microphone signal BEFORE the daemon's VAD, speaker
// gate and speech recognizers ever see it.
//
// Usage: always-capture [--no-vp] [--agc] [--duck]
//   --no-vp  plain capture (no echo cancellation) — measurement baseline
//   --agc    keep VPIO's automatic gain control on (default off: the daemon's
//            energy thresholds are calibrated on un-gained mic levels)
//   --duck   let VPIO duck other apps' audio (default: no ducking — Always must
//            never make the user's video quieter)
// Exits when stdout closes (EPIPE) or the parent process dies.

import AVFoundation
import Foundation

signal(SIGPIPE, SIG_IGN)
let args = Set(CommandLine.arguments.dropFirst())
let useVP = !args.contains("--no-vp")
let parentPid = getppid()

func log(_ s: String) {
    FileHandle.standardError.write("always-capture: \(s)\n".data(using: .utf8)!)
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

input.installTap(onBus: 0, bufferSize: 1024, format: inFormat) { buffer, _ in
    let frames = Int(buffer.frameLength)
    guard frames > 0, let src = buffer.floatChannelData?[0],
          let mono = AVAudioPCMBuffer(pcmFormat: monoIn, frameCapacity: AVAudioFrameCount(frames))
    else { return }
    mono.frameLength = AVAudioFrameCount(frames)
    mono.floatChannelData![0].update(from: src, count: frames)
    let cap = AVAudioFrameCount(Double(frames) * 16_000 / inFormat.sampleRate) + 32
    guard let out = AVAudioPCMBuffer(pcmFormat: outFormat, frameCapacity: cap) else { return }
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
log("capturing")

// Parent-death watchdog: the daemon may be SIGKILLed with our stdout pipe
// still referenced elsewhere; never outlive it.
Timer.scheduledTimer(withTimeInterval: 1.0, repeats: true) { _ in
    if getppid() != parentPid { exit(0) }
}
NotificationCenter.default.addObserver(
    forName: .AVAudioEngineConfigurationChange, object: engine, queue: nil
) { _ in
    // Device change / route change: exit so the daemon respawns us bound to
    // the new default input (same as its RespawnRecorder path for rec).
    log("engine configuration changed — exiting for respawn")
    exit(0)
}
RunLoop.main.run()
