import AppKit
import Foundation
import os.log

/// Watches `NSWorkspace.didActivateApplicationNotification` and forwards
/// the active app's bundle identifier to the daemon. The daemon stores
/// this as the current-app key and consults its per-app overrides at
/// each decision point (pause, auto-enter, delay).
final class FocusedAppMonitor {
    static let shared = FocusedAppMonitor()

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
        // is correct from launch.
        if let bundle = NSWorkspace.shared.frontmostApplication?.bundleIdentifier {
            self.notify(bundleID: bundle)
        }
        observer = NSWorkspace.shared.notificationCenter.addObserver(
            forName: NSWorkspace.didActivateApplicationNotification,
            object: nil,
            queue: .main
        ) { [weak self] note in
            guard let self = self else { return }
            let app = note.userInfo?[NSWorkspace.applicationUserInfoKey] as? NSRunningApplication
            self.notify(bundleID: app?.bundleIdentifier)
        }
    }

    private func notify(bundleID: String?) {
        if bundleID == lastBundleID { return }
        lastBundleID = bundleID
        logger.info("focused app -> \(bundleID ?? "nil")")
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
