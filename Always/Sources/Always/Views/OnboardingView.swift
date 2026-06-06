import SwiftUI
import AVFoundation
import ApplicationServices
import AppKit

struct OnboardingView: View {
    // API key state
    @State private var apiKey: String = ""
    @State private var isSaving = false
    @State private var saveError: String?
    @State private var validatingKey = false
    @State private var keyValid: Bool? = nil
    @State private var lastValidatedKey: String = ""

    // Permission state
    @State private var micStatus: AVAuthorizationStatus = AVCaptureDevice.authorizationStatus(for: .audio)
    @State private var accessibilityStatus: Bool = AXIsProcessTrusted()

    // Periodic refresh for accessibility (no push notifications from macOS)
    @State private var permissionRefreshTimer: Timer? = nil

    @Environment(\.dismiss) private var dismiss

    private let cliService = CLIService()

    private var allRequirementsMet: Bool {
        micStatus == .authorized
            && accessibilityStatus
            && keyValid == true
            && !apiKey.isEmpty
    }

    var body: some View {
        VStack(spacing: 20) {
            // Header
            VStack(spacing: 8) {
                Image(systemName: "mic.fill")
                    .font(.system(size: 44))
                    .foregroundColor(.accentColor)

                Text("Welcome to Always")
                    .font(.title)
                    .fontWeight(.bold)

                Text("Let's get you set up in three quick steps")
                    .font(.subheadline)
                    .foregroundColor(.secondary)
            }
            .padding(.top, 16)

            // Step 1: Microphone
            permissionRow(
                title: "Microphone access",
                subtitle: "Needed to capture your voice",
                granted: micStatus == .authorized,
                trailing: {
                    if micStatus == .notDetermined {
                        Button("Grant") { requestMicrophone() }
                    } else if micStatus == .denied || micStatus == .restricted {
                        Button("Open Settings") { openMicrophoneSettings() }
                    }
                }
            )

            // Step 2: Accessibility
            permissionRow(
                title: "Accessibility access",
                subtitle: "Needed to paste transcribed text into other apps",
                granted: accessibilityStatus,
                trailing: {
                    if !accessibilityStatus {
                        Button("Grant") { requestAccessibility() }
                        Button("Open Settings") { openAccessibilitySettings() }
                            .buttonStyle(.bordered)
                    }
                }
            )

            // Step 3: API Key
            VStack(alignment: .leading, spacing: 8) {
                HStack(spacing: 8) {
                    Image(systemName: keyValid == true ? "checkmark.circle.fill" : "circle")
                        .foregroundColor(keyValid == true ? .green : .secondary)
                    Text("Groq API Key")
                        .font(.headline)
                    Spacer()
                    if validatingKey {
                        ProgressView().controlSize(.small)
                    } else if keyValid == false {
                        Image(systemName: "xmark.circle.fill").foregroundColor(.red)
                    }
                }

                SecureField("Enter your API key", text: $apiKey)
                    .textFieldStyle(.roundedBorder)
                    .controlSize(.regular)
                    .onChange(of: apiKey) {
                        // Invalidate previous result when key changes
                        if apiKey != lastValidatedKey {
                            keyValid = nil
                        }
                    }

                HStack {
                    Button("Validate") {
                        Task { await validateGroqKey(apiKey) }
                    }
                    .disabled(apiKey.isEmpty || validatingKey)

                    Button("Get an API key") {
                        if let url = URL(string: "https://console.groq.com/keys") {
                            NSWorkspace.shared.open(url)
                        }
                    }
                    .buttonStyle(.bordered)

                    Spacer()
                }

                Text("Your key is saved locally for Always and never shown in command output.")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }
            .padding(.horizontal, 20)

            // Error message
            if let error = saveError {
                Text(error)
                    .font(.caption)
                    .foregroundColor(.red)
                    .padding(.horizontal, 20)
            }

            Spacer()

            // Done button
            Button(action: completeSetup) {
                if isSaving {
                    ProgressView().progressViewStyle(.circular)
                } else {
                    Text("Done")
                        .fontWeight(.semibold)
                        .frame(minWidth: 100)
                }
            }
            .disabled(!allRequirementsMet || isSaving)
            .buttonStyle(.borderedProminent)
            .controlSize(.large)
            .padding(.bottom, 20)
        }
        .frame(width: 520, height: 560)
        .background(WindowAccessor { window in
            window?.identifier = NSUserInterfaceItemIdentifier("AlwaysOnboarding")
            window?.title = "Welcome to Always"
        })
        .onAppear {
            loadExistingKey()
            refreshPermissions()
            startPermissionRefreshTimer()
        }
        .onDisappear {
            stopPermissionRefreshTimer()
        }
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

    // MARK: - API key validation

    private func validateGroqKey(_ key: String) async {
        await MainActor.run {
            validatingKey = true
            saveError = nil
        }
        defer {
            Task { @MainActor in validatingKey = false }
        }

        guard let url = URL(string: "https://api.groq.com/openai/v1/models") else {
            await MainActor.run { keyValid = false }
            return
        }

        var request = URLRequest(url: url)
        request.setValue("Bearer \(key)", forHTTPHeaderField: "Authorization")
        request.timeoutInterval = 10

        do {
            let (_, response) = try await URLSession.shared.data(for: request)
            let validation = groqKeyValidationResult(
                statusCode: (response as? HTTPURLResponse)?.statusCode
            )
            await MainActor.run {
                keyValid = validation == .valid
                lastValidatedKey = key
                if case let .invalid(message) = validation {
                    saveError = message
                }
            }
        } catch {
            let validation = groqKeyValidationResult(error: error)
            await MainActor.run {
                keyValid = false
                if case let .invalid(message) = validation {
                    saveError = message
                }
            }
        }
    }

    // MARK: - Saved key

    private func loadExistingKey() {
        Task {
            do {
                let config = try await cliService.getConfig()
                await MainActor.run {
                    apiKey = config.groqApiKey ?? ""
                    if !apiKey.isEmpty {
                        keyValid = true
                        lastValidatedKey = apiKey
                    }
                }
            } catch {
                await MainActor.run {
                    saveError = "Could not load saved settings: \(error.localizedDescription)"
                }
            }
        }
    }

    // MARK: - Done

    private func completeSetup() {
        guard allRequirementsMet else { return }
        isSaving = true
        saveError = nil

        Task {
            do {
                _ = try await cliService.setConfig(key: "groq_api_key", value: apiKey)
                await MainActor.run {
                    isSaving = false
                    dismiss()
                }
            } catch {
                await MainActor.run {
                    isSaving = false
                    saveError = "Failed to save API key: \(error.localizedDescription)"
                }
            }
        }
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

#Preview {
    OnboardingView()
}
