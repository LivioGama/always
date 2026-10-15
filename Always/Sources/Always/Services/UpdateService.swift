import AppKit
import Combine
import Foundation
import os.log

/// Thin UI layer over the daemon's updater (SPEC §12a). The daemon checks
/// GitHub Releases, verifies the minisign signatures and runs the installer;
/// this service only forwards the user's two requests (`CheckForUpdates`,
/// `InstallUpdate`) and renders the daemon's answers as one `UpdateState`.
///
/// Same shape as `VoiceEnrollmentClient`: a global singleton that subscribes
/// to `Notification.Name.daemonEvent` and sends commands through
/// `StateMonitor.shared`. It is touched at launch so the `UpdateAvailable`
/// frame in the daemon's connect burst is never missed.
@MainActor
final class UpdateService: ObservableObject {
    static let shared = UpdateService()

    /// `DaemonCommand` unit variants (`src/always/event.rs`), sent as
    /// `{"type":"<name>"}` — the only two update commands there are.
    private enum Command {
        static let check = "CheckForUpdates"
        static let install = "InstallUpdate"
    }

    /// A check normally answers in well under a second.
    private static let checkTimeout: TimeInterval = 20
    /// How long the installer gets to quit and replace this app before the
    /// GUI assumes it failed and resumes normal behaviour.
    private static let handoffTimeout: TimeInterval = 120

    private let logger = Logger(subsystem: "com.always.app", category: "updates")

    @Published private(set) var model = UpdateModel()
    @Published private(set) var isDaemonConnected = false

    var state: UpdateState { model.state }
    /// Newest known release newer than this build, if any.
    var offer: UpdateOffer? { model.offer }

    private var observer: NSObjectProtocol?
    private var cancellables = Set<AnyCancellable>()
    private var checkTimeoutTask: Task<Void, Never>?
    private var handoffTimeoutTask: Task<Void, Never>?

    private init() {
        observer = NotificationCenter.default.addObserver(
            forName: .daemonEvent,
            object: nil,
            queue: .main
        ) { [weak self] note in
            guard let event = note.object as? DaemonEvent else { return }
            MainActor.assumeIsolated {
                self?.handle(event)
            }
        }

        StateMonitor.shared.$isDaemonConnected
            .removeDuplicates()
            .receive(on: DispatchQueue.main)
            .sink { [weak self] connected in
                self?.connectionChanged(connected)
            }
            .store(in: &cancellables)
    }

    // MARK: Derived UI state

    private var isBusy: Bool {
        switch state {
        case .checking, .installing: return true
        default: return false
        }
    }

    var canCheck: Bool { isDaemonConnected && !isBusy }

    /// Install is possible only when the daemon said so (`can_apply`).
    var canInstall: Bool {
        guard isDaemonConnected, offer?.canApply == true else { return false }
        switch state {
        case .available, .failed: return true
        default: return false
        }
    }

    // MARK: Commands (UI → daemon)

    /// "Check now": runs one GitHub check even when the background check is
    /// switched off (`update_check=false`) — the user asked.
    func checkNow() {
        guard canCheck else { return }
        model.beginCheck()
        StateMonitor.shared.sendCommand(Command.check)
        armCheckTimeout()
    }

    /// "Install update": the daemon downloads, verifies and starts the
    /// installer, answering with `UpdateProgress` / `UpdateFailed`.
    func installNow() {
        guard canInstall, model.beginInstall() else { return }
        StateMonitor.shared.sendCommand(Command.install)
    }

    // MARK: Events (daemon → UI)

    private func handle(_ daemonEvent: DaemonEvent) {
        guard let event = UpdateEvent(daemonEvent) else { return }
        logger.info("update event: \(daemonEvent.type.rawValue, privacy: .public)")
        let before = model.state
        model.apply(event)
        stateChanged(from: before)
    }

    private func connectionChanged(_ connected: Bool) {
        isDaemonConnected = connected
        guard !connected else { return }
        let before = model.state
        model.daemonDisconnected()
        stateChanged(from: before)
    }

    private func stateChanged(from before: UpdateState) {
        let after = model.state
        if after != .checking { checkTimeoutTask?.cancel() }

        let wasHandoff = before == .installing(.handoff)
        let isHandoff = after == .installing(.handoff)
        if isHandoff && !wasHandoff { beginHandoff() }
        if wasHandoff && !isHandoff { endHandoff() }
    }

    // MARK: Installer handoff

    /// The verified installer is about to quit this app. Let that quit go
    /// through (`applicationShouldTerminate` refuses background quits with no
    /// window) so the app tears down cleanly, and keep the watchdog from
    /// respawning the daemon the installer is deliberately stopping.
    private func beginHandoff() {
        logger.info("installer started — expecting this app and the daemon to quit")
        AppDelegate.userInitiatedQuit = true
        StateMonitor.shared.suppressDaemonRespawn(for: Self.handoffTimeout)
        handoffTimeoutTask?.cancel()
        handoffTimeoutTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: UInt64(Self.handoffTimeout * 1_000_000_000))
            guard !Task.isCancelled else { return }
            self?.handoffExpired()
        }
    }

    private func handoffExpired() {
        logger.error("installer did not replace the app within \(Self.handoffTimeout, privacy: .public)s")
        let before = model.state
        model.handoffTimedOut()
        stateChanged(from: before)
    }

    private func endHandoff() {
        handoffTimeoutTask?.cancel()
        handoffTimeoutTask = nil
        AppDelegate.userInitiatedQuit = false
        StateMonitor.shared.resumeDaemonRespawn()
    }

    private func armCheckTimeout() {
        checkTimeoutTask?.cancel()
        checkTimeoutTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: UInt64(Self.checkTimeout * 1_000_000_000))
            guard !Task.isCancelled, let self else { return }
            let before = self.model.state
            self.model.checkTimedOut()
            self.stateChanged(from: before)
        }
    }
}
