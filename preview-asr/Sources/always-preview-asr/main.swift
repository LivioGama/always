// always-preview-asr — live-preview speech recognizer for the Always daemon.
//
// Parakeet EOU 120M, 160 ms chunks (FluidAudio 0.17.7) on the Neural Engine
// (`computeUnits = .cpuAndNeuralEngine`; `.all` measured slower). The daemon
// spawns this once, keeps it warm, and feeds it 16 kHz mono PCM for each
// utterance. Partial text is PUSHED from the engine's decode callback the
// moment new tokens land — never polled.
//
// This process only produces PREVIEW text. The daemon never pastes it: the
// pasted final comes from the Apple recognizer (SPEC.md §5).
//
// Wire protocol (all integers little-endian):
//   stdin  frames: [op: u8][seq: u32][len: u32][payload: len bytes]
//     op 'B' begin utterance `seq` (resets decoder state)
//     op 'A' audio for `seq`: payload = i16 samples
//     op 'E' end utterance `seq`: flush, emit `final`, reset
//     op 'W' warm-up inference (keeps the ANE path hot)
//   stdout JSON lines:
//     {"ev":"ready","load_ms":…,"warm_ms":…}
//     {"ev":"partial","seq":n,"text":"…","t_ms":…}
//     {"ev":"final","seq":n,"text":"…","finalize_ms":…}
//     {"ev":"error","msg":"…"}
// EOF on stdin = the daemon is gone: exit immediately.

import AVFAudio
import CoreML
import FluidAudio
import Foundation

setvbuf(stdout, nil, _IOLBF, 0)

let clockStart = DispatchTime.now().uptimeNanoseconds
@inline(__always) func nowMs() -> Double {
    Double(DispatchTime.now().uptimeNanoseconds - clockStart) / 1e6
}

let outLock = NSLock()
func emit(_ object: [String: Any]) {
    autoreleasepool { emitLine(object) }
}

private func emitLine(_ object: [String: Any]) {
    guard let data = try? JSONSerialization.data(withJSONObject: object, options: [.sortedKeys]),
        var line = String(data: data, encoding: .utf8)
    else { return }
    line += "\n"
    outLock.lock()
    FileHandle.standardOutput.write(line.data(using: .utf8)!)
    outLock.unlock()
}

/// Utterance currently being decoded. Read from the decode callback, which
/// FluidAudio invokes inside the actor while the consumer task awaits it.
final class SeqBox: @unchecked Sendable {
    private let lock = NSLock()
    private var value: UInt32 = 0
    func set(_ v: UInt32) { lock.lock(); value = v; lock.unlock() }
    func get() -> UInt32 { lock.lock(); defer { lock.unlock() }; return value }
}
let currentSeq = SeqBox()

enum Command {
    case begin(UInt32)
    case audio(UInt32, [Float])
    case end(UInt32)
    case warm
}

func pcmBuffer(_ samples: [Float]) -> AVAudioPCMBuffer {
    let format = AVAudioFormat(
        commonFormat: .pcmFormatFloat32, sampleRate: 16_000, channels: 1, interleaved: false)!
    let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: AVAudioFrameCount(max(samples.count, 1)))!
    buffer.frameLength = AVAudioFrameCount(samples.count)
    samples.withUnsafeBufferPointer {
        buffer.floatChannelData![0].update(from: $0.baseAddress!, count: samples.count)
    }
    return buffer
}

/// Blocking exact read from stdin. nil on EOF.
func readExact(_ count: Int) -> Data? {
    var out = Data(capacity: count)
    while out.count < count {
        let chunk = FileHandle.standardInput.readData(ofLength: count - out.count)
        if chunk.isEmpty { return nil }
        out.append(chunk)
    }
    return out
}

func u32(_ d: Data, _ at: Int) -> UInt32 {
    d.withUnsafeBytes { raw in
        UInt32(littleEndian: raw.loadUnaligned(fromByteOffset: at, as: UInt32.self))
    }
}

let (commands, continuation) = AsyncStream<Command>.makeStream(bufferingPolicy: .unbounded)

// stdin reader: its own thread so a slow decode never stalls the pipe.
let reader = Thread {
    // Each iteration in its own autorelease pool: this thread has no run
    // loop, so autoreleased NSData from FileHandle reads would otherwise
    // accumulate for the life of the process.
    while autoreleasepool(invoking: { readOneFrame() }) {}
    // Daemon closed our stdin (exited or killed): never outlive it.
    exit(0)
}

/// Read and dispatch one stdin frame. false on EOF / protocol error.
func readOneFrame() -> Bool {
    do {
        guard let header = readExact(9) else { return false }
        let op = header[header.startIndex]
        let seq = u32(header, 1)
        let len = Int(u32(header, 5))
        guard len <= 16_000 * 2 * 60, let payload = len > 0 ? readExact(len) : Data() else { return false }
        switch op {
        case UInt8(ascii: "B"): continuation.yield(.begin(seq))
        case UInt8(ascii: "E"): continuation.yield(.end(seq))
        case UInt8(ascii: "W"): continuation.yield(.warm)
        case UInt8(ascii: "A"):
            let n = payload.count / 2
            var samples = [Float](repeating: 0, count: n)
            payload.withUnsafeBytes { raw in
                for i in 0..<n {
                    samples[i] = Float(Int16(littleEndian: raw.loadUnaligned(fromByteOffset: i * 2, as: Int16.self))) / 32768.0
                }
            }
            continuation.yield(.audio(seq, samples))
        default:
            emit(["ev": "error", "msg": "unknown op \(op)"])
        }
        return true
    }
}
reader.stackSize = 1 << 20
reader.start()

let config = MLModelConfiguration()
switch ProcessInfo.processInfo.environment["ALWAYS_PREVIEW_COMPUTE"] ?? "ane" {
case "all": config.computeUnits = .all
case "cpu": config.computeUnits = .cpuOnly
case "gpu": config.computeUnits = .cpuAndGPU
default: config.computeUnits = .cpuAndNeuralEngine
}
let manager = StreamingEouAsrManager(configuration: config, chunkSize: .ms160)

func warmUp() async throws {
    let saved = currentSeq.get()
    currentSeq.set(0)  // seq 0 output is ignored by the daemon
    let noise = (0..<32_000).map { _ in Float.random(in: -0.001...0.001) }
    try await manager.appendAudio(pcmBuffer(noise))
    try await manager.processBufferedAudio()
    _ = try await manager.finish()
    await manager.reset()
    currentSeq.set(saved)
}

let loadStart = nowMs()
do {
    try await manager.loadModels()
} catch {
    emit(["ev": "error", "msg": "load failed: \(error)"])
    exit(3)
}
let loadMs = nowMs() - loadStart
await manager.setPartialCallback { text in
    let seq = currentSeq.get()
    guard seq != 0 else { return }
    emit(["ev": "partial", "seq": seq, "text": text.trimmingCharacters(in: .whitespaces), "t_ms": nowMs()])
}
let warmStart = nowMs()
try? await warmUp()
emit(["ev": "ready", "load_ms": Int(loadMs), "warm_ms": Int(nowMs() - warmStart)])

for await command in commands {
    do {
        switch command {
        case .begin(let seq):
            await manager.reset()
            currentSeq.set(seq)
        case .audio(let seq, let samples):
            guard seq == currentSeq.get(), seq != 0 else { continue }
            try await manager.appendAudio(pcmBuffer(samples))
            try await manager.processBufferedAudio()
        case .end(let seq):
            guard seq == currentSeq.get(), seq != 0 else { continue }
            let started = nowMs()
            let text = try await manager.finish().trimmingCharacters(in: .whitespaces)
            emit(["ev": "final", "seq": seq, "text": text, "finalize_ms": Int(nowMs() - started)])
            currentSeq.set(0)
            await manager.reset()
        case .warm:
            if currentSeq.get() == 0 { try await warmUp() }
        }
    } catch {
        emit(["ev": "error", "msg": "\(error)"])
        await manager.reset()
    }
}
