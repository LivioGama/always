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

/// Resolve a lang hint (or the current locale) to a SpeechTranscriber
/// supported locale. Locale.current can carry a region-override subtag (e.g.
/// "en_US@rg=chzzzz") that never appears in supportedLocales — compare on the
/// base identifier. For a language-only match prefer the <lang>_US variant
/// (best-trained English model) over an arbitrary sibling.
@available(macOS 26.0, *)
private func resolveSpeechLocale(langHint: String?) async throws -> Locale {
    let hint = langHint?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
    let wanted = hint.isEmpty ? Locale.current : Locale(identifier: hint)

    let supported = await SpeechTranscriber.supportedLocales
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
    return locale
}

/// Install the on-device speech model for `locale` if absent.
@available(macOS 26.0, *)
private func ensureSpeechModelInstalled(transcriber: SpeechTranscriber, locale: Locale) async throws {
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
}

/// Rescore a segment: when the model's own alternative hypotheses contain a
/// known domain phrase, prefer that hypothesis. This never invents words —
/// it only picks among candidates the engine produced.
@available(macOS 26.0, *)
private func rescoreAgainstPhrases(text: String, alternatives: [AttributedString], phrases: [String]) -> String {
    for alt in alternatives {
        let altText = String(alt.characters)
        if let hit = phrases.first(where: { phrase in
            altText.range(of: phrase, options: [.caseInsensitive]) != nil
                && text.range(of: phrase, options: [.caseInsensitive]) == nil
        }) {
            appleLog("[apple-stt] rescore: '\(text)' -> '\(altText)' (matched '\(hit)')")
            return altText
        }
    }
    return text
}

/// Join segment texts then collapse whitespace — segment texts can carry edge
/// spaces, which produced the observed double-spaced transcripts.
private func joinSegments(_ parts: [String]) -> String {
    parts.joined(separator: " ")
        .components(separatedBy: .whitespaces)
        .filter { !$0.isEmpty }
        .joined(separator: " ")
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
    let locale = try await resolveSpeechLocale(langHint: langHint)

    // .alternativeTranscriptions gives per-segment hypotheses we can rescore
    // against the vocabulary; .transcriptionConfidence attributes each run
    // for diagnostics.
    let transcriber = SpeechTranscriber(
        locale: locale,
        transcriptionOptions: [],
        reportingOptions: [.alternativeTranscriptions],
        attributeOptions: [.transcriptionConfidence]
    )

    try await ensureSpeechModelInstalled(transcriber: transcriber, locale: locale)

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
                let text = rescoreAgainstPhrases(
                    text: String(result.text.characters),
                    alternatives: result.alternatives,
                    phrases: phrases
                )
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
    return joinSegments(parts)
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

// MARK: - Incremental streaming session (macOS 26+, SpeechAnalyzer push API)

/// A live SpeechAnalyzer session fed by `AsyncStream<AnalyzerInput>`. The Rust
/// side pushes 16 kHz mono float32 chunks as they arrive from the capture
/// loop; results are collected on a background task into an ordered segment
/// list so `push` can return the cumulative transcript decoded so far.
///
/// Volatile results (`.volatileResults`) re-emit a segment as the model
/// revises it — the segment list replaces any stored segment whose audio
/// range overlaps a new result, so the joined transcript is always the
/// engine's latest belief about the whole utterance.
@available(macOS 26.0, *)
private final class AppleSttStreamSession {
    private let analyzer: SpeechAnalyzer
    private let inputBuilder: AsyncStream<AnalyzerInput>.Continuation
    private let analyzeTask: Task<CMTime?, Error>
    /// Optional so it defaults before init body — the collecting closure
    /// captures `self`, which is only legal once every stored property is
    /// initialized.
    private var resultsTask: Task<Void, Never>? = nil
    private let inFormat: AVAudioFormat

    private let lock = NSLock()
    private var segments: [(start: Double, end: Double, text: String)] = []
    private var streamError: String?

    init(lang: String?, phrases: [String]) async throws {
        var authStatus = SFSpeechRecognizer.authorizationStatus()
        if authStatus == .notDetermined {
            let sema = DispatchSemaphore(value: 0)
            SFSpeechRecognizer.requestAuthorization { newStatus in
                authStatus = newStatus
                sema.signal()
            }
            _ = sema.wait(timeout: .now() + 5)
        }
        guard authStatus == .authorized else {
            throw NSError(
                domain: "AlwaysAppleSTT", code: 3,
                userInfo: [NSLocalizedDescriptionKey: "speech recognition not authorized"])
        }

        let locale = try await resolveSpeechLocale(langHint: lang)

        // The live session must never block the capture loop on a model
        // download — refuse to start when the locale is not installed; the
        // one-shot path installs it on demand and the next utterance streams.
        let installed = await SpeechTranscriber.installedLocales
        guard installed.contains(where: { $0.identifier == locale.identifier }) else {
            throw NSError(
                domain: "AlwaysAppleSTT", code: 4,
                userInfo: [
                    NSLocalizedDescriptionKey:
                        "speech model for \(locale.identifier) not installed"
                ])
        }

        let transcriber = SpeechTranscriber(
            locale: locale,
            transcriptionOptions: [],
            reportingOptions: [.volatileResults, .alternativeTranscriptions],
            attributeOptions: [.transcriptionConfidence]
        )

        var context = AnalysisContext()
        if !phrases.isEmpty {
            context.contextualStrings = [.general: phrases]
        }

        let (inputSequence, inputBuilder) = AsyncStream<AnalyzerInput>.makeStream()
        self.inputBuilder = inputBuilder

        let analyzer = SpeechAnalyzer(modules: [transcriber])
        self.analyzer = analyzer
        try await analyzer.setContext(context)

        // The recognizer worker only accepts Int16 PCM (16 or 8 kHz mono) —
        // feeding Float32 traps `SpeechRecognizerWorker.preRunRecognition`
        // (SIGTRAP, no error path). Convert f32 → i16 at the FFI boundary.
        let format = AVAudioFormat(
            commonFormat: .pcmFormatInt16,
            sampleRate: 16_000,
            channels: 1,
            interleaved: false)!
        self.inFormat = format
        try await analyzer.prepareToAnalyze(in: format)

        self.analyzeTask = Task { try await analyzer.analyzeSequence(inputSequence) }

        // Collect results into the segment table. Each emission covers
        // `result.range`; a volatile re-emission supersedes whatever we hold
        // for the overlapping span.
        self.resultsTask = Task { [weak self] in
            do {
                for try await result in transcriber.results {
                    guard let self else { break }
                    let start = result.range.start.seconds
                    let end = result.range.end.seconds
                    guard start.isFinite, end.isFinite else { continue }
                    let text = rescoreAgainstPhrases(
                        text: String(result.text.characters),
                        alternatives: result.alternatives,
                        phrases: phrases
                    )
                    self.lock.lock()
                    self.segments.removeAll { $0.start < end && start < $0.end }
                    self.segments.append((start: start, end: end, text: text))
                    self.segments.sort { $0.start < $1.start }
                    self.lock.unlock()
                }
            } catch {
                guard let self else { return }
                self.lock.lock()
                self.streamError = error.localizedDescription
                self.lock.unlock()
                appleLog("[apple-stt] stream results error: \(error.localizedDescription)")
            }
        }
        appleLog("[apple-stt] stream session started locale=\(locale.identifier)")
    }

    /// Cumulative transcript decoded so far, joined in audio order.
    func currentTranscript() -> String {
        lock.lock()
        defer { lock.unlock() }
        return joinSegments(segments.map { $0.text })
    }

    func push(samples: UnsafePointer<Float>, count: Int) -> String? {
        lock.lock()
        let err = streamError
        lock.unlock()
        if err != nil { return nil }
        guard let buf = AVAudioPCMBuffer(
            pcmFormat: inFormat, frameCapacity: AVAudioFrameCount(count))
        else { return nil }
        buf.frameLength = AVAudioFrameCount(count)
        // f32 [-1,1] → i16 PCM, the only sample type the worker accepts.
        let dst = buf.int16ChannelData![0]
        for i in 0..<count {
            let s = samples[i]
            dst[i] = Int16(min(max(s * 32_768.0, -32_768.0), 32_767.0))
        }
        inputBuilder.yield(AnalyzerInput(buffer: buf))
        return currentTranscript()
    }

    /// Close input, flush the analyzer, return the final transcript.
    func finish() async throws -> String {
        inputBuilder.finish()
        try await analyzer.finalizeAndFinishThroughEndOfInput()
        _ = try await analyzeTask.value
        // The results sequence terminates once analysis finishes.
        _ = await resultsTask?.value
        let final = currentTranscript()
        appleLog("[apple-stt] stream finished text=\(final)")
        return final
    }

    /// Tear down without a result (session abandoned mid-utterance).
    func cancel() async {
        inputBuilder.finish()
        resultsTask?.cancel()
        analyzeTask.cancel()
        await analyzer.cancelAndFinishNow()
    }
}

/// Unmanaged handle registry: the C side holds `AppleSttStreamRef` (a retained
/// pointer); we keep a strong ref until `finish`/`cancel`/`free`.
private typealias AppleSttStreamRef = UnsafeMutableRawPointer

/// True when the incremental streaming path can run on this device:
/// macOS 26+ with speech recognition available. Rust gates
/// `supports_streaming` on this.
@_cdecl("apple_stt_stream_supported")
public func appleSttStreamSupported() -> Int32 {
    if #available(macOS 26.0, *) {
        return isAppleSttAvailable()
    }
    return 0
}

/// Start a streaming session. Returns an opaque session pointer, or NULL when
/// streaming can't run now (not authorized, locale unsupported, model not yet
/// installed — the one-shot path covers those cases and installs the model).
/// Blocks up to 4 s on async setup; normally far less.
@_cdecl("apple_stt_stream_start")
public func appleSttStreamStart(
    _ lang: UnsafePointer<CChar>?,
    _ phrases: UnsafePointer<CChar>?
) -> UnsafeMutableRawPointer? {
    guard #available(macOS 26.0, *) else { return nil }

    let langHint = lang.map { String(cString: $0) }.flatMap { $0.isEmpty ? nil : $0 }
    let phraseList = phrases
        .map { String(cString: $0) }
        .map { $0.split(separator: "\n").map { String($0) } } ?? []

    let sema = DispatchSemaphore(value: 0)
    final class Box: @unchecked Sendable { var session: AppleSttStreamSession? }
    let box = Box()
    Task {
        do {
            box.session = try await AppleSttStreamSession(lang: langHint, phrases: phraseList)
        } catch {
            appleLog("[apple-stt] stream start failed: \(error.localizedDescription)")
        }
        sema.signal()
    }
    if sema.wait(timeout: .now() + 4) == .timedOut {
        appleLog("[apple-stt] stream start timed out")
        return nil
    }
    guard let session = box.session else { return nil }
    return Unmanaged.passRetained(session).toOpaque()
}

/// Push `count` float32 samples (16 kHz mono) into the session. Returns a
/// strdup'd cumulative transcript (possibly empty), or NULL on failure.
/// The caller owns the returned string — free with `free_apple_stt_string`.
@_cdecl("apple_stt_stream_push")
public func appleSttStreamPush(
    _ session: UnsafeMutableRawPointer?,
    _ samples: UnsafePointer<Float>?,
    _ count: Int
) -> UnsafeMutablePointer<CChar>? {
    guard #available(macOS 26.0, *), let session, let samples, count > 0 else {
        return nil
    }
    let s = Unmanaged<AppleSttStreamSession>.fromOpaque(session).takeUnretainedValue()
    guard let text = s.push(samples: samples, count: count) else { return nil }
    return duplicateCString(text)
}

/// Finalize the session and return the final transcript as an
/// AppleSttResponse (same ownership rules as `transcribe_wav_with_apple_stt`).
/// The session is consumed — do not reuse the pointer afterwards.
@_cdecl("apple_stt_stream_finish")
public func appleSttStreamFinish(
    _ session: UnsafeMutableRawPointer?
) -> UnsafeMutablePointer<AppleSttResponse> {
    let responsePtr = ResponsePointer.allocate(capacity: 1)
    responsePtr.initialize(to: AppleSttResponse(text: nil, success: 0, error_message: nil))
    guard #available(macOS 26.0, *), let session else {
        responsePtr.pointee.error_message = duplicateCString("no stream session")
        return responsePtr
    }
    let s = Unmanaged<AppleSttStreamSession>.fromOpaque(session).takeRetainedValue()
    let sema = DispatchSemaphore(value: 0)
    final class Box: @unchecked Sendable {
        var text: String?
        var errorText: String?
    }
    let box = Box()
    Task {
        do {
            box.text = try await s.finish()
        } catch {
            box.errorText = error.localizedDescription
        }
        sema.signal()
    }
    if sema.wait(timeout: .now() + 6) == .timedOut {
        appleLog("[apple-stt] stream finish timed out")
        box.errorText = "stream finish timed out"
    }
    if let text = box.text {
        responsePtr.pointee.text = duplicateCString(text)
        responsePtr.pointee.success = 1
    } else {
        responsePtr.pointee.error_message = duplicateCString(box.errorText ?? "unknown error")
    }
    return responsePtr
}

/// Abandon a session without finishing (caller discarded the utterance).
/// Consumes the pointer.
@_cdecl("apple_stt_stream_cancel")
public func appleSttStreamCancel(_ session: UnsafeMutableRawPointer?) {
    guard #available(macOS 26.0, *), let session else { return }
    let s = Unmanaged<AppleSttStreamSession>.fromOpaque(session).takeRetainedValue()
    Task { await s.cancel() }
}

@_cdecl("free_apple_stt_string")
public func freeAppleSttString(_ s: UnsafeMutablePointer<CChar>?) {
    if let s { free(s) }
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
