import AppKit
import Combine
import Foundation
import os.log

/// Watches `NSWorkspace.didActivateApplicationNotification` and forwards
/// the active app's bundle identifier to the daemon. The daemon stores
/// this as the current-app key and consults its per-app overrides at
/// each decision point (pause, auto-enter, delay).
///
/// Also exposes the focused app's bundle id + display name as
/// `@Published` properties so the status bar icon, MenuBarView, and
/// SettingsWindow can render the per-app resume/pause controls without
/// each maintaining their own `NSWorkspace` observer.
///
/// **Self-filtering:** Always's own bundle id is suppressed from focus
/// notifications. When the user has the Settings window open the focus
/// monitor keeps reporting the *previous* (real) app — without this,
/// every click into Settings would push `com.always` to the daemon
/// and the status bar would oscillate between "paused for VS Code" and
/// "paused for Always" every time the user reads a slider value.
final class FocusedAppMonitor: ObservableObject {
    static let shared = FocusedAppMonitor()

    /// Always's own bundle id (matches `CFBundleIdentifier` in
    /// `Always/Info.plist`). Centralised so every UI surface — focus
    /// filter, allowlist filter, menu bar control — uses the same
    /// constant. If the bundle id ever changes both sides must update
    /// in lockstep or Always would appear in its own allowlist.
    static let ownBundleId = "com.always"

    @Published private(set) var currentBundleId: String?
    @Published private(set) var currentAppName: String?

    private let logger = Logger(subsystem: "com.always.app", category: "focused-app")
    private weak var stateMonitor: StateMonitor?
    private var observer: NSObjectProtocol?
    /// Suppress duplicate notifications — Cocoa can fire activation
    /// twice in quick succession (e.g. when an app brings up a sheet).
    private var lastBundleID: String?

    private init() {}

    func start(stateMonitor: StateMonitor) {
        self.stateMonitor = stateMonitor
        // Push current frontmost app immediately so the daemon's view
        // is correct from launch. The notify() filter swallows Always
        // itself — at launch the front app is usually Always (we just
        // opened Settings), so the daemon keeps an empty current_app
        // until the user clicks into another window.
        let front = NSWorkspace.shared.frontmostApplication
        if let bundle = front?.bundleIdentifier {
            self.notify(bundleID: bundle, name: front?.localizedName)
        }
        observer = NSWorkspace.shared.notificationCenter.addObserver(
            forName: NSWorkspace.didActivateApplicationNotification,
            object: nil,
            queue: .main
        ) { [weak self] note in
            guard let self = self else { return }
            let app = note.userInfo?[NSWorkspace.applicationUserInfoKey] as? NSRunningApplication
            self.notify(bundleID: app?.bundleIdentifier, name: app?.localizedName)
        }
    }

    private func notify(bundleID: String?, name: String?) {
        // Filter out Always itself — Settings interactions are not a
        // "focus change" we want the daemon (or the UI) to react to.
        // The previous real app stays as `currentBundleId` so the
        // allowlist toggle in the menu / Settings still references the
        // workspace app the user came from.
        if bundleID == Self.ownBundleId {
            logger.debug("ignoring self-focus (\(bundleID ?? "nil"))")
            return
        }
        if bundleID == lastBundleID { return }
        lastBundleID = bundleID
        logger.info("focused app -> \(bundleID ?? "nil")")
        DispatchQueue.main.async {
            self.currentBundleId = bundleID
            self.currentAppName = name
        }
        // Payload is a dictionary that mirrors the daemon's
        // `NotifyFocusedAppChanged { bundle_id: Option<String> }`.
        // Swift JSON encodes `nil` as JSON null, which Rust deserializes
        // as `None` — exactly what we want.
        stateMonitor?.sendCommandWithData(
            "NotifyFocusedAppChanged",
            ["bundle_id": bundleID]
        )
    }
}
