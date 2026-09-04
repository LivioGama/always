import Foundation
import Combine

/// Narrates onboarding steps using the macOS `say` CLI, which respects
/// the system voice setting — including Siri natural voices that
/// `AVSpeechSynthesizer` cannot use.
///
/// If the system voice is a Siri natural voice, narration uses it.
/// If not, narration is silent — no fallback to low-quality voices.
@MainActor
final class OnboardingNarrator: ObservableObject {
    static let shared = OnboardingNarrator()

    @Published private(set) var isSpeaking = false

    private var currentProcess: Process?

    private init() {}

    /// Speak a phrase using `say`. Cancels any in-flight speech first.
    func speak(_ text: String, rateMultiplier: Float = 1.0) {
        guard !text.isEmpty else { return }
        stop()

        guard Self.systemVoiceIsSiri() else {
            // No Siri voice set as system voice — stay silent.
            return
        }

        // `say` without -v uses the system voice (which is a Siri voice).
        // Rate: `say` uses words per minute (default ~175). Slower for
        // a calm, guided feel.
        let wpm = Int(175 * 0.85 * Double(rateMultiplier))

        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/say")
        process.arguments = ["-r", String(wpm), text]

        // Pipe stderr/stdout to /dev/null so it doesn't clutter logs.
        let devnull = FileHandle(forWritingAtPath: "/dev/null")
        process.standardOutput = devnull
        process.standardError = devnull

        process.terminationHandler = { [weak self] _ in
            Task { @MainActor in
                self?.didFinishSpeaking()
            }
        }

        do {
            try process.run()
            currentProcess = process
            isSpeaking = true
        } catch {
            // If `say` fails to launch, stay silent — no fallback.
            isSpeaking = false
        }
    }

    func stop() {
        currentProcess?.terminate()
        currentProcess = nil
        isSpeaking = false
    }

    /// Called when the `say` process exits.
    fileprivate func didFinishSpeaking() {
        currentProcess = nil
        isSpeaking = false
    }

    // MARK: - Siri voice detection

    /// Check if the system's spoken-content voice is a Siri natural voice.
    /// Reads `com.apple.Accessibility SpokenContentDefaultVoiceSelectionsByLanguage`
    /// and checks whether any `voiceId` starts with `com.apple.siri.natural`.
    private static func systemVoiceIsSiri() -> Bool {
        // Try the modern Accessibility domain first (macOS 13+).
        if let result = checkAccessibilityDomain() {
            return result
        }
        // Fallback: check the legacy speech.voice.prefs domain.
        return checkLegacyDomain()
    }

    private static func checkAccessibilityDomain() -> Bool? {
        let task = Process()
        task.executableURL = URL(fileURLWithPath: "/usr/bin/defaults")
        task.arguments = ["read", "com.apple.Accessibility",
                          "SpokenContentDefaultVoiceSelectionsByLanguage"]

        let pipe = Pipe()
        task.standardOutput = pipe
        task.standardError = FileHandle(forWritingAtPath: "/dev/null")

        do {
            try task.run()
            task.waitUntilExit()
        } catch {
            return nil
        }

        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        let output = String(data: data, encoding: .utf8) ?? ""
        return output.contains("com.apple.siri.natural")
    }

    private static func checkLegacyDomain() -> Bool {
        // On some macOS versions the voice id is stored in
        // com.apple.speech.voice.prefs. Check SelectedVoiceName or
        // the voice id fields for Siri identifiers.
        let task = Process()
        task.executableURL = URL(fileURLWithPath: "/usr/bin/defaults")
        task.arguments = ["read", "com.apple.speech.voice.prefs"]

        let pipe = Pipe()
        task.standardOutput = pipe
        task.standardError = FileHandle(forWritingAtPath: "/dev/null")

        do {
            try task.run()
            task.waitUntilExit()
        } catch {
            return false
        }

        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        let output = String(data: data, encoding: .utf8) ?? ""
        return output.contains("com.apple.siri.natural")
            || output.contains("Siri")
    }
}
