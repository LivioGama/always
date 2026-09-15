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
