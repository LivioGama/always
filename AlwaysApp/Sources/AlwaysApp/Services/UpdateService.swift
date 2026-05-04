import Foundation
import Sparkle
import os.log

/// Wraps Sparkle's `SPUStandardUpdaterController` so the rest of the app
/// only sees a single "check now" affordance and a published "update
/// available" boolean. The release workflow generates and uploads
/// `appcast.xml` next to the signed DMG; this service polls it on the
/// schedule configured in `Info.plist` (`SUScheduledCheckInterval`).
///
/// Note: requires the `SUFeedURL` + `SUPublicEDKey` keys in Info.plist.
/// During development those keys are absent and Sparkle initialization
/// is a no-op aside from an `os_log` warning, so this class can be
/// constructed safely even before the release pipeline is configured.
final class UpdateService: ObservableObject {
    static let shared = UpdateService()

    private let logger = Logger(subsystem: "com.always.app", category: "updates")
    private let updaterController: SPUStandardUpdaterController

    /// Convenience: forwards Sparkle's `canCheckForUpdates` flag.
    @Published var canCheckForUpdates: Bool = false

    private init() {
        // `startingUpdater: true` lets Sparkle schedule background checks
        // immediately. `userDriverDelegate` is nil — the standard UI
        // (download progress + restart prompt) is exactly what we want.
        self.updaterController = SPUStandardUpdaterController(
            startingUpdater: true,
            updaterDelegate: nil,
            userDriverDelegate: nil
        )
        // Bridge Sparkle's KVO-published flag into a SwiftUI-friendly @Published.
        self.canCheckForUpdates = updaterController.updater.canCheckForUpdates
        updaterController.updater.publisher(for: \.canCheckForUpdates)
            .receive(on: DispatchQueue.main)
            .assign(to: &$canCheckForUpdates)
        logger.info("Sparkle updater initialized; feed=\(self.feedDescription, privacy: .public)")
    }

    /// User-initiated check ("Check for Updates…" menu item).
    func checkForUpdates() {
        updaterController.checkForUpdates(nil)
    }

    private var feedDescription: String {
        // Reading `feedURL` is private API; rely on Info.plist key for
        // human-visible diagnostics instead.
        let url = Bundle.main.object(forInfoDictionaryKey: "SUFeedURL") as? String
        return url ?? "<no SUFeedURL configured>"
    }
}
