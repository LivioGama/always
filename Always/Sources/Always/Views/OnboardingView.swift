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

    /// Reset the completion flag so onboarding shows again. Used by
    /// `forceOnboarding()` for explicit re-triggering.
    static func reset() {
        UserDefaults.standard.set(false, forKey: key)
        UserDefaults.standard.synchronize()
    }
}

struct OnboardingView: View {
    /// Four steps, in order. Welcome sets the tone; permissions are
    /// required; voice enrollment is optional; ready confirms.
    private enum Step: Int, CaseIterable {
        case welcome
        case permissions
        case voice
        case tips
        case ready

        var title: String {
            switch self {
            case .welcome:     return "Welcome"
            case .permissions: return "Permissions"
            case .voice:       return "My Voice"
            case .tips:        return "Tips"
            case .ready:       return "Ready"
            }
        }

        var narration: String {
            switch self {
            case .welcome:
                return "Welcome to Always. I'll guide you through a quick setup so I can start typing what you say. Click continue to begin."
            case .permissions:
                return "Always needs microphone access to hear you, and accessibility access to type your words into any app. Grant each permission, and I'll continue when you're ready."
            case .voice:
                return "Now, let's teach me your voice. This is optional, but it lets me ignore other people, videos, and calls. Record three short samples and I'll only listen to you."
            case .tips:
                return "Here are a few tips to get the most out of Always. Auto-enter is on by default, but holding Command prevents the text from being sent. Control Option V pastes filtered content anyway, and Control Option W lets you correct the last transcript to build your vocabulary."
            case .ready:
                return "You're all set. Tick the apps where I should type — every other app stays paused — then start speaking. Welcome to Always."
            }
        }
    }

    @State private var step: Step = .welcome
    @ObservedObject private var enrollment: VoiceEnrollmentClient = .shared
    @ObservedObject private var narrator: OnboardingNarrator = .shared
    @ObservedObject private var speech: SpeechModelSetup = .shared

    @State private var saveError: String?
    @State private var showSkipVoiceConfirmation = false

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
    ///
    /// On macOS 26+ Speech Recognition is required too: Apple's on-device
    /// engine is the default transcriber there, and without the grant
    /// every sentence fails. The model download is NOT waited for — it
    /// keeps going in the background while the user records My Voice.
    private var requiredPermissionsGranted: Bool {
        micStatus == .authorized && accessibilityStatus
            && (!speech.isOnDeviceEngineAvailable || speech.isAuthorized)
    }

    var body: some View {
        VStack(spacing: 0) {
            // Step content
            VStack(spacing: 20) {
                if step != .ready { stepIcon }
                stepTitle
                stepSubtitle
                stepContent
            }
            .frame(maxWidth: .infinity)
            .padding(.top, 36)
            .padding(.horizontal, 32)

            Spacer(minLength: 0)

            // Step indicator + footer
            stepIndicator
            footer
        }
        .frame(width: 580, height: 640)
        .background(WindowAccessor { window in
            window?.identifier = NSUserInterfaceItemIdentifier("AlwaysOnboarding")
            window?.title = "Welcome to Always"
        })
        .onAppear {
            refreshPermissions()
            startPermissionRefreshTimer()
            enrollment.requestStatus()
            // Start the on-device model download right away (it is large
            // and the user has three onboarding steps to get through).
            speech.ensureModelInstalled()
            narrator.speak(Step.welcome.narration)
            // Bypass per-app pause rules during onboarding so the user can
            // test voice typing without adding an app to the allowlist first.
            StateMonitor.shared.setSetupMode(enabled: true)
        }
        .onDisappear {
            stopPermissionRefreshTimer()
            narrator.stop()
            StateMonitor.shared.setSetupMode(enabled: false)
        }
        .onChange(of: step) { _, newStep in
            narrator.speak(newStep.narration)
        }
        .onChange(of: enrollment.isEnrolled) { _, isEnrolled in
            // Completing all three samples must leave the gate ON — the
            // step's own copy promises "Always will only listen to you",
            // and the daemon truthfully reports `enabled: false` until
            // SetVoiceProfileEnabled arrives. Leaving this to Settings
            // meant users finished setup with the gate dark.
            if isEnrolled {
                enrollment.setEnabled(true)
            }
        }
    }

    // MARK: - Step chrome

    @ViewBuilder
    private var stepIcon: some View {
        if step == .welcome {
            // Show the real app logo on the welcome screen.
            if let icon = appIconImage {
                icon
                    .resizable()
                    .interpolation(.high)
                    .aspectRatio(contentMode: .fit)
                    .frame(width: 88, height: 88)
                    .clipShape(RoundedRectangle(cornerRadius: 20))
                    .shadow(color: Color.accentColor.opacity(0.3), radius: 8, y: 4)
            } else {
                // Fallback if the icon can't be loaded.
                ZStack {
                    Circle()
                        .fill(iconCircleColor)
                        .frame(width: 72, height: 72)
                    Image(systemName: stepIconName)
                        .font(.system(size: 32, weight: .light))
                        .foregroundColor(.white)
                }
                .shadow(color: iconCircleColor.opacity(0.3), radius: 8, y: 4)
            }
        } else {
            ZStack {
                Circle()
                    .fill(iconCircleColor)
                    .frame(width: 72, height: 72)
                Image(systemName: stepIconName)
                    .font(.system(size: 32, weight: .light))
                    .foregroundColor(.white)
            }
            .shadow(color: iconCircleColor.opacity(0.3), radius: 8, y: 4)
        }
    }

    /// Load the app icon from the app bundle.
    private var appIconImage: Image? {
        if let nsImage = NSImage(named: "AlwaysIcon")
            ?? NSImage(contentsOfFile: "/Applications/Always.app/Contents/Resources/AlwaysIcon.icns") {
            return Image(nsImage: nsImage)
        }
        return nil
    }

    private var stepIconName: String {
        switch step {
        case .welcome:     return "waveform.circle.fill"
        case .permissions: return "lock.shield.fill"
        case .voice:       return "mic.badge.xmark"
        case .tips:        return "lightbulb.fill"
        case .ready:       return "checkmark.seal.fill"
        }
    }

    private var iconCircleColor: Color {
        switch step {
        case .welcome:     return .accentColor
        case .permissions: return .orange
        case .voice:       return .purple
        case .tips:        return .yellow
        case .ready:       return .green
        }
    }

    private var stepTitle: some View {
        Text(stepTitleText)
            .font(.title.bold())
            .multilineTextAlignment(.center)
    }

    private var stepTitleText: String {
        switch step {
        case .welcome:     return "Welcome to Always"
        case .permissions: return "Grant Permissions"
        case .voice:       return "Teach It Your Voice"
        case .tips:        return "Tips & Shortcuts"
        case .ready:       return "You're All Set"
        }
    }

    private var stepSubtitle: some View {
        Text(stepSubtitleText)
            .font(.subheadline)
            .foregroundColor(.secondary)
            .multilineTextAlignment(.center)
            .padding(.horizontal, 24)
    }

    private var stepSubtitleText: String {
        switch step {
        case .welcome:
            return "Always listens to your voice and types what you say — in any app. Let's get set up in three quick steps."
        case .permissions:
            return speech.isOnDeviceEngineAvailable
                ? "macOS needs you to approve three permissions. This window updates automatically once you do."
                : "macOS needs you to approve two permissions. This window updates automatically once you do."
        case .voice:
            return "Optional, but it stops Always transcribing other people, videos and calls."
        case .tips:
            return "A few shortcuts that make Always feel invisible. Memorize these — you'll use them daily."
        case .ready:
            return "Pick the apps Always types into, then start speaking. You can revisit any of this in Settings."
        }
    }

    @ViewBuilder
    private var stepContent: some View {
        switch step {
        case .welcome:     welcomeStep
        case .permissions: permissionsStep
        case .voice:       voiceStep
        case .tips:        tipsStep
        case .ready:       readyStep
        }
    }

    // MARK: - Step indicator

    private var stepIndicator: some View {
        HStack(spacing: 8) {
            ForEach(Step.allCases, id: \.rawValue) { s in
                Capsule()
                    .fill(s == step ? Color.accentColor : Color.secondary.opacity(0.25))
                    .frame(width: s == step ? 24 : 8, height: 8)
                    .animation(.easeInOut(duration: 0.25), value: step)
            }
        }
        .padding(.bottom, 16)
    }

    // MARK: - Step 0: Welcome

    private var welcomeStep: some View {
        VStack(spacing: 16) {
            // Feature highlights
            VStack(spacing: 12) {
                welcomeFeature(
                    icon: "mic.fill",
                    title: "Speak naturally",
                    detail: "Always transcribes your voice in real time."
                )
                welcomeFeature(
                    icon: "keyboard",
                    title: "Types anywhere",
                    detail: "Your words appear in whatever app is focused."
                )
                welcomeFeature(
                    icon: "person.wave.2.fill",
                    title: "Only your voice",
                    detail: "Optional voice profile ignores other speakers."
                )
            }
            .padding(.horizontal, 20)
            .padding(.top, 8)

            if narrator.isSpeaking {
                HStack(spacing: 6) {
                    Image(systemName: "speaker.wave.2.fill")
                        .font(.caption)
                        .foregroundColor(.accentColor)
                    Text("Listening to the guide…")
                        .font(.caption)
                        .foregroundColor(.secondary)
                }
                .padding(.top, 4)
            }
        }
    }

    @ViewBuilder
    private func welcomeFeature(icon: String, title: String, detail: String) -> some View {
        HStack(spacing: 14) {
            ZStack {
                Circle()
                    .fill(Color.accentColor.opacity(0.12))
                    .frame(width: 36, height: 36)
                Image(systemName: icon)
                    .font(.system(size: 16))
                    .foregroundColor(.accentColor)
            }
            VStack(alignment: .leading, spacing: 2) {
                Text(title).font(.body.weight(.medium))
                Text(detail).font(.caption).foregroundColor(.secondary)
            }
            Spacer()
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 10)
        .background(Color.secondary.opacity(0.06))
        .cornerRadius(10)
    }

    // MARK: - Step 1: Permissions

    private var permissionsStep: some View {
        // Tighter than the other steps: up to four cards plus the speech
        // model row must fit the fixed 640 pt window.
        VStack(spacing: speech.isOnDeviceEngineAvailable ? 8 : 12) {
            permissionCard(
                icon: "mic.fill",
                iconColor: .red,
                title: "Microphone",
                detail: "Required — this is how Always hears you.",
                granted: micStatus == .authorized,
                waiting: micStatus == .denied || micStatus == .restricted,
                actionTitle: micActionTitle,
                action: handleMicAction
            )

            permissionCard(
                icon: "keyboard",
                iconColor: .blue,
                title: "Accessibility",
                detail: "Required — this is how your words get typed into other apps.",
                granted: accessibilityStatus,
                waiting: false,
                actionTitle: accessibilityStatus ? nil : "Open Settings",
                action: handleAccessibilityAction
            )

            if speech.isOnDeviceEngineAvailable {
                permissionCard(
                    icon: "waveform",
                    iconColor: .purple,
                    title: "Speech Recognition",
                    detail: speechDetail,
                    granted: speech.isAuthorized,
                    waiting: false,
                    actionTitle: speech.isAuthorized ? nil
                        : (speech.authStatus == .notDetermined ? "Grant" : "Open Settings"),
                    action: handleSpeechAction
                )
            }

            permissionCard(
                icon: "command",
                iconColor: .indigo,
                title: "Input Monitoring",
                detail: "Optional — enables global shortcuts (⌃⌥P pause, ⌃⌥V paste).",
                granted: inputMonitoringGranted,
                waiting: false,
                actionTitle: inputMonitoringGranted ? nil : "Open System Settings",
                action: handleInputMonitoringAction
            )

            if speech.isOnDeviceEngineAvailable {
                speechModelRow
            }

            if !requiredPermissionsGranted {
                Text("macOS may need you to toggle Always on in System Settings. This window updates on its own once you do.")
                    .font(.caption)
                    .foregroundColor(.secondary)
                    .multilineTextAlignment(.center)
                    .padding(.horizontal, 20)
                    .padding(.top, 4)
            }
        }
        .padding(.horizontal, 20)
    }

    private var speechDetail: String {
        "Required — Always transcribes on this Mac with Apple's on-device engine. No account, no API key, nothing leaves your Mac."
    }

    /// Download state of the on-device language model (macOS 26+).
    @ViewBuilder
    private var speechModelRow: some View {
        HStack(spacing: 10) {
            switch speech.modelState {
            case .installed:
                Image(systemName: "checkmark.circle.fill").foregroundColor(.green)
                Text("Speech model ready\(speech.languageName.isEmpty ? "" : " — \(speech.languageName)")")
                    .font(.caption).foregroundColor(.secondary)
            case .downloading(let fraction):
                ProgressView(value: fraction).frame(width: 140)
                Text("Downloading speech model\(speech.languageName.isEmpty ? "" : " (\(speech.languageName))")… \(Int(fraction * 100))%")
                    .font(.caption).foregroundColor(.secondary)
            case .failed(let reason):
                Image(systemName: "exclamationmark.triangle.fill").foregroundColor(.orange)
                Text("Speech model: \(reason)")
                    .font(.caption).foregroundColor(.secondary)
                    .lineLimit(2)
                Button("Retry") { speech.retry() }.controlSize(.small)
            case .needsDictation:
                Image(systemName: "exclamationmark.triangle.fill").foregroundColor(.orange)
                Text("Turn on Dictation (System Settings → Keyboard) — this Mac's speech engine needs it.")
                    .font(.caption).foregroundColor(.secondary)
                    .lineLimit(2)
                Button("Open") { speech.openDictationSettings() }.controlSize(.small)
                Button("Retry") { speech.retry() }.controlSize(.small)
            case .checking, .unknown:
                ProgressView().controlSize(.small)
                Text("Checking speech model…").font(.caption).foregroundColor(.secondary)
            case .notApplicable:
                EmptyView()
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, 16)
    }

    @ViewBuilder
    private func permissionCard(
        icon: String,
        iconColor: Color,
        title: String,
        detail: String,
        granted: Bool,
        waiting: Bool,
        actionTitle: String?,
        action: @escaping () -> Void
    ) -> some View {
        HStack(spacing: 14) {
            ZStack {
                Circle()
                    .fill(granted ? Color.green.opacity(0.12) : iconColor.opacity(0.12))
                    .frame(width: 40, height: 40)
                Image(systemName: granted ? "checkmark" : icon)
                    .font(.system(size: 17))
                    .foregroundColor(granted ? .green : iconColor)
            }

            VStack(alignment: .leading, spacing: 3) {
                Text(title).font(.body.weight(.medium))
                Text(detail)
                    .font(.caption)
                    .foregroundColor(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }

            Spacer()

            if granted {
                Image(systemName: "checkmark.circle.fill")
                    .font(.system(size: 22))
                    .foregroundColor(.green)
            } else if let actionTitle {
                Button(actionTitle, action: action)
                    .buttonStyle(.borderedProminent)
                    .controlSize(.small)
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, speech.isOnDeviceEngineAvailable ? 10 : 14)
        .background(
            RoundedRectangle(cornerRadius: 12)
                .fill(Color.secondary.opacity(0.06))
                .overlay(
                    RoundedRectangle(cornerRadius: 12)
                        .stroke(granted ? Color.green.opacity(0.2) : Color.secondary.opacity(0.12), lineWidth: 1)
                )
        )
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
                    .padding(.horizontal, 20)
            }

            if let err = enrollment.lastError {
                Text(err)
                    .font(.caption)
                    .foregroundColor(.red)
                    .padding(.horizontal, 20)
            }
        }
        .padding(.horizontal, 20)
    }

    @ViewBuilder
    private func enrollmentRow(_ s: VoiceEnrollmentClient.Step) -> some View {
        let recorded = enrollment.recordedSteps.contains(s.rawValue)
        let isRecording = enrollment.recordingStep == s
        let otherRecording = enrollment.recordingStep != nil && !isRecording

        HStack(spacing: 14) {
            ZStack {
                Circle()
                    .fill(recorded ? Color.green.opacity(0.12) : Color.purple.opacity(0.12))
                    .frame(width: 40, height: 40)
                Image(systemName: recorded ? "checkmark" : "mic")
                    .font(.system(size: 16))
                    .foregroundColor(recorded ? .green : .purple)
            }

            VStack(alignment: .leading, spacing: 3) {
                Text(s.title).font(.body.weight(.medium))
                Text(isRecording ? "Listening — keep talking…" : s.prompt)
                    .font(.caption)
                    .foregroundColor(.secondary)
            }

            Spacer()

            if isRecording {
                ProgressView(value: enrollment.progress)
                    .frame(width: 60)
                Button("Cancel") { enrollment.cancelRecording() }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
            } else {
                Button(recorded ? "Re-record" : "Record") { enrollment.record(s) }
                    .disabled(otherRecording)
                    .controlSize(.small)
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
        .background(
            RoundedRectangle(cornerRadius: 12)
                .fill(Color.secondary.opacity(0.06))
        )
    }

    // MARK: - Step 3: Tips

    private var tipsStep: some View {
        VStack(spacing: 12) {
            tipCard(
                icon: "return",
                iconColor: .blue,
                shortcut: "⌘ Hold",
                title: "Auto-enter is on",
                detail: "Your text is typed and sent automatically. Hold ⌘ (Cmd) while speaking to prevent the final Enter — your words appear but nothing gets sent."
            )
            tipCard(
                icon: "doc.on.clipboard",
                iconColor: .orange,
                shortcut: "⌃⌥V",
                title: "Paste filtered text anyway",
                detail: "Always filters profanity and filler words. If you want the raw, unfiltered transcript pasted at your cursor, press ⌃⌥V."
            )
            tipCard(
                icon: "text.badge.checkmark",
                iconColor: .green,
                shortcut: "⌃⌥W",
                title: "Correct & feed your vocabulary",
                detail: "Said something and the transcript got it wrong? Press ⌃⌥W, type the correction, and Always learns the fix for next time — automatically adding it to your vocabulary."
            )
        }
        .padding(.horizontal, 20)
    }

    @ViewBuilder
    private func tipCard(
        icon: String,
        iconColor: Color,
        shortcut: String,
        title: String,
        detail: String
    ) -> some View {
        HStack(spacing: 14) {
            // Shortcut badge — prominent, left-most so it's the first
            // thing the eye lands on.
            Text(shortcut)
                .font(.system(size: 14, weight: .bold, design: .monospaced))
                .foregroundColor(.white)
                .padding(.horizontal, 12)
                .padding(.vertical, 6)
                .background(
                    LinearGradient(
                        colors: [iconColor, iconColor.opacity(0.7)],
                        startPoint: .top,
                        endPoint: .bottom
                    )
                )
                .clipShape(RoundedRectangle(cornerRadius: 8))
                .shadow(color: iconColor.opacity(0.3), radius: 3, y: 2)

            VStack(alignment: .leading, spacing: 3) {
                Text(title).font(.body.weight(.semibold))
                Text(detail)
                    .font(.caption)
                    .foregroundColor(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }

            Spacer()
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 14)
        .background(
            RoundedRectangle(cornerRadius: 12)
                .fill(Color.secondary.opacity(0.06))
                .overlay(
                    RoundedRectangle(cornerRadius: 12)
                        .stroke(Color.secondary.opacity(0.12), lineWidth: 1)
                )
        )
    }

    // MARK: - Step 4: Ready

    private var readyStep: some View {
        VStack(spacing: 16) {
            // App logo with success checkmark badge.
            ZStack {
                if let icon = appIconImage {
                    icon
                        .resizable()
                        .interpolation(.high)
                        .aspectRatio(contentMode: .fit)
                        .frame(width: 80, height: 80)
                        .clipShape(RoundedRectangle(cornerRadius: 18))
                        .shadow(color: Color.green.opacity(0.3), radius: 12, y: 4)
                } else {
                    Circle()
                        .fill(Color.green.opacity(0.12))
                        .frame(width: 80, height: 80)
                    Image(systemName: "checkmark.seal.fill")
                        .font(.system(size: 40))
                        .foregroundColor(.green)
                }

                // Success checkmark badge in the corner.
                Image(systemName: "checkmark.circle.fill")
                    .font(.system(size: 26))
                    .foregroundColor(.green)
                    .background(Circle().fill(Color(NSColor.windowBackgroundColor)).frame(width: 28, height: 28))
                    .offset(x: 28, y: 28)
            }
            .padding(.top, 4)

            // Where voice typing is ON. Every app is paused until it is
            // on this allowlist; setup used to end without saying so.
            OnboardingAppAllowlist()
        }
        .padding(.horizontal, 20)
    }

    // MARK: - Footer

    private var footer: some View {
        HStack(spacing: 12) {
            // Back button — hidden on welcome (first step) and ready
            // (final step — no going back from completion).
            if step != .welcome && step != .ready {
                Button("Back") {
                    step = Step(rawValue: step.rawValue - 1) ?? .welcome
                }
                .buttonStyle(.bordered)
                .controlSize(.large)
                .frame(minWidth: 120)
            }

            Spacer()

            // Mute toggle for the narrator
            if narrator.isSpeaking {
                Button {
                    narrator.stop()
                } label: {
                    Image(systemName: "speaker.slash.fill")
                }
                .buttonStyle(.borderless)
                .help("Stop voice guide")
            }

            switch step {
            case .welcome:
                Button {
                    step = .permissions
                } label: {
                    Text("Continue")
                        .fontWeight(.semibold)
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
                .frame(minWidth: 120)

            case .permissions:
                Button {
                    step = .voice
                } label: {
                    Text("Continue")
                        .fontWeight(.semibold)
                }
                .disabled(!requiredPermissionsGranted)
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
                .frame(minWidth: 120)

            case .voice:
                // Group Skip + Continue tightly on the right side.
                HStack(spacing: 8) {
                    Button("Skip for now") {
                        showSkipVoiceConfirmation = true
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.large)
                    .confirmationDialog(
                        "Are you sure?",
                        isPresented: $showSkipVoiceConfirmation,
                        titleVisibility: .visible
                    ) {
                        Button("Skip voice setup") {
                            step = .tips
                        }
                        Button("Set up my voice", role: .cancel) {}
                    } message: {
                        Text("Voice enrollment makes a big difference. It lets Always ignore other people, videos, and calls — so it only types what you say. We really recommend it.")
                    }
                    Button {
                        step = .tips
                    } label: {
                        Text(enrollment.isEnrolled ? "Continue" : "Continue without it")
                            .fontWeight(.semibold)
                    }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.large)
                }
                .fixedSize()

            case .tips:
                Button {
                    step = .ready
                } label: {
                    Text("Continue")
                        .fontWeight(.semibold)
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
                .frame(minWidth: 120)

            case .ready:
                Button {
                    finish()
                } label: {
                    Text("Start Using Always")
                        .fontWeight(.semibold)
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
                .frame(minWidth: 120)
            }
        }
        .padding(.horizontal, 24)
        .padding(.bottom, 24)
    }

    // MARK: - Completion

    /// Record that setup is done and close. Called from both the finish
    /// and skip paths — completion is about having been through the
    /// flow, not about how much of it the user chose to fill in.
    private func finish() {
        OnboardingCompletion.markComplete()
        narrator.stop()
        dismiss()
    }

    // MARK: - Permission actions

    private var micActionTitle: String? {
        switch micStatus {
        case .authorized:   return nil
        case .notDetermined: return "Grant"
        case .denied, .restricted: return "Open Settings"
        @unknown default:   return "Open Settings"
        }
    }

    private func handleMicAction() {
        if micStatus == .notDetermined {
            requestMicrophone()
        } else {
            openMicrophoneSettings()
        }
    }

    private func handleAccessibilityAction() {
        if !AXIsProcessTrusted() {
            // Register Always in the Accessibility list (and show the
            // system "would like to control this computer" sheet) BEFORE
            // opening the pane. Deep-linking alone left the list without
            // an Always entry, so a new user had to find the app with "+".
            requestAccessibility()
            openAccessibilitySettings()
        }
    }

    private func handleSpeechAction() {
        if speech.authStatus == .notDetermined {
            speech.requestAuthorization()
        } else {
            speech.openSystemSettings()
        }
    }

    private func handleInputMonitoringAction() {
        // IOHIDRequestAccess first: it adds Always to the Input Monitoring
        // list (deep-linking alone left no entry to toggle), then the pane
        // opens.
        PermissionsManager.shared.requestInputMonitoringIfNeeded()
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
        speech.refreshAuthorization()
        micStatus = AVCaptureDevice.authorizationStatus(for: .audio)
        accessibilityStatus = AXIsProcessTrusted()
        inputMonitoringGranted =
            IOHIDCheckAccess(kIOHIDRequestTypeListenEvent) == kIOHIDAccessTypeGranted
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

// MARK: - Typing demo animation

/// Animated simulation of Always transcribing speech into a text field.
/// Cycles through a few phrases, typing them out character-by-character
/// with a blinking cursor, then clearing and starting the next phrase.
struct TypingDemoView: View {
    private let phrases: [String] = [
        "Hello, world.",
        "Always is now listening.",
        "Just speak naturally — I'll type for you.",
    ]

    @State private var currentPhrase = 0
    @State private var displayedText = ""
    @State private var cursorVisible = true
    @State private var timer: Timer?
    @State private var isErasing = false
    @State private var isPausing = false

    var body: some View {
        VStack(spacing: 16) {
            // Mic indicator
            HStack(spacing: 6) {
                Image(systemName: "mic.fill")
                    .font(.system(size: 10))
                    .foregroundColor(.green)
                Text("Listening")
                    .font(.system(size: 11, weight: .medium))
                    .foregroundColor(.green)
                Circle()
                    .fill(Color.green)
                    .frame(width: 6, height: 6)
                    .opacity(cursorVisible ? 1 : 0.3)
            }
            .frame(maxWidth: .infinity, alignment: .leading)

            // Text field with typing animation
            HStack(spacing: 0) {
                Text(displayedText)
                    .font(.system(size: 22, weight: .medium))
                    .foregroundColor(.primary)
                Text("|")
                    .font(.system(size: 22, weight: .medium))
                    .foregroundColor(.accentColor)
                    .opacity(cursorVisible ? 1 : 0)
                Spacer()
            }
            .padding(.horizontal, 18)
            .padding(.vertical, 22)
            .background(
                RoundedRectangle(cornerRadius: 12)
                    .fill(Color.secondary.opacity(0.06))
                    .overlay(
                        RoundedRectangle(cornerRadius: 12)
                            .stroke(Color.accentColor.opacity(0.2), lineWidth: 1)
                    )
            )
        }
        .onAppear { startAnimation() }
        .onDisappear { timer?.invalidate() }
    }

    private func startAnimation() {
        // Blink the cursor continuously.
        withAnimation(.easeInOut(duration: 0.5).repeatForever()) {
            cursorVisible.toggle()
        }

        timer = Timer.scheduledTimer(withTimeInterval: 0.06, repeats: true) { _ in
            tick()
        }
    }

    private func tick() {
        let full = phrases[currentPhrase]

        if isPausing { return }

        if isErasing {
            if !displayedText.isEmpty {
                displayedText.removeLast()
            } else {
                currentPhrase = (currentPhrase + 1) % phrases.count
                isErasing = false
                isPausing = true
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.3) {
                    isPausing = false
                }
            }
            return
        }

        if displayedText.count < full.count {
            let idx = full.index(full.startIndex, offsetBy: displayedText.count)
            displayedText.append(full[idx])
        } else {
            // Phrase complete — pause, then start erasing.
            isPausing = true
            DispatchQueue.main.asyncAfter(deadline: .now() + 1.8) {
                isPausing = false
                isErasing = true
            }
        }
    }
}

#Preview {
    OnboardingView()
}
