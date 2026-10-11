import Foundation
import AppKit
import ApplicationServices
import AVFoundation
import Combine
import CoreGraphics
import IOKit
import IOKit.hid
import os.log

/// Tracks the three macOS TCC permissions Always actually needs:
///   * Microphone — for the SoX `rec` subprocess that feeds the VAD.
///   * Accessibility — for the synthetic Cmd+V the daemon posts after
///     a transcript is ready, and for the overlay's text-cursor query.
///   * Input Monitoring — for the global keyboard shortcuts (pause,
///     auto-enter, force-paste) and the daemon's event tap.
///
/// Pattern mirrors Handy: surface the status as a non-modal banner in
/// Settings, let the user click through to System Settings, re-check on
/// app focus so the banner clears the moment permission flips to granted.
/// No modal alert spam.
final class PermissionsManager: ObservableObject {
    static let shared = PermissionsManager()

    @Published private(set) var micStatus: MicStatus = .notDetermined
    @Published private(set) var accessibilityStatus: AccessibilityStatus = .notTrusted
    @Published private(set) var inputMonitoringStatus: InputMonitoringStatus = .notGranted
    /// Ground truth from the DAEMON about its shortcut event tap
    /// (`ShortcutListenerStatus` UDS event). `nil` until the daemon
    /// reports. Distinct from `inputMonitoringStatus`, which reflects the
    /// GUI process's own TCC check — the two can disagree when the grant
    /// is keyed to a different responsible process.
    @Published private(set) var daemonShortcutsGranted: Bool?

    /// When the user just navigated away to grant a permission via
    /// `openSystemSettings`, we keep track of which one so we can
    /// skip a redundant poll after the system-settings panel appears.
    /// (System Settings briefly steals focus, which would trigger an
    /// immediate `didBecomeActive` check.)
    private var pendingGrantPermission: Permission?

    enum MicStatus: Equatable, CustomStringConvertible {
        case granted
        case denied
        case restricted
        case notDetermined
        var isOK: Bool { self == .granted }
        var description: String {
            switch self {
            case .granted: return "granted"
            case .denied: return "denied"
            case .restricted: return "restricted"
            case .notDetermined: return "notDetermined"
            }
        }
    }

    enum AccessibilityStatus: Equatable, CustomStringConvertible {
        case trusted
        case notTrusted
        var isOK: Bool { self == .trusted }
        var description: String {
            switch self {
            case .trusted: return "trusted"
            case .notTrusted: return "notTrusted"
            }
        }
    }

    enum InputMonitoringStatus: Equatable, CustomStringConvertible {
        case granted
        case notGranted
        var isOK: Bool { self == .granted }
        var description: String {
            switch self {
            case .granted: return "granted"
            case .notGranted: return "notGranted"
            }
        }
    }

    private let logger = Logger(subsystem: "com.always.app", category: "permissions")
    private var pollTimer: Timer?

    private init() {
        refresh()
        startFocusRefresh()
    }

    /// Re-read all permission statuses from the OS. Cheap; safe to
    /// call on every app activation. Does NOT trigger any system
    /// prompt — call `requestMicrophoneIfNeeded` /
    /// `requestAccessibilityIfNeeded` / `requestInputMonitoringIfNeeded`
    /// for that.
    func refresh() {
        let mic = AVCaptureDevice.authorizationStatus(for: .audio)
        let newMic: MicStatus
        switch mic {
        case .authorized:    newMic = .granted
        case .denied:        newMic = .denied
        case .restricted:    newMic = .restricted
        case .notDetermined: newMic = .notDetermined
        @unknown default:    newMic = .notDetermined
        }
        let newAccess: AccessibilityStatus = AXIsProcessTrusted() ? .trusted : .notTrusted
        // Input Monitoring (kTCCServiceListenEvent) — gates the daemon's
        // listen-only global-hotkey CGEventTap. Preflight is a cheap status
        // read that never prompts.
        let newInputMon: InputMonitoringStatus =
            IOHIDCheckAccess(kIOHIDRequestTypeListenEvent) == kIOHIDAccessTypeGranted
            ? .granted : .notGranted
        DispatchQueue.main.async { [weak self] in
            guard let self else { return }
            let changed = self.micStatus != newMic
                || self.accessibilityStatus != newAccess
                || self.inputMonitoringStatus != newInputMon
            if changed {
                self.logger.info("permissions: mic \(self.micStatus)→\(newMic, privacy: .public) acc \(self.accessibilityStatus)→\(newAccess, privacy: .public) inm \(self.inputMonitoringStatus)→\(newInputMon, privacy: .public)")
            }
            self.micStatus = newMic
            self.accessibilityStatus = newAccess
            self.inputMonitoringStatus = newInputMon
        }
    }

    /// Fire the system TCC prompt for microphone. No-op if already
    /// granted/denied/restricted (those are terminal — only
    /// `notDetermined` produces a prompt).
    func requestMicrophoneIfNeeded() {
        guard micStatus == .notDetermined else { return }
        AVCaptureDevice.requestAccess(for: .audio) { [weak self] _ in
            self?.refresh()
        }
    }

    /// Direct the user to the System Settings accessibility pane.
    ///
    /// `AXIsProcessTrustedWithOptions` with the prompt option requires
    /// the calling process to already have accessibility to surface the
    /// system dialog — which is exactly the chicken-and-egg problem we
    /// are debugging here. Instead, we skip the broken prompt path and
    /// go straight to System Settings where the user can enable Always.
    /// Returns true if the app already has accessibility (in which case
    /// there is nothing to do).
    @discardableResult
    func requestAccessibilityIfNeeded() -> Bool {
        if AXIsProcessTrusted() {
            accessibilityStatus = .trusted
            return true
        }
        // Not trusted — open System Settings so the user can toggle
        // Always ON. The banner will clear on the next refresh (which
        // fires on didBecomeActive or the 2s poll).
        openSystemSettings(for: .accessibility)
        return false
    }

    /// Add "Always" to the Input Monitoring list and surface the system
    /// setting pane so the user can enable it.
    ///
    /// The daemon's global-hotkey listener is a *listen-only*
    /// `CGEventTap`, which macOS gates behind Input Monitoring — NOT
    /// Accessibility. The daemon is spawned as a direct child of this
    /// app, so its requirement is attributed to the responsible process:
    /// "Always". Requesting from the GUI (which has a real bundle +
    /// windowserver session) reliably surfaces "Always" in System
    /// Settings → Privacy & Security → Input Monitoring and shows the
    /// one-time prompt; a faceless helper's own request does not.
    /// Once the user enables it here, the daemon's tap is authorized.
    /// Must be re-granted after a bundle-id change (TCC is keyed to
    /// bundle id).
    @discardableResult
    func requestInputMonitoringIfNeeded() -> Bool {
        if IOHIDCheckAccess(kIOHIDRequestTypeListenEvent) == kIOHIDAccessTypeGranted {
            return true
        }
        // Register "Always" in System Settings → Privacy & Security →
        // Input Monitoring. This adds the app entry and fires the one-
        // time prompt (which may or may not appear on macOS 26). Either
        // way, we then deep-link so the user can toggle it themselves.
        IOHIDRequestAccess(kIOHIDRequestTypeListenEvent)
        // Mark so the next didBecomeActive skips the poll (system
        // Settings briefly steals focus, which would trigger a check
        // before the user has had a chance to toggle the switch).
        pendingGrantPermission = .inputMonitoring
        openSystemSettings(for: .inputMonitoring)
        return false
    }

    /// Record the daemon's reported tap status (called by StateMonitor on
    /// every `ShortcutListenerStatus` event).
    func updateDaemonShortcutStatus(granted: Bool) {
        DispatchQueue.main.async {
            if self.daemonShortcutsGranted != granted {
                self.logger.info("daemon shortcut tap granted=\(granted, privacy: .public)")
                self.daemonShortcutsGranted = granted
            }
        }
    }

    /// Deep-link into System Settings → Privacy & Security pane for a
    /// specific permission. `nil` falls back to the root Privacy pane.
    func openSystemSettings(for permission: Permission) {
        let url: URL? = {
            switch permission {
            case .microphone:
                return URL(
                    string: "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone"
                )
            case .accessibility:
                return URL(
                    string: "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"
                )
            case .inputMonitoring:
                return URL(
                    string: "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent"
                )
            }
        }()
        if let url {
            NSWorkspace.shared.open(url)
        }
    }

    enum Permission { case microphone, accessibility, inputMonitoring }

    /// Re-check status whenever the app comes to the foreground —
    /// this is the moment the user has likely just toggled a switch
    /// in System Settings.
    ///
    /// When the user just navigated away to grant a permission via
    /// `requestInputMonitoringIfNeeded` / `requestAccessibilityIfNeeded`,
    /// we wait 1.5 seconds before refreshing. That gives the system
    /// settings panel time to appear (which steals focus and would
    /// trigger an immediate, premature check that sees the old state).
    private func startFocusRefresh() {
        NotificationCenter.default.addObserver(
            self,
            selector: #selector(handleAppActivate),
            name: NSApplication.didBecomeActiveNotification,
            object: nil
        )
        // Light fallback poll every 2s during the first 30s after
        // launch so we catch a grant even if the user doesn't switch
        // focus back to Always. Stops itself when all are granted.
        pollTimer = Timer.scheduledTimer(withTimeInterval: 2.0, repeats: true) { [weak self] timer in
            guard let self else { timer.invalidate(); return }
            self.refresh()
            if self.micStatus.isOK
                && self.accessibilityStatus.isOK
                && self.inputMonitoringStatus.isOK
                && (self.daemonShortcutsGranted == nil || self.daemonShortcutsGranted == true)
            {
                timer.invalidate()
                self.pollTimer = nil
            }
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + 30) { [weak self] in
            self?.pollTimer?.invalidate()
            self?.pollTimer = nil
        }
    }

    @objc private func handleAppActivate() {
        // If the user just navigated away to grant a specific permission,
        // wait a beat before checking — System Settings briefly stole
        // focus and the grant hasn't taken effect yet.
            if pendingGrantPermission != nil {
                pendingGrantPermission = nil
                DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) { [weak self] in
                    self?.refresh()
                }
            } else {
                refresh()
            }
    }
}
