import Foundation
import Sparkle
import Combine
import os.log

/// Wraps a Sparkle `SPUUpdater` with a custom `SPUUserDriver` that
/// publishes state to SwiftUI instead of showing Sparkle's standard
/// update window. This gives an inline update checker in the About
/// panel — status text, progress bar, install button — inspired by
/// Handy's Tauri-based update checker.
///
/// State machine:
///   idle → checking → (updateAvailable | upToDate | error)
///   updateAvailable → downloading → extracting → readyToInstall → installing → installed
///
/// Requires `SUFeedURL` + `SUPublicEDKey` keys in Info.plist.
final class UpdateService: ObservableObject {
    static let shared = UpdateService()

    private let logger = Logger(subsystem: "com.always.app", category: "updates")
    private let driver: InlineUpdateDriver?
    private let updater: SPUUpdater?

    @Published var canCheckForUpdates: Bool = false
    @Published private(set) var checkState: UpdateCheckState = .idle

    private var stateCancellable: AnyCancellable?

    private init() {
        let host = Bundle.main
        guard Self.hasUsablePublicKey(in: host) else {
            self.driver = nil
            self.updater = nil
            logger.warning("Sparkle disabled because SUPublicEDKey is missing or still uses the placeholder")
            return
        }

        let driver = InlineUpdateDriver()
        self.driver = driver
        let updater = SPUUpdater(
            hostBundle: host,
            applicationBundle: host,
            userDriver: driver,
            delegate: nil
        )
        self.updater = updater

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

        // Forward driver state to our published property.
        driver.$state
            .receive(on: DispatchQueue.main)
            .assign(to: &$checkState)
    }

    /// User-initiated check ("Check for Updates…" menu item or About button).
    func checkForUpdates() {
        guard let updater else {
            logger.warning("Sparkle update check skipped because updater is not configured")
            return
        }
        updater.checkForUpdates()
    }

    /// Install the currently-available update (triggers download if not
    /// yet downloaded, then install + relaunch).
    func installUpdate() {
        driver?.installUpdate()
    }

    /// Dismiss the current update notification (user declined).
    func dismissUpdate() {
        driver?.dismissUpdate()
    }

    private static func hasUsablePublicKey(in bundle: Bundle) -> Bool {
        guard let key = bundle.object(forInfoDictionaryKey: "SUPublicEDKey") as? String else {
            return false
        }
        let trimmed = key.trimmingCharacters(in: .whitespacesAndNewlines)
        return !trimmed.isEmpty && trimmed != "REPLACE_WITH_BASE64_EDDSA_PUBLIC_KEY"
    }

    private var feedDescription: String {
        let url = Bundle.main.object(forInfoDictionaryKey: "SUFeedURL") as? String
        return url ?? "<no SUFeedURL configured>"
    }
}

// MARK: - Inline update state

/// Published state for the inline update checker UI.
enum UpdateCheckState: Equatable {
    case idle
    case checking
    case upToDate
    case updateAvailable(version: String, releaseNotes: String?)
    case downloading(progress: Double)       // 0.0–1.0
    case extracting(progress: Double)         // 0.0–1.0
    case readyToInstall
    case installing
    case installed
    case error(String)
}

// MARK: - Custom SPUUserDriver

/// Implements Sparkle's `SPUUserDriver` protocol to publish update
/// state to SwiftUI instead of showing a separate window. Every method
/// is called on the main thread per the protocol contract.
private final class InlineUpdateDriver: NSObject, SPUUserDriver, ObservableObject {

    @Published private(set) var state: UpdateCheckState = .idle

    // Closures stored to call back into Sparkle at the right moment.
    private var updateFoundReply: ((SPUUserUpdateChoice) -> Void)?
    private var readyToInstallReply: ((SPUUserUpdateChoice) -> Void)?
    private var downloadCancellation: (() -> Void)?
    private var checkCancellation: (() -> Void)?

    // Track download bytes for progress.
    private var expectedContentLength: UInt64 = 0
    private var downloadedBytes: UInt64 = 0

    // MARK: - Permission request (one-time, auto-allow)

    func show(
        _ request: SPUUpdatePermissionRequest,
        reply: @escaping (SUUpdatePermissionResponse) -> Void
    ) {
        // Auto-allow automatic update checks — the user can disable in
        // Settings if they want. No prompt needed.
        let response = SUUpdatePermissionResponse(automaticUpdateChecks: true, sendSystemProfile: false)
        reply(response)
    }

    // MARK: - Update check

    func showUserInitiatedUpdateCheck(cancellation: @escaping () -> Void) {
        checkCancellation = cancellation
        DispatchQueue.main.async { self.state = .checking }
    }

    func showUpdateNotFoundWithError(
        _ error: Error,
        acknowledgement: @escaping () -> Void
    ) {
        checkCancellation = nil
        DispatchQueue.main.async { self.state = .upToDate }
        acknowledgement()
    }

    func showUpdaterError(
        _ error: Error,
        acknowledgement: @escaping () -> Void
    ) {
        checkCancellation = nil
        DispatchQueue.main.async {
            self.state = .error(error.localizedDescription)
        }
        acknowledgement()
    }

    // MARK: - Update found

    func showUpdateFound(
        with appcastItem: SUAppcastItem,
        state: SPUUserUpdateState,
        reply: @escaping (SPUUserUpdateChoice) -> Void
    ) {
        checkCancellation = nil
        let version = appcastItem.versionString
        let notes = appcastItem.itemDescription

        // Store the reply — the UI calls installUpdate() or dismissUpdate()
        // which invokes it with the right choice.
        updateFoundReply = reply

        DispatchQueue.main.async {
            self.state = .updateAvailable(version: version, releaseNotes: notes)
        }
    }

    func showUpdateReleaseNotes(with downloadData: SPUDownloadData) {
        // Could publish release notes HTML here if we wanted a notes view.
        // For now, the appcast item description is already shown.
    }

    func showUpdateReleaseNotesFailedToDownloadWithError(_ error: Error) {
        // Non-critical — the version string is still shown.
    }

    // MARK: - Download

    func showDownloadInitiated(cancellation: @escaping () -> Void) {
        downloadCancellation = cancellation
        downloadedBytes = 0
        expectedContentLength = 0
        DispatchQueue.main.async { self.state = .downloading(progress: 0) }
    }

    func showDownloadDidReceiveExpectedContentLength(_ expectedContentLength: UInt64) {
        self.expectedContentLength = expectedContentLength
    }

    func showDownloadDidReceiveData(ofLength length: UInt64) {
        downloadedBytes += length
        let progress: Double = expectedContentLength > 0
            ? Double(downloadedBytes) / Double(expectedContentLength)
            : 0
        DispatchQueue.main.async { self.state = .downloading(progress: progress) }
    }

    func showDownloadDidStartExtractingUpdate() {
        downloadCancellation = nil
        DispatchQueue.main.async { self.state = .extracting(progress: 0) }
    }

    func showExtractionReceivedProgress(_ progress: Double) {
        DispatchQueue.main.async { self.state = .extracting(progress: progress) }
    }

    // MARK: - Install

    func showReady(toInstallAndRelaunch reply: @escaping (SPUUserUpdateChoice) -> Void) {
        readyToInstallReply = reply
        DispatchQueue.main.async { self.state = .readyToInstall }
    }

    func showInstallingUpdate(
        withApplicationTerminated applicationTerminated: Bool,
        retryTerminatingApplication: @escaping () -> Void
    ) {
        DispatchQueue.main.async { self.state = .installing }
    }

    func showUpdateInstalledAndRelaunched(
        _ relaunched: Bool,
        acknowledgement: @escaping () -> Void
    ) {
        DispatchQueue.main.async { self.state = .installed }
        acknowledgement()
    }

    func dismissUpdateInstallation() {
        updateFoundReply = nil
        readyToInstallReply = nil
        downloadCancellation = nil
        checkCancellation = nil
        DispatchQueue.main.async { self.state = .idle }
    }

    // MARK: - User actions (called from SwiftUI)

    func installUpdate() {
        // If we have a pending "update found" reply, accept it.
        if let reply = updateFoundReply {
            updateFoundReply = nil
            reply(.install)
            return
        }
        // If we have a pending "ready to install" reply, accept it.
        if let reply = readyToInstallReply {
            readyToInstallReply = nil
            reply(.install)
            return
        }
    }

    func dismissUpdate() {
        if let reply = updateFoundReply {
            updateFoundReply = nil
            reply(.dismiss)
        }
        if let reply = readyToInstallReply {
            readyToInstallReply = nil
            reply(.dismiss)
        }
        DispatchQueue.main.async { self.state = .idle }
    }
}
