import SwiftUI
import AVFoundation
import ApplicationServices
import AppKit

/// Whether first-run setup has been completed.
///
/// Onboarding used to be gated on "is there a Groq API key?", which is
/// wrong in both directions: a user running a local model has no key and
/// was re-shown the welcome window on every launch forever, while a user
/// who pasted a key never saw it at all. Completion is now its own fact,
/// recorded once and never re-derived.
enum OnboardingCompletion {
    private static let key = "onboardingCompletedV1"

    static var isComplete: Bool {
        UserDefaults.standard.bool(forKey: key)
    }

    static func markComplete() {
        UserDefaults.standard.set(true, forKey: key)
        // Persist immediately: the app is a menu-bar agent that can be
        // killed without a clean terminate, and re-showing onboarding to
        // someone who finished it is the exact bug being fixed.
        UserDefaults.standard.synchronize()
    }
}

struct OnboardingView: View {
    /// Two steps, in order. Permissions first because nothing works
    /// without them; My Voice second because it needs a working mic.
    private enum Step: Int, CaseIterable {
        case permissions
        case voice

        var title: String {
            switch self {
            case .permissions: return "Permissions"
            case .voice: return "My Voice"
            }
        }
    }

    @State private var step: Step = .permissions
    @ObservedObject private var enrollment: VoiceEnrollmentClient = .shared

    @State private var saveError: String?

    // Permission state
    @State private var micStatus: AVAuthorizationStatus = AVCaptureDevice.authorizationStatus(for: .audio)
    @State private var accessibilityStatus: Bool = AXIsProcessTrusted()
    @State private var inputMonitoringGranted: Bool =
        IOHIDCheckAccess(kIOHIDRequestTypeListenEvent) == kIOHIDAccessTypeGranted

    // Periodic refresh for accessibility (no push notifications from macOS)
    @State private var permissionRefreshTimer: Timer? = nil

    @Environment(\.dismiss) private var dismiss


    /// What must be granted before setup can proceed.
    ///
    /// Microphone and Accessibility only. Input Monitoring is genuinely
    /// optional — it gates the global hotkeys, not dictation — and
    /// blocking on it would strand users who decline it in a welcome
    /// window they cannot dismiss. The API key is not required either:
    /// a local model needs no key at all.
    private var requiredPermissionsGranted: Bool {
        micStatus == .authorized && accessibilityStatus
    }

    var body: some View {
        VStack(spacing: 18) {
            header

            switch step {
            case .permissions: permissionsStep
            case .voice:       voiceStep
            }

            if let error = saveError {
                Text(error)
                    .font(.caption)
                    .foregroundColor(.red)
                    .padding(.horizontal, 20)
            }

            Spacer(minLength: 0)

            footer
        }
        .frame(width: 520, height: 560)
        .background(WindowAccessor { window in
            window?.identifier = NSUserInterfaceItemIdentifier("AlwaysOnboarding")
            window?.title = "Welcome to Always"
        })
        .onAppear {
            refreshPermissions()
            startPermissionRefreshTimer()
            enrollment.requestStatus()
        }
        .onDisappear {
            stopPermissionRefreshTimer()
        }
    }

    // MARK: - Chrome

    private var header: some View {
        VStack(spacing: 8) {
            Image(systemName: step == .permissions ? "lock.shield" : "waveform.and.mic")
                .font(.system(size: 40))
                .foregroundColor(.accentColor)

            Text(step == .permissions ? "Welcome to Always" : "Teach it your voice")
                .font(.title)
                .fontWeight(.bold)

            Text(step == .permissions
                 ? "Two quick steps. First, the permissions Always needs to work."
                 : "Optional, but it stops Always transcribing other people, videos and calls.")
                .font(.subheadline)
                .foregroundColor(.secondary)
                .multilineTextAlignment(.center)
                .padding(.horizontal, 32)

            // Step dots — cheap orientation, no back-navigation surprise.
            HStack(spacing: 6) {
                ForEach(Step.allCases, id: \.rawValue) { s in
                    Circle()
                        .fill(s == step ? Color.accentColor : Color.secondary.opacity(0.3))
                        .frame(width: 7, height: 7)
                }
            }
            .padding(.top, 2)
        }
        .padding(.top, 16)
    }

    private var footer: some View {
        HStack(spacing: 12) {
            if step == .voice {
                Button("Back") { step = .permissions }
                    .buttonStyle(.bordered)
            }

            Spacer()

            switch step {
            case .permissions:
                Button {
                    step = .voice
                } label: {
                    Text("Continue")
                        .fontWeight(.semibold)
                        .frame(minWidth: 100)
                }
                .disabled(!requiredPermissionsGranted)
                .buttonStyle(.borderedProminent)
                .controlSize(.large)

            case .voice:
                // Always finishable. Voice enrollment is genuinely
                // optional, and a welcome window that cannot be
                // dismissed is worse than one skipped too early.
                Button("Skip for now") { finish() }
                    .buttonStyle(.bordered)

                Button {
                    finish()
                } label: {
                    Text(enrollment.isEnrolled ? "Finish" : "Finish without it")
                        .fontWeight(.semibold)
                        .frame(minWidth: 100)
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
            }
        }
        .padding(.horizontal, 20)
        .padding(.bottom, 20)
    }

    // MARK: - Step 1: permissions

    private var permissionsStep: some View {
        VStack(spacing: 14) {
            permissionRow(
                title: "Microphone access",
                subtitle: "Required — this is how Always hears you",
                granted: micStatus == .authorized,
                trailing: {
                    if micStatus == .notDetermined {
                        Button("Grant") { requestMicrophone() }
                    } else if micStatus == .denied || micStatus == .restricted {
                        Button("Open Settings") { openMicrophoneSettings() }
                    }
                }
            )

            permissionRow(
                title: "Accessibility access",
                subtitle: "Required — this is how your words get typed into other apps",
                granted: accessibilityStatus,
                trailing: {
                    if !accessibilityStatus {
                        Button("Grant") { requestAccessibility() }
                        Button("Open Settings") { openAccessibilitySettings() }
                            .buttonStyle(.bordered)
                    }
                }
            )

            permissionRow(
                title: "Input Monitoring",
                subtitle: "Optional — enables the global shortcuts (⌃⌥P pause, ⌃⌥V paste)",
                granted: inputMonitoringGranted,
                trailing: {
                    if !inputMonitoringGranted {
                        Button("Grant") { requestInputMonitoring() }
                        Button("Open Settings") { openInputMonitoringSettings() }
                            .buttonStyle(.bordered)
                    }
                }
            )

            if !requiredPermissionsGranted {
                Text("macOS may need you to toggle Always on in System Settings. This window updates on its own once you do.")
                    .font(.caption)
                    .foregroundColor(.secondary)
                    .multilineTextAlignment(.center)
                    .padding(.horizontal, 28)
            }
        }
    }

    // MARK: - Step 2: My Voice

    private var voiceStep: some View {
        VStack(spacing: 12) {
            ForEach(VoiceEnrollmentClient.Step.allCases) { s in
                enrollmentRow(s)
            }

            if enrollment.isEnrolled {
                Label("All three recorded — Always will only listen to you.",
                      systemImage: "checkmark.seal.fill")
                    .font(.caption)
                    .foregroundColor(.green)
                    .padding(.top, 4)
            } else {
                Text("Record all three and Always ignores every voice but yours. You can do this later in Settings.")
                    .font(.caption)
                    .foregroundColor(.secondary)
                    .multilineTextAlignment(.center)
                    .padding(.horizontal, 28)
            }

            if let err = enrollment.lastError {
                Text(err)
                    .font(.caption)
                    .foregroundColor(.red)
                    .padding(.horizontal, 28)
            }
        }
    }

    @ViewBuilder
    private func enrollmentRow(_ s: VoiceEnrollmentClient.Step) -> some View {
        let recorded = enrollment.recordedSteps.contains(s.rawValue)
        let isRecording = enrollment.recordingStep == s
        let otherRecording = enrollment.recordingStep != nil && !isRecording

        HStack(spacing: 12) {
            Image(systemName: recorded ? "checkmark.circle.fill" : "circle")
                .foregroundColor(recorded ? .green : .secondary)
                .font(.system(size: 18))

            VStack(alignment: .leading, spacing: 2) {
                Text(s.title).font(.headline)
                Text(isRecording ? "Listening — keep talking…" : s.prompt)
                    .font(.caption)
                    .foregroundColor(.secondary)
            }

            Spacer()

            if isRecording {
                ProgressView(value: enrollment.progress)
                    .frame(width: 70)
                Button("Cancel") { enrollment.cancelRecording() }
                    .buttonStyle(.bordered)
            } else {
                Button(recorded ? "Re-record" : "Record") { enrollment.record(s) }
                    .disabled(otherRecording)
            }
        }
        .padding(.horizontal, 20)
    }

    // MARK: - Completion

    /// Record that setup is done and close. Called from both the finish
    /// and skip paths — completion is about having been through the
    /// flow, not about how much of it the user chose to fill in.
    private func finish() {
        OnboardingCompletion.markComplete()
        dismiss()
    }

    // MARK: - Reusable permission row

    @ViewBuilder
    private func permissionRow<Trailing: View>(
        title: String,
        subtitle: String,
        granted: Bool,
        @ViewBuilder trailing: () -> Trailing
    ) -> some View {
        HStack(spacing: 12) {
            Image(systemName: granted ? "checkmark.circle.fill" : "circle")
                .font(.system(size: 22))
                .foregroundColor(granted ? .green : .secondary)
            VStack(alignment: .leading, spacing: 2) {
                Text(title).font(.headline)
                Text(subtitle).font(.caption).foregroundColor(.secondary)
            }
            Spacer()
            HStack(spacing: 6) {
                trailing()
            }
        }
        .padding(.horizontal, 20)
    }

    // MARK: - Microphone

    private func requestMicrophone() {
        AVCaptureDevice.requestAccess(for: .audio) { _ in
            DispatchQueue.main.async {
                micStatus = AVCaptureDevice.authorizationStatus(for: .audio)
            }
        }
    }

    private func openMicrophoneSettings() {
        if let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone") {
            NSWorkspace.shared.open(url)
        }
    }

    // MARK: - Accessibility

    private func requestAccessibility() {
        let key = kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String
        let options: NSDictionary = [key: true]
        _ = AXIsProcessTrustedWithOptions(options)
    }

    private func openAccessibilitySettings() {
        if let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility") {
            NSWorkspace.shared.open(url)
        }
    }

    private func refreshPermissions() {
        micStatus = AVCaptureDevice.authorizationStatus(for: .audio)
        accessibilityStatus = AXIsProcessTrusted()
        inputMonitoringGranted =
            IOHIDCheckAccess(kIOHIDRequestTypeListenEvent) == kIOHIDAccessTypeGranted
    }

    // MARK: - Input Monitoring

    private func requestInputMonitoring() {
        // Same API as PermissionsManager: registers "Always" under Privacy &
        // Security → Input Monitoring and fires the one-time prompt.
        _ = IOHIDRequestAccess(kIOHIDRequestTypeListenEvent)
    }

    private func openInputMonitoringSettings() {
        if let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent") {
            NSWorkspace.shared.open(url)
        }
    }

    private func startPermissionRefreshTimer() {
        stopPermissionRefreshTimer()
        permissionRefreshTimer = Timer.scheduledTimer(withTimeInterval: 1.0, repeats: true) { _ in
            DispatchQueue.main.async {
                refreshPermissions()
            }
        }
    }

    private func stopPermissionRefreshTimer() {
        permissionRefreshTimer?.invalidate()
        permissionRefreshTimer = nil
    }


    // The Groq API key step was removed from onboarding: it is not a
    // requirement any more (local models need no key at all), and making
    // a first-run window un-dismissable until someone pastes a cloud
    // credential is hostile. The field still lives in
    // Settings → Models, so nothing was lost — it moved.
}

// MARK: - Window identity helper

struct WindowAccessor: NSViewRepresentable {
    let onResolve: (NSWindow?) -> Void

    func makeNSView(context: Context) -> NSView {
        let v = NSView()
        DispatchQueue.main.async { [weak v] in
            onResolve(v?.window)
        }
        return v
    }

    func updateNSView(_ nsView: NSView, context: Context) {}
}

#Preview {
    OnboardingView()
}
