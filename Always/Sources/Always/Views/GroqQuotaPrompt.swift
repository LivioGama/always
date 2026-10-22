import AppKit

/// Shown when Groq's daily (free-tier) quota is used up: offers a one-click
/// switch to an on-device engine. The daemon sends the trigger at most once
/// per day, so declining is never nagged.
@MainActor
enum GroqQuotaPrompt {
    static func present() {
        let models = ModelManagerClient.shared
        guard models.activeBackend == "groq" else { return }
        // Same pick as the daemon's fallback model: recommended first, then
        // speed + accuracy. Apple's recognizer needs no download.
        let rank = { (m: ModelInfo) -> (Int, Float) in
            (m.is_recommended ? 1 : 0, m.speed_score + m.accuracy_score)
        }
        let local = models.models
            .filter { $0.is_downloaded && !$0.is_downloading }
            .max { rank($0) < rank($1) }
        // Apple only where its recognizer exists (macOS 26+); otherwise offer
        // to download the best model and switch once it is installed.
        let apple = local == nil && SpeechModelSetup.shared.isOnDeviceEngineAvailable
        let download = local == nil && !apple
            ? models.models.filter { !$0.is_custom }.max { rank($0) < rank($1) }
            : nil

        let alert = NSAlert()
        alert.alertStyle = .informational
        alert.messageText = "Groq's free quota is used up for today"
        let action: (() -> Void)?
        if let local {
            alert.informativeText = "Dictation through Groq will fail until the quota resets. "
                + "Switch to \(local.name)? It runs on this Mac, is free, and keeps live preview. "
                + "You can switch back in Settings → Models."
            alert.addButton(withTitle: "Switch to \(local.name)")
            action = { models.setActive(backend: "local:\(local.id)") }
        } else if apple {
            alert.informativeText = "Dictation through Groq will fail until the quota resets. "
                + "Switch to Apple on-device recognition? It runs on this Mac and is free. "
                + "You can switch back in Settings → Models."
            alert.addButton(withTitle: "Switch to Apple on-device recognition")
            action = { models.setActive(backend: "apple") }
        } else if let download {
            alert.informativeText = "Dictation through Groq will fail until the quota resets. "
                + "Download \(download.name) (\(download.size_mb) MB) and switch to it when it's ready? "
                + "It runs on this Mac and is free. You can switch back in Settings → Models."
            alert.addButton(withTitle: "Download & Switch")
            action = {
                models.pendingBackend = "local:\(download.id)"
                models.download(download.id)
            }
        } else {
            alert.informativeText = "Dictation through Groq will fail until the quota resets. "
                + "You can pick another engine in Settings → Models."
            action = nil
        }
        alert.addButton(withTitle: action == nil ? "OK" : "Keep Groq")

        // The alert takes focus; hand it back so dictation pastes land
        // where the user was working.
        let previous = NSWorkspace.shared.frontmostApplication
        NSApp.activate(ignoringOtherApps: true)
        let response = alert.runModal()
        previous?.activate()
        if response == .alertFirstButtonReturn { action?() }
    }
}
