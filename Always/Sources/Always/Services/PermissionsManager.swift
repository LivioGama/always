import Foundation
import AppKit
import ApplicationServices
import AVFoundation
import Combine
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
        DispatchQueue.main.async {
            if self.micStatus != newMic {
                self.logger.info("mic permission \(String(describing: self.micStatus)) → \(String(describing: newMic), privacy: .public)")
                self.micStatus = newMic
            }
            if self.accessibilityStatus != newAccess {
                self.logger.info("accessibility permission \(String(describing: self.accessibilityStatus)) → \(String(describing: newAccess), privacy: .public)")
                self.accessibilityStatus = newAccess
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

    /// Deep-link into System Settings → Privacy & Security pane for a
    /// specific permission. `nil` falls back to the root Privacy pane.
    func openSystemSettings(for permission: Permission) {
        let url: URL? = {
            switch permission {
            case .microphone:
                return URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone")
            case .accessibility:
                return URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
            }
        }()
        if let url {
            NSWorkspace.shared.open(url)
        }
    }

    enum Permission { case microphone, accessibility }

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
            if self.micStatus.isOK && self.accessibilityStatus.isOK {
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
