import Foundation
import Sparkle
import os.log

/// Wraps a Sparkle `SPUUpdater` so the rest of the app only sees a single
/// "check now" affordance and a published "can check" boolean. The release
/// workflow generates and uploads `appcast.xml` next to the signed DMG;
/// this service polls it on the schedule configured in `Info.plist`
/// (`SUScheduledCheckInterval`).
///
/// Note: requires the `SUFeedURL` + `SUPublicEDKey` keys in Info.plist.
///
/// Why a custom user driver: when the appcast is unreachable (e.g. the
/// very first signed release before `appcast.xml` exists at the feed URL),
/// Sparkle's standard driver pops an "Unable to check for updates" alert.
/// We suppress that single failure mode by overriding `showUpdaterError`
/// — every other interaction (update found, download progress, install
/// prompt) is left to the stock UI.
final class UpdateService: ObservableObject {
    static let shared = UpdateService()

    private let logger = Logger(subsystem: "com.always.app", category: "updates")
    private let userDriver: SilentErrorUserDriver
    private let updater: SPUUpdater

    /// Convenience: forwards Sparkle's `canCheckForUpdates` flag.
    @Published var canCheckForUpdates: Bool = false

    private init() {
        let host = Bundle.main
        let driver = SilentErrorUserDriver(hostBundle: host, delegate: nil)
        self.userDriver = driver
        self.updater = SPUUpdater(
            hostBundle: host,
            applicationBundle: host,
            userDriver: driver,
            delegate: nil
        )

        do {
            try updater.start()
            logger.info("Sparkle updater started; feed=\(self.feedDescription, privacy: .public)")
        } catch {
            logger.error("Sparkle start failed: \(error.localizedDescription, privacy: .public)")
        }

        self.canCheckForUpdates = updater.canCheckForUpdates
        updater.publisher(for: \.canCheckForUpdates)
            .receive(on: DispatchQueue.main)
            .assign(to: &$canCheckForUpdates)
    }

    /// User-initiated check ("Check for Updates…" menu item).
    func checkForUpdates() {
        updater.checkForUpdates()
    }

    private var feedDescription: String {
        // Reading `feedURL` is private API; rely on Info.plist key for
        // human-visible diagnostics instead.
        let url = Bundle.main.object(forInfoDictionaryKey: "SUFeedURL") as? String
        return url ?? "<no SUFeedURL configured>"
    }
}

/// Drop-in replacement for `SPUStandardUserDriver` that swallows the
/// "Unable to check for updates" alert. The error is logged so it stays
/// diagnosable, but the user-facing modal is skipped — appropriate for
/// the first signed release where `appcast.xml` may not yet exist at the
/// feed URL.
private final class SilentErrorUserDriver: SPUStandardUserDriver {
    private let logger = Logger(subsystem: "com.always.app", category: "updates")

    override func showUpdaterError(_ error: Error) async {
        logger.error("update error suppressed: \(error.localizedDescription, privacy: .public)")
    }
}
