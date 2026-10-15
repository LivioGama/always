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

/// 1 when Apple's on-device engine can be the DEFAULT backend for a user
/// who never chose one: SpeechAnalyzer exists (macOS 26+) and Speech
/// Recognition has not been refused. On older macOS the legacy recognizer's
/// file path may use Apple's servers, so it is never picked silently.
@_cdecl("apple_stt_default_capable")
public func appleSttDefaultCapable() -> Int32 {
    if #available(macOS 26.0, *) {
        let status = SFSpeechRecognizer.authorizationStatus()
        if status == .denied || status == .restricted { return 0 }
        // SpeechTranscriber is hardware-gated (`isAvailable` is false on
        // e.g. virtualised Apple silicon); there the on-device legacy
        // dictation model serves instead — still nothing leaves the Mac.
        if SpeechTranscriber.isAvailable { return 1 }
        return SFSpeechRecognizer(locale: Locale.current)?.supportsOnDeviceRecognition == true ? 1 : 0
    }
    return 0
}

/// Install the on-device SpeechTranscriber model for `lang` (nil/empty =
/// current locale) BEFORE the first utterance. Without this the model was
/// downloaded inside the first transcription's 12 s budget: on a fresh Mac
/// the first sentences failed with "speech recognition timed out" while the
/// download carried on unseen. Progress is logged every ~5%.
/// Returns 0 = already installed, 1 = installed now, -1 = unsupported
/// locale, -2 = download failed, -3 = timed out, -4 = needs macOS 26,
/// -5 = SpeechTranscriber unavailable on this hardware (the on-device
/// legacy dictation model is used; nothing to install).
@_cdecl("apple_stt_prepare")
public func appleSttPrepare(_ lang: UnsafePointer<CChar>?, _ timeoutSecs: Int32) -> Int32 {
    guard #available(macOS 26.0, *) else { return -4 }
    guard SpeechTranscriber.isAvailable else {
        appleLog("[apple-stt] prepare: SpeechTranscriber unavailable on this Mac — on-device legacy dictation model in use")
        return -5
    }
    let hint = lang.map { String(cString: $0) }.flatMap { $0.isEmpty ? nil : $0 }
    final class Box: @unchecked Sendable { var code: Int32 = -3 }
    let box = Box()
    let semaphore = DispatchSemaphore(value: 0)
    Task {
        defer { semaphore.signal() }
        do {
            let locale = try await resolveAnalyzerLocale(hint)
            let installed = await SpeechTranscriber.installedLocales
            if installed.contains(where: { $0.identifier == locale.identifier }) {
                appleLog("[apple-stt] prepare: model for \(locale.identifier) already installed")
                box.code = 0
                return
            }
            let transcriber = SpeechTranscriber(
                locale: locale, transcriptionOptions: [],
                reportingOptions: [.alternativeTranscriptions],
                attributeOptions: [.transcriptionConfidence])
            guard let request = try await AssetInventory.assetInstallationRequest(supporting: [transcriber]) else {
                appleLog("[apple-stt] prepare: no installation request for \(locale.identifier) (installed)")
                box.code = 0
                return
            }
            appleLog("[apple-stt] prepare: downloading model for \(locale.identifier)")
            let started = Date()
            let progress = request.progress
            let reporter = Task {
                var last = -1
                while !Task.isCancelled {
                    let pct = Int(progress.fractionCompleted * 100)
                    if pct / 5 != last / 5 {
                        appleLog("[apple-stt] prepare: download \(pct)%")
                        last = pct
                    }
                    try? await Task.sleep(nanoseconds: 500_000_000)
                }
            }
            defer { reporter.cancel() }
            try await request.downloadAndInstall()
            appleLog(String(format: "[apple-stt] prepare: installed %@ in %.1f s", locale.identifier, Date().timeIntervalSince(started)))
            box.code = 1
        } catch let error as NSError where error.domain == "AlwaysAppleSTT" && error.code == 2 {
            appleLog("[apple-stt] prepare: \(error.localizedDescription)")
            box.code = -1
        } catch {
            appleLog("[apple-stt] prepare: download failed: \(error.localizedDescription)")
            box.code = -2
        }
    }
    if semaphore.wait(timeout: .now() + .seconds(Int(max(timeoutSecs, 1)))) == .timedOut {
        appleLog("[apple-stt] prepare: still downloading after \(timeoutSecs) s")
        return -3
    }
    return box.code
}

/// The SpeechTranscriber locale for a language hint (nil = current locale),
/// shared by transcription and `apple_stt_prepare` so both pick — and
/// install — the same model.
@available(macOS 26.0, *)
private func resolveAnalyzerLocale(_ langHint: String?) async throws -> Locale {
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
            domain: "AlwaysAppleSTT", code: 2,
            userInfo: [NSLocalizedDescriptionKey: "unsupported locale \(wanted.identifier)"])
    }
    return locale
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
) async throws -> (text: String, confidence: Float) {
    let locale = try await resolveAnalyzerLocale(langHint)

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

    // Per-word acoustic confidences of the settled (final) results — the
    // `.transcriptionConfidence` attribute requested above. Their mean is
    // the cheap "was this even the recognizer's language?" signal the
    // daemon's foreign-speech gate reads (O(words), no extra inference).
    let collectionTask = Task { () -> ([String], [Double]) in
        var parts: [String] = []
        var confidences: [Double] = []
        do {
            for try await result in transcriber.results {
                var text = String(result.text.characters)
                if result.isFinal {
                    for run in result.text.runs {
                        if let c = run.transcriptionConfidence { confidences.append(c) }
                    }
                }
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
        return (parts, confidences)
    }

    try await analyzer.finalizeAndFinishThroughEndOfInput()
    let (parts, confidences) = await collectionTask.value
    let confidence: Float = confidences.isEmpty
        ? -1 : Float(confidences.reduce(0, +) / Double(confidences.count))
    appleLog("[apple-stt] analyzer finished, \(parts.count) settled results, confidence=\(confidence)")
    // Join then collapse whitespace — segment texts can carry edge spaces,
    // which produced the observed double-spaced transcripts.
    let joined = parts.joined(separator: " ")
        .components(separatedBy: .whitespaces)
        .filter { !$0.isEmpty }
        .joined(separator: " ")
    return (joined, confidence)
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
    // contextualStrings only works with cloud recognition — except on
    // macOS 26+, where this path only runs because SpeechTranscriber is
    // unavailable on the hardware and Apple's engine is the DEFAULT for
    // users who never opted into anything: keep that on-device.
    if #available(macOS 26.0, *) {
        req.requiresOnDeviceRecognition = recognizer.supportsOnDeviceRecognition
    } else {
        req.requiresOnDeviceRecognition = false
    }
    // Sentence punctuation: endpointing reads the final terminator to
    // decide the speaker is done (`classify_pause_text`). Without it every
    // legacy transcript looked mid-sentence and the silence window doubled
    // (measured on a fresh macOS 26 VM: all 11 utterances "MidSentence",
    // ~2.1 s from end of speech to text).
    if #available(macOS 13.0, *) { req.addsPunctuation = true }
    if !phrases.isEmpty {
        req.contextualStrings = phrases
    }
    req.shouldReportPartialResults = false

    let semaphore = DispatchSemaphore(value: 0)
    final class ResultBox: @unchecked Sendable {
        var bestText: String?
        var confidence: Float = -1
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
            box.confidence = meanSegmentConfidence(result.bestTranscription)
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
        responsePtr.pointee.confidence = box.confidence
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
    responsePtr.initialize(
        to: AppleSttResponse(text: nil, success: 0, error_message: nil, confidence: -1))

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

    if #available(macOS 26.0, *), SpeechTranscriber.isAvailable {
        let semaphore = DispatchSemaphore(value: 0)
        final class ResultBox: @unchecked Sendable {
            var text: String?
            var confidence: Float = -1
            var errorText: String?
        }
        let box = ResultBox()

        Task {
            do {
                let out = try await transcribeWithSpeechAnalyzer(
                    fileURL: fileURL,
                    langHint: langHint,
                    phrases: phraseList
                )
                box.text = out.text
                box.confidence = out.confidence
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
                responsePtr.pointee.confidence = box.confidence
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

/// Mean per-segment confidence of a legacy SFSpeech transcription, or -1
/// when the engine reported none (partials carry 0 for every segment).
private func meanSegmentConfidence(_ t: SFTranscription) -> Float {
    let values = t.segments.map { $0.confidence }.filter { $0 > 0 }
    if values.isEmpty { return -1 }
    return values.reduce(0, +) / Float(values.count)
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
    /// Partial push target (see `apple_stt_stream_set_callback`). Invoked
    /// under `lock`, so once `finish`/`abort` has taken the lock and set
    /// `closed`, no callback is running or will run.
    private var partialCallback: apple_stt_partial_cb?
    private var partialContext: UnsafeMutableRawPointer?

    func setCallback(_ cb: apple_stt_partial_cb?, _ ctx: UnsafeMutableRawPointer?) {
        lock.lock()
        partialCallback = cb
        partialContext = ctx
        lock.unlock()
    }

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
        // Strictly on-device. The session is fed audio from speech ONSET,
        // before the speaker gate has verified whose voice it is — media
        // (YouTube) speech gets fed too and its output discarded. With
        // cloud fallback enabled that audio would be shipped to Apple's
        // servers for recognition we then throw away. On-device keeps the
        // entire path local: nothing about unverified audio ever leaves
        // the machine.
        req.requiresOnDeviceRecognition = true
        if #available(macOS 13.0, *) { req.addsPunctuation = true }  // see transcribeWithLegacyRecognizer
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
                if !text.isEmpty && text != session.latestText && !session.closed,
                   let cb = session.partialCallback {
                    text.withCString { cb(session.partialContext, $0) }
                }
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

    /// 0 unknown, 1 heard words, 2 finished with no words, 3 failed —
    /// see `apple_stt_stream_outcome` in the bridge header.
    func outcome() -> Int32 {
        lock.lock()
        defer { lock.unlock() }
        if !latestText.isEmpty { return 1 }
        if failed { return 3 }
        if let t = finalText, t.isEmpty { return 2 }
        return 0
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

@_cdecl("apple_stt_stream_set_callback")
public func appleSttStreamSetCallback(
    _ handle: UnsafeMutableRawPointer,
    _ cb: apple_stt_partial_cb?,
    _ ctx: UnsafeMutableRawPointer?
) {
    let session = Unmanaged<AppleSttStreamSession>.fromOpaque(handle).takeUnretainedValue()
    session.setCallback(cb, ctx)
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

@_cdecl("apple_stt_stream_outcome")
public func appleSttStreamOutcome(_ handle: UnsafeMutableRawPointer) -> Int32 {
    let session = Unmanaged<AppleSttStreamSession>.fromOpaque(handle).takeUnretainedValue()
    return session.outcome()
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
