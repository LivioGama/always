import AVFAudio
import Dispatch
import Foundation
import Speech

private typealias ResponsePointer = UnsafeMutablePointer<AppleSttResponse>

private func appleLog(_ message: String) {
    var msg = message + "\n"
    msg.withCString { ptr in
        _ = fputs(ptr, stderr)
    }
    fflush(stderr)
    // stderr is /dev/null when spawned by the GUI; mirror to a file for
    // debugging. Cheap append, one line per call.
    if let data = msg.data(using: .utf8),
        let handle = FileHandle(forWritingAtPath: "/tmp/always_apple_stt.log")
    {
        handle.seekToEndOfFile()
        handle.write(data)
        try? handle.close()
    } else if let data = msg.data(using: .utf8) {
        try? data.write(to: URL(fileURLWithPath: "/tmp/always_apple_stt.log"))
    }
}

private func duplicateCString(_ text: String) -> UnsafeMutablePointer<CChar>? {
    return text.withCString { basePointer in
        guard let duplicated = strdup(basePointer) else {
            return nil
        }
        return duplicated
    }
}

@_cdecl("is_apple_stt_available")
public func isAppleSttAvailable() -> Int32 {
    // Both SpeechTranscriber (macOS 26+) and SFSpeechRecognizer share the
    // same TCC speech-recognition permission.
    if #available(macOS 26.0, *) {
        let status = SFSpeechRecognizer.authorizationStatus()
        return (status == .authorized || status == .notDetermined) ? 1 : 0
    }
    guard SFSpeechRecognizer() != nil else { return 0 }
    let status = SFSpeechRecognizer.authorizationStatus()
    return (status == .authorized || status == .notDetermined) ? 1 : 0
}

/// SpeechAnalyzer + SpeechTranscriber path (macOS 26+). SpeechTranscriber is
/// Apple's newest, most accurate on-device engine (~4x lower WER than the
/// legacy SFSpeechRecognizer model) and the fastest path measured here
/// (~300 ms). Domain vocabulary is passed via `AnalysisContext.contextualStrings`
/// under `.general` — the only tag the SDK defines.
///
/// Note: `DictationTranscriber` + a compiled `SFCustomLanguageModelData` custom
/// LM was evaluated and rejected — it added 2-5 s of latency per utterance and
/// was *less* accurate on technical vocabulary than plain SpeechTranscriber.
@available(macOS 26.0, *)
private func transcribeWithSpeechAnalyzer(
    fileURL: URL,
    langHint: String?,
    phrases: [String]
) async throws -> String {
    let hint = langHint?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
    let wanted = hint.isEmpty ? Locale.current : Locale(identifier: hint)

    let supported = await SpeechTranscriber.supportedLocales
    appleLog("[apple-stt] supported locales: \(supported.map { $0.identifier })")
    // Locale.current can carry a region-override subtag (e.g.
    // "en_US@rg=chzzzz") that never appears in supportedLocales — compare on
    // the base identifier. For a language-only match prefer the <lang>_US
    // variant (best-trained English model) over an arbitrary sibling.
    let normalized = wanted.identifier.components(separatedBy: "@").first ?? wanted.identifier
    let wantedLanguage = wanted.language.languageCode?.identifier
    let languageMatches = supported.filter {
        $0.language.languageCode?.identifier == wantedLanguage && wantedLanguage != nil
    }
    guard
        let locale = supported.first(where: { $0.identifier == normalized })
            ?? languageMatches.first(where: { $0.identifier.hasSuffix("_US") })
            ?? languageMatches.first
    else {
        throw NSError(
            domain: "AlwaysAppleSTT",
            code: 2,
            userInfo: [
                NSLocalizedDescriptionKey: "unsupported locale \(wanted.identifier)"
            ]
        )
    }

    // .alternativeTranscriptions gives per-segment hypotheses we can rescore
    // against the vocabulary; .transcriptionConfidence attributes each run
    // for diagnostics.
    let transcriber = SpeechTranscriber(
        locale: locale,
        transcriptionOptions: [],
        reportingOptions: [.alternativeTranscriptions],
        attributeOptions: [.transcriptionConfidence]
    )

    let installed = await SpeechTranscriber.installedLocales
    appleLog("[apple-stt] installed locales: \(installed.map { $0.identifier }), using \(locale.identifier)")
    if !installed.contains(where: { $0.identifier == locale.identifier }) {
        appleLog("[apple-stt] installing speech model for \(locale.identifier)")
        if let request = try await AssetInventory.assetInstallationRequest(supporting: [
            transcriber
        ]) {
            try await request.downloadAndInstall()
        }
    }

    var context = AnalysisContext()
    if !phrases.isEmpty {
        context.contextualStrings = [.general: phrases]
    }

    let audioFile = try AVAudioFile(forReading: fileURL)
    // The inputAudioFile init is the only file path that accepts
    // `analysisContext` (contextualStrings) on this SDK. finishAfterFile is
    // left false and finalization is driven explicitly: without
    // `finalizeAndFinishThroughEndOfInput()` the tail — and on observed
    // builds every segment — never reaches isFinal, which is exactly the
    // "0 final results" failure from the first implementation.
    let analyzer = try await SpeechAnalyzer(
        inputAudioFile: audioFile,
        modules: [transcriber],
        analysisContext: context,
        finishAfterFile: false
    )

    let collectionTask = Task { () -> [String] in
        var parts: [String] = []
        do {
            for try await result in transcriber.results {
                var text = String(result.text.characters)
                // Rescore: when the model's own alternative hypotheses contain a
                // known domain phrase, prefer that hypothesis. This never invents
                // words — it only picks among candidates the engine produced.
                for alt in result.alternatives {
                    let altText = String(alt.characters)
                    if let hit = phrases.first(where: { phrase in
                        altText.range(of: phrase, options: [.caseInsensitive]) != nil
                            && text.range(of: phrase, options: [.caseInsensitive]) == nil
                    }) {
                        appleLog("[apple-stt] rescore: '\(text)' -> '\(altText)' (matched '\(hit)')")
                        text = altText
                        break
                    }
                }
                appleLog("[apple-stt] result #\(parts.count + 1) final=\(result.isFinal) text=\(text)")
                if result.isFinal {
                    parts.append(text)
                }
            }
        } catch {
            appleLog("[apple-stt] results stream error: \(error.localizedDescription)")
        }
        return parts
    }

    try await analyzer.finalizeAndFinishThroughEndOfInput()
    let parts = await collectionTask.value
    appleLog("[apple-stt] analyzer finished, \(parts.count) settled results")
    // Join then collapse whitespace — segment texts can carry edge spaces,
    // which produced the observed double-spaced transcripts.
    return parts.joined(separator: " ")
        .components(separatedBy: .whitespaces)
        .filter { !$0.isEmpty }
        .joined(separator: " ")
}

/// Legacy SFSpeechRecognizer path for macOS < 26. Kept because the Apple
/// backend is the only zero-download option on older systems.
private func transcribeWithLegacyRecognizer(
    fileURL: URL,
    langHint: String?,
    phrases: [String],
    locale: Locale,
    responsePtr: ResponsePointer
) -> UnsafeMutablePointer<AppleSttResponse> {
    guard let recognizer = SFSpeechRecognizer(locale: locale) else {
        appleLog("[apple-stt] no recognizer for locale \(locale.identifier)")
        responsePtr.pointee.error_message = duplicateCString("no speech recognizer for locale")
        return responsePtr
    }

    // Run callbacks on a dedicated serial queue instead of the main queue.
    // The daemon's main thread is Rust and does not pump NSRunLoop, so a
    // main-queue callback would never fire. The underlying DispatchQueue must
    // be retained strongly (underlyingQueue is unowned).
    let callbackQueue = OperationQueue()
    callbackQueue.maxConcurrentOperationCount = 1
    callbackQueue.name = "com.always.apple-stt"
    callbackQueue.qualityOfService = .userInitiated
    callbackQueue.underlyingQueue = LegacyStt.dispatchQueue
    recognizer.queue = callbackQueue

    let req = SFSpeechURLRecognitionRequest(url: fileURL)
    // contextualStrings only works with cloud recognition.
    req.requiresOnDeviceRecognition = false
    if !phrases.isEmpty {
        req.contextualStrings = phrases
    }
    req.shouldReportPartialResults = false

    let semaphore = DispatchSemaphore(value: 0)
    final class ResultBox: @unchecked Sendable {
        var bestText: String?
        var errorText: String?
        var noSpeech = false
        var done = false
    }
    let box = ResultBox()

    let task = recognizer.recognitionTask(with: req) { result, error in
        if let error = error {
            if isNoSpeechError(error) {
                box.noSpeech = true
            } else {
                box.errorText = error.localizedDescription
            }
            box.done = true
            semaphore.signal()
            return
        }
        if let result = result, result.isFinal {
            box.bestText = result.bestTranscription.formattedString
            box.done = true
            semaphore.signal()
        }
    }

    let deadline = Date().addingTimeInterval(5)
    while !box.done && Date() < deadline {
        Thread.sleep(forTimeInterval: 0.05)
    }
    _ = task

    if !box.done {
        box.errorText = "speech recognition timed out"
    }

    if let text = box.bestText {
        responsePtr.pointee.text = duplicateCString(text)
        responsePtr.pointee.success = 1
    } else if box.noSpeech {
        responsePtr.pointee.success = 1
    } else {
        responsePtr.pointee.error_message = duplicateCString(box.errorText ?? "unknown error")
    }
    return responsePtr
}

@_cdecl("transcribe_wav_with_apple_stt")
public func transcribeWavWithAppleStt(
    _ wavPath: UnsafePointer<CChar>,
    _ lang: UnsafePointer<CChar>?,
    _ phrases: UnsafePointer<CChar>?
) -> UnsafeMutablePointer<AppleSttResponse> {
    let responsePtr = ResponsePointer.allocate(capacity: 1)
    responsePtr.initialize(to: AppleSttResponse(text: nil, success: 0, error_message: nil))

    let path = String(cString: wavPath)
    let fileURL = URL(fileURLWithPath: path)

    let langHint = lang.map { String(cString: $0) }.flatMap { $0.isEmpty ? nil : $0 }
    let phraseList = phrases
        .map { String(cString: $0) }
        .map { $0.split(separator: "\n").map { String($0) } } ?? []

    let locale: Locale = langHint.map { Locale(identifier: $0) } ?? Locale.current
    appleLog("[apple-stt] transcribe path=\(path) locale=\(locale.identifier) phrases=\(phraseList.count)")

    var authStatus = SFSpeechRecognizer.authorizationStatus()
    if authStatus == .notDetermined {
        let semaphore = DispatchSemaphore(value: 0)
        SFSpeechRecognizer.requestAuthorization { newStatus in
            authStatus = newStatus
            semaphore.signal()
        }
        _ = semaphore.wait(timeout: .now() + 5)
    }
    if authStatus != .authorized {
        appleLog("[apple-stt] not authorized, status=\(authStatus.rawValue)")
        responsePtr.pointee.error_message = duplicateCString("speech recognition not authorized")
        return responsePtr
    }

    if #available(macOS 26.0, *) {
        let semaphore = DispatchSemaphore(value: 0)
        final class ResultBox: @unchecked Sendable {
            var text: String?
            var errorText: String?
        }
        let box = ResultBox()

        Task {
            do {
                box.text = try await transcribeWithSpeechAnalyzer(
                    fileURL: fileURL,
                    langHint: langHint,
                    phrases: phraseList
                )
            } catch {
                appleLog("[apple-stt] analyzer error: \(error.localizedDescription)")
                box.errorText = error.localizedDescription
            }
            semaphore.signal()
        }

        if semaphore.wait(timeout: .now() + 12) == .timedOut {
            appleLog("[apple-stt] analyzer timed out")
            box.errorText = "speech recognition timed out"
        }

        if let text = box.text {
            appleLog("[apple-stt] analyzer result text=\(text)")
            if text.isEmpty {
                // Empty analyzer output used to be returned as a silent
                // success — the user heard nothing. Fall back to the legacy
                // SFSpeechRecognizer (cloud-capable) before giving up.
                appleLog("[apple-stt] analyzer empty — falling back to legacy recognizer")
                return transcribeWithLegacyRecognizer(
                    fileURL: fileURL,
                    langHint: langHint,
                    phrases: phraseList,
                    locale: locale,
                    responsePtr: responsePtr
                )
            } else {
                responsePtr.pointee.text = duplicateCString(text)
                responsePtr.pointee.success = 1
            }
        } else {
            responsePtr.pointee.error_message = duplicateCString(
                box.errorText ?? "unknown error")
        }
        return responsePtr
    }

    return transcribeWithLegacyRecognizer(
        fileURL: fileURL,
        langHint: langHint,
        phrases: phraseList,
        locale: locale,
        responsePtr: responsePtr
    )
}

/// Shared serial dispatch queue for the legacy SFSpeechRecognizer callback
/// path. `OperationQueue.underlyingQueue` is unowned, so this must live at
/// file scope.
private enum LegacyStt {
    static let dispatchQueue = DispatchQueue(label: "com.always.apple-stt", qos: .userInitiated)
}

private func isNoSpeechError(_ error: Error) -> Bool {
    let desc = error.localizedDescription.lowercased()
    return desc.contains("no speech detected") || desc.contains("no speech")
        || desc.contains("nothing detected")
}

@_cdecl("free_apple_stt_result")
public func freeAppleSttResult(_ response: UnsafeMutablePointer<AppleSttResponse>?) {
    guard let response = response else { return }

    if let text = response.pointee.text {
        free(UnsafeMutablePointer(mutating: text))
    }

    if let error = response.pointee.error_message {
        free(UnsafeMutablePointer(mutating: error))
    }

    response.deallocate()
}

// MARK: - Streaming recognition (SFSpeechAudioBufferRecognitionRequest)
//
// The file-based paths above only return text AFTER end-of-utterance —
// the user waits silence-detection + a full WAV round trip before the
// first word appears. A buffer request fed live yields partial results
// within ~200-400ms of speech, which is what dictation is supposed to
// feel like. The daemon creates one session per utterance, feeds 16 kHz
// mono i16 frames as they arrive, polls partials, and finishes at the
// end-of-speech cut.

/// One live dictation session. Thread-safe: the recognition task
/// callbacks arrive on the recognizer's queue while the daemon's
/// event-loop thread feeds audio and polls.
final class AppleSttStreamSession: @unchecked Sendable {
    private let lock = NSLock()
    private var request: SFSpeechAudioBufferRecognitionRequest?
    private var task: SFSpeechRecognitionTask?
    /// Monotonic partial text — grows as recognition proceeds.
    private var latestText: String = ""
    /// Set once when the task reports `isFinal` (or a terminal error).
    private var finalText: String?
    private var failed = false
    /// Set by `finishHandle()` so late callbacks don't resurrect state.
    private var closed = false

    private init() {}

    /// Build and start a streaming session. Returns nil when speech
    /// recognition is unavailable/authorized-out — the caller falls back
    /// to the file-based path.
    static func start(langHint: String?, phrases: [String]) -> AppleSttStreamSession? {
        var authStatus = SFSpeechRecognizer.authorizationStatus()
        if authStatus == .notDetermined {
            let semaphore = DispatchSemaphore(value: 0)
            SFSpeechRecognizer.requestAuthorization { newStatus in
                authStatus = newStatus
                semaphore.signal()
            }
            _ = semaphore.wait(timeout: .now() + 5)
        }
        guard authStatus == .authorized else {
            appleLog("[apple-stt-stream] not authorized, status=\(authStatus.rawValue)")
            return nil
        }
        let locale: Locale = langHint.map { Locale(identifier: $0) } ?? Locale.current
        guard let recognizer = SFSpeechRecognizer(locale: locale) else {
            appleLog("[apple-stt-stream] no recognizer for locale \(locale.identifier)")
            return nil
        }

        let session = AppleSttStreamSession()
        let callbackQueue = OperationQueue()
        callbackQueue.maxConcurrentOperationCount = 1
        callbackQueue.name = "com.always.apple-stt-stream"
        callbackQueue.qualityOfService = .userInitiated
        callbackQueue.underlyingQueue = LegacyStt.dispatchQueue
        recognizer.queue = callbackQueue

        let req = SFSpeechAudioBufferRecognitionRequest()
        req.shouldReportPartialResults = true
        // On-device keeps partials private and avoids network jitter.
        // Cloud fallback stays available for accuracy parity with the
        // file path (which also leaves it off).
        req.requiresOnDeviceRecognition = false
        if !phrases.isEmpty {
            req.contextualStrings = phrases
        }
        session.request = req

        session.task = recognizer.recognitionTask(with: req) { [weak session] result, error in
            guard let session else { return }
            session.lock.lock()
            defer { session.lock.unlock() }
            if let result {
                let text = result.bestTranscription.formattedString
                if !text.isEmpty { session.latestText = text }
                if result.isFinal {
                    session.finalText = session.latestText
                }
            }
            if let error {
                // "No speech detected" is a normal terminal outcome for an
                // empty/quiet session — not a failure worth reporting.
                if isNoSpeechError(error) {
                    session.finalText = session.finalText ?? ""
                } else {
                    appleLog("[apple-stt-stream] task error: \(error.localizedDescription)")
                    session.failed = true
                }
            }
        }
        appleLog("[apple-stt-stream] session started locale=\(locale.identifier)")
        return session
    }

    /// Append 16 kHz mono i16 samples. Converting to float32 here keeps
    /// the Rust side a plain memcpy-free pointer pass.
    func feed(_ samples: UnsafePointer<Int16>, count: Int) {
        lock.lock()
        let request = self.request
        let closed = self.closed
        lock.unlock()
        guard let request, !closed, count > 0 else { return }
        guard
            let format = AVAudioFormat(
                commonFormat: .pcmFormatFloat32,
                sampleRate: 16_000,
                channels: 1,
                interleaved: false
            ),
            let buffer = AVAudioPCMBuffer(
                pcmFormat: format,
                frameCapacity: AVAudioFrameCount(count)
            )
        else { return }
        buffer.frameLength = AVAudioFrameCount(count)
        let dst = buffer.floatChannelData![0]
        for i in 0..<count {
            dst[i] = Float(samples[i]) / 32768.0
        }
        request.append(buffer)
    }

    /// Latest partial text, or nil when unchanged since `since` —
    /// the caller passes its last-seen text to dedupe broadcasts.
    func partial(after seen: String) -> String? {
        lock.lock()
        defer { lock.unlock() }
        if latestText.isEmpty || latestText == seen { return nil }
        return latestText
    }

    /// End audio and wait up to `timeoutMs` for the final result.
    /// Falls back to the latest partial when the task won't finalize in
    /// time — a slightly stale word beats re-transcribing from scratch.
    func finish(timeoutMs: Int32) -> String? {
        lock.lock()
        let request = self.request
        closed = true
        lock.unlock()
        request?.endAudio()

        let deadline = Date().addingTimeInterval(TimeInterval(timeoutMs) / 1000.0)
        while Date() < deadline {
            lock.lock()
            let done = finalText != nil || failed
            lock.unlock()
            if done { break }
            usleep(20_000)
        }
        lock.lock()
        defer { lock.unlock() }
        if let t = finalText { return t }
        if failed { return nil }
        return latestText.isEmpty ? nil : latestText
    }

    func abort() {
        lock.lock()
        closed = true
        let task = self.task
        let request = self.request
        lock.unlock()
        task?.finish() // deliver whatever was recognized, then
        request?.endAudio()
    }
}

@_cdecl("apple_stt_stream_start")
public func appleSttStreamStart(
    _ lang: UnsafePointer<CChar>?,
    _ phrases: UnsafePointer<CChar>?
) -> UnsafeMutableRawPointer? {
    let langHint = lang.map { String(cString: $0) }.flatMap { $0.isEmpty ? nil : $0 }
    let phraseList = phrases
        .map { String(cString: $0) }
        .map { $0.split(separator: "\n").map { String($0) } } ?? []
    guard let session = AppleSttStreamSession.start(langHint: langHint, phrases: phraseList)
    else { return nil }
    return Unmanaged.passRetained(session).toOpaque()
}

@_cdecl("apple_stt_stream_feed")
public func appleSttStreamFeed(
    _ handle: UnsafeMutableRawPointer,
    _ samples: UnsafePointer<Int16>,
    _ count: Int32
) {
    let session = Unmanaged<AppleSttStreamSession>.fromOpaque(handle).takeUnretainedValue()
    session.feed(samples, count: Int(count))
}

/// Returns the latest partial as a freshly-allocated C string the caller
/// must free with `free_apple_stt_string`, or NULL when nothing new.
@_cdecl("apple_stt_stream_partial")
public func appleSttStreamPartial(
    _ handle: UnsafeMutableRawPointer,
    _ seen: UnsafePointer<CChar>?
) -> UnsafeMutablePointer<CChar>? {
    let session = Unmanaged<AppleSttStreamSession>.fromOpaque(handle).takeUnretainedValue()
    let seenText = seen.map { String(cString: $0) } ?? ""
    guard let text = session.partial(after: seenText) else { return nil }
    return duplicateCString(text)
}

/// Ends the audio stream and blocks up to timeout_ms for the final text.
/// Returns a freshly-allocated C string (free with
/// `free_apple_stt_string`) or NULL when recognition produced nothing.
@_cdecl("apple_stt_stream_finish")
public func appleSttStreamFinish(
    _ handle: UnsafeMutableRawPointer,
    _ timeoutMs: Int32
) -> UnsafeMutablePointer<CChar>? {
    let session = Unmanaged<AppleSttStreamSession>.fromOpaque(handle).takeUnretainedValue()
    guard let text = session.finish(timeoutMs: timeoutMs), !text.isEmpty else { return nil }
    return duplicateCString(text)
}

@_cdecl("apple_stt_stream_free")
public func appleSttStreamFree(_ handle: UnsafeMutableRawPointer?) {
    guard let handle else { return }
    let session = Unmanaged<AppleSttStreamSession>.fromOpaque(handle).takeRetainedValue()
    session.abort()
}

@_cdecl("free_apple_stt_string")
public func freeAppleSttString(_ ptr: UnsafeMutablePointer<CChar>?) {
    guard let ptr else { return }
    free(ptr)
}
