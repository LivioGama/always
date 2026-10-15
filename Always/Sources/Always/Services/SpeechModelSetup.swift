import AppKit
import AVFoundation
import Foundation
import Speech
import os.log

/// First-run setup for Apple's on-device speech engine — the default
/// transcription backend on macOS 26+ (no API key, nothing leaves the Mac).
///
/// Two things must be true before the first sentence can be transcribed,
/// and before this existed neither was surfaced to a new user:
///   * Speech Recognition permission. The daemon used to ask for it from
///     the faceless background process on the first utterance, waiting
///     5 s for an answer — a user slower than that lost the sentence.
///     Asking here, from the app, shows the prompt in context.
///   * The on-device language model. It was downloaded lazily inside the
///     first transcription's 12 s budget, so on a fresh Mac the first
///     sentences failed with "speech recognition timed out" while the
///     download ran unseen. Here it downloads up front, with progress.
@MainActor
final class SpeechModelSetup: ObservableObject {
    static let shared = SpeechModelSetup()

    enum ModelState: Equatable {
        case unknown
        /// macOS < 26: no SpeechAnalyzer; Apple is not the default engine.
        case notApplicable
        case checking
        case downloading(Double)
        case installed
        /// Hardware without SpeechTranscriber falls back to the system
        /// dictation model, which refuses to run while macOS Dictation is
        /// switched off ("Siri and Dictation are disabled").
        case needsDictation
        case failed(String)
    }

    @Published private(set) var authStatus: SFSpeechRecognizerAuthorizationStatus =
        SFSpeechRecognizer.authorizationStatus()
    @Published private(set) var modelState: ModelState = .unknown
    /// Display name of the language being installed ("English (US)").
    @Published private(set) var languageName: String = ""

    private let logger = Logger(subsystem: "com.always.app", category: "speech-setup")
    private var installTask: Task<Void, Never>?

    /// On-device SpeechAnalyzer exists on this Mac (macOS 26+).
    var isOnDeviceEngineAvailable: Bool {
        if #available(macOS 26.0, *) { return true }
        return false
    }

    var isAuthorized: Bool { authStatus == .authorized }

    /// Everything the default engine needs is in place.
    var isReady: Bool {
        !isOnDeviceEngineAvailable || (isAuthorized && modelState == .installed)
    }

    func refreshAuthorization() {
        let now = SFSpeechRecognizer.authorizationStatus()
        if now != authStatus { authStatus = now }
    }

    func requestAuthorization() {
        guard authStatus == .notDetermined else { return }
        SFSpeechRecognizer.requestAuthorization { status in
            Task { @MainActor in
                self.authStatus = status
                self.logger.info("speech recognition authorization: \(status.rawValue, privacy: .public)")
                if status == .authorized { self.retry() }
            }
        }
    }

    func openSystemSettings() {
        if let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_SpeechRecognition") {
            NSWorkspace.shared.open(url)
        }
    }

    /// Install the on-device model for the current locale if missing.
    /// Idempotent; safe to call on every appearance.
    func ensureModelInstalled() {
        guard installTask == nil else { return }
        switch modelState {
        case .installed, .downloading, .checking: return
        default: break
        }
        #if compiler(>=6.2)
        guard #available(macOS 26.0, *) else {
            modelState = .notApplicable
            return
        }
        guard SpeechTranscriber.isAvailable else {
            // Hardware without SpeechTranscriber: the built-in on-device
            // dictation model serves; nothing to download — but it only
            // runs with macOS Dictation turned on. Probe it once.
            modelState = .checking
            languageName = "built-in dictation model"
            Self.probeLegacyDictation { [weak self] ok in
                // nil: can't tell before Speech Recognition is granted —
                // `requestAuthorization` re-runs this once it is.
                self?.modelState = ok.map { $0 ? .installed : .needsDictation } ?? .unknown
            }
            return
        }
        modelState = .checking
        installTask = Task { [weak self] in
            await self?.install()
            self?.installTask = nil
        }
        #else
        modelState = .notApplicable
        #endif
    }

    #if compiler(>=6.2)
    @available(macOS 26.0, *)
    private func install() async {
        do {
            guard let locale = await Self.analyzerLocale() else {
                modelState = .failed("Your language (\(Locale.current.identifier)) isn't supported by Apple's on-device engine")
                return
            }
            languageName = Locale.current.localizedString(forIdentifier: locale.identifier) ?? locale.identifier
            let installed = await SpeechTranscriber.installedLocales
            if installed.contains(where: { $0.identifier == locale.identifier }) {
                modelState = .installed
                return
            }
            let transcriber = SpeechTranscriber(
                locale: locale, transcriptionOptions: [],
                reportingOptions: [.alternativeTranscriptions],
                attributeOptions: [.transcriptionConfidence])
            guard let request = try await AssetInventory.assetInstallationRequest(supporting: [transcriber]) else {
                modelState = .installed
                return
            }
            modelState = .downloading(0)
            let progress = request.progress
            let poll = Task { @MainActor [weak self] in
                while !Task.isCancelled {
                    if case .downloading = self?.modelState {
                        self?.modelState = .downloading(progress.fractionCompleted)
                    }
                    try? await Task.sleep(nanoseconds: 250_000_000)
                }
            }
            defer { poll.cancel() }
            try await request.downloadAndInstall()
            modelState = .installed
            logger.info("on-device speech model installed for \(locale.identifier, privacy: .public)")
        } catch {
            logger.error("speech model install failed: \(error.localizedDescription, privacy: .public)")
            modelState = .failed(error.localizedDescription)
        }
    }

    /// Same locale choice as the daemon (`swift/apple_stt.swift`
    /// `resolveAnalyzerLocale`) so the model installed here is the one the
    /// daemon uses.
    @available(macOS 26.0, *)
    private static func analyzerLocale() async -> Locale? {
        let wanted = Locale.current
        let supported = await SpeechTranscriber.supportedLocales
        let normalized = wanted.identifier.components(separatedBy: "@").first ?? wanted.identifier
        let language = wanted.language.languageCode?.identifier
        let matches = supported.filter {
            $0.language.languageCode?.identifier == language && language != nil
        }
        return supported.first(where: { $0.identifier == normalized })
            ?? matches.first(where: { $0.identifier.hasSuffix("_US") })
            ?? matches.first
    }
    #endif

    /// Retry after a failure.
    func retry() {
        switch modelState {
        case .failed, .needsDictation: modelState = .unknown
        default: break
        }
        ensureModelInstalled()
    }

    func openDictationSettings() {
        if let url = URL(string: "x-apple.systempreferences:com.apple.Keyboard-Settings.extension") {
            NSWorkspace.shared.open(url)
        }
    }

    /// Recognise 0.3 s of silence on-device: succeeds (or reports "no
    /// speech") when the system dictation model can run, fails with
    /// "Siri and Dictation are disabled" when Dictation is off.
    private static func probeLegacyDictation(_ done: @escaping @MainActor (Bool?) -> Void) {
        guard let recognizer = SFSpeechRecognizer(locale: Locale.current) ?? SFSpeechRecognizer(),
              recognizer.supportsOnDeviceRecognition,
              SFSpeechRecognizer.authorizationStatus() == .authorized,
              let format = AVAudioFormat(standardFormatWithSampleRate: 16_000, channels: 1),
              let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: 4_800)
        else {
            // Not authorised yet: unknown. Authorised but no on-device
            // model for this language: the daemon cannot use it either.
            let authorized = SFSpeechRecognizer.authorizationStatus() == .authorized
            Task { @MainActor in done(authorized ? false : nil) }
            return
        }
        buffer.frameLength = 4_800
        let request = SFSpeechAudioBufferRecognitionRequest()
        request.requiresOnDeviceRecognition = true
        final class Once: @unchecked Sendable { var fired = false }
        let once = Once()
        let finish: @Sendable (Bool) -> Void = { ok in
            Task { @MainActor in
                guard !once.fired else { return }
                once.fired = true
                done(ok)
            }
        }
        let task = recognizer.recognitionTask(with: request) { result, error in
            if let error {
                finish(!error.localizedDescription.lowercased().contains("disabled"))
            } else if result?.isFinal == true {
                finish(true)
            }
        }
        request.append(buffer)
        request.endAudio()
        DispatchQueue.main.asyncAfter(deadline: .now() + 5) {
            task.cancel()
            finish(true)
        }
    }
}
