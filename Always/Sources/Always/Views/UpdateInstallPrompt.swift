import AppKit

/// The one confirmation shown before an update is installed. Installing quits
/// and relaunches Always, which interrupts dictation, so it is never a
/// single stray click. Shared by the menu bar item and the About panel.
@MainActor
enum UpdateInstallPrompt {
    static func present(for offer: UpdateOffer) {
        let alert = NSAlert()
        alert.alertStyle = .informational

        if offer.canApply {
            alert.messageText = "Install Always v\(offer.latest)?"
            alert.informativeText =
                "Always will download and verify the update, then quit and relaunch. "
                + "Dictation pauses for a few seconds."
            alert.addButton(withTitle: "Install & Relaunch")
            alert.addButton(withTitle: "Cancel")
        } else {
            // The daemon refused in advance (`can_apply` false): say what to do instead.
            alert.messageText = "Always v\(offer.latest) is available"
            alert.informativeText =
                offer.applyHint.isEmpty
                ? "This install can't be updated from the app." : offer.applyHint
            if offer.releaseURL != nil { alert.addButton(withTitle: "Open Release Page") }
            alert.addButton(withTitle: "Close")
        }

        NSApp.activate(ignoringOtherApps: true)
        let response = alert.runModal()
        guard response == .alertFirstButtonReturn else { return }

        if offer.canApply {
            UpdateService.shared.installNow()
        } else if let url = offer.releaseURL {
            NSWorkspace.shared.open(url)
        }
    }
}
