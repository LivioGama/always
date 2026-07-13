import Foundation
import AppKit
import ApplicationServices
import AVFoundation
import Combine
import CoreGraphics
import IOKit
import IOKit.hid
import os.log

/// Tracks the two macOS TCC permissions Always actually needs:
///   * Microphone — for the SoX `rec` subprocess that feeds the VAD.
///   * Accessibility — for the synthetic Cmd+V the daemon posts after
///     a transcript is ready, and for the overlay's text-cursor query.
///
/// Pattern is borrowed from Handy: surface the status as a non-modal
/// banner in Settings, let the user click through to System Settings,
/// re-check on app focus so the banner clears the moment permission
/// flips to granted. No modal alert spam.
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

    enum MicStatus: Equatable {
        case granted
        case denied
        case restricted
        case notDetermined
        var isOK: Bool { self == .granted }
    }

    enum AccessibilityStatus: Equatable {
        case trusted
        case notTrusted
        var isOK: Bool { self == .trusted }
    }

    enum InputMonitoringStatus: Equatable {
        case granted
        case notGranted
        var isOK: Bool { self == .granted }
    }

    private let logger = Logger(subsystem: "com.always.app", category: "permissions")
    private var pollTimer: Timer?

    private init() {
        refresh()
        startFocusRefresh()
    }

    /// Re-read both permission statuses from the OS. Cheap; safe to
    /// call on every app activation. Does NOT trigger any system
    /// prompt — call `requestMicrophoneIfNeeded` /
    /// `requestAccessibilityIfNeeded` for that.
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
        DispatchQueue.main.async {
            if self.micStatus != newMic {
                self.logger.info("mic permission \(String(describing: self.micStatus)) → \(String(describing: newMic), privacy: .public)")
                self.micStatus = newMic
            }
            if self.accessibilityStatus != newAccess {
                self.logger.info("accessibility permission \(String(describing: self.accessibilityStatus)) → \(String(describing: newAccess), privacy: .public)")
                self.accessibilityStatus = newAccess
            }
            if self.inputMonitoringStatus != newInputMon {
                self.logger.info("input-monitoring permission \(String(describing: self.inputMonitoringStatus)) → \(String(describing: newInputMon), privacy: .public)")
                self.inputMonitoringStatus = newInputMon
            }
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

    /// Fire the system Accessibility prompt. Unlike mic, this is
    /// always safe to call — the OS dedupes silently. Returns true if
    /// permission is already trusted.
    @discardableResult
    func requestAccessibilityIfNeeded() -> Bool {
        if AXIsProcessTrusted() {
            accessibilityStatus = .trusted
            return true
        }
        let key = kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String
        let options = [key: true] as CFDictionary
        let trusted = AXIsProcessTrustedWithOptions(options)
        accessibilityStatus = trusted ? .trusted : .notTrusted
        return trusted
    }

    /// Fire the system Input Monitoring (kTCCServiceListenEvent) prompt.
    ///
    /// The daemon's global-hotkey listener is a *listen-only* `CGEventTap`,
    /// which macOS gates behind Input Monitoring — NOT Accessibility. The
    /// daemon is spawned as a direct child of this app, so its requirement is
    /// attributed to the responsible process: "Always". Requesting from the GUI
    /// (which has a real bundle + windowserver session) reliably surfaces
    /// "Always" in System Settings → Privacy & Security → Input Monitoring and
    /// shows the one-time prompt; a faceless helper's own request does not. Once
    /// the user enables it here, the daemon's tap is authorized. Must be
    /// re-granted after a bundle-id change (TCC is keyed to bundle id).
    @discardableResult
    func requestInputMonitoringIfNeeded() -> Bool {
        if IOHIDCheckAccess(kIOHIDRequestTypeListenEvent) == kIOHIDAccessTypeGranted {
            return true
        }
        // Registers "Always" in System Settings → Privacy & Security → Input
        // Monitoring and fires the one-time prompt. This is the API working
        // input-monitoring apps use; `CGRequestListenEventAccess` does not
        // surface the entry on macOS 26.
        return IOHIDRequestAccess(kIOHIDRequestTypeListenEvent)
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
                return URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone")
            case .accessibility:
                return URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
            case .inputMonitoring:
                return URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent")
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
    private func startFocusRefresh() {
        NotificationCenter.default.addObserver(
            self,
            selector: #selector(handleAppActivate),
            name: NSApplication.didBecomeActiveNotification,
            object: nil
        )
        // Light fallback poll every 2s during the first 30s after
        // launch so we catch a grant even if the user doesn't switch
        // focus back to Always. Stops itself when both are granted.
        pollTimer = Timer.scheduledTimer(withTimeInterval: 2.0, repeats: true) { [weak self] timer in
            guard let self else { timer.invalidate(); return }
            self.refresh()
            if self.micStatus.isOK && self.accessibilityStatus.isOK && self.inputMonitoringStatus.isOK {
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
        refresh()
    }
}
