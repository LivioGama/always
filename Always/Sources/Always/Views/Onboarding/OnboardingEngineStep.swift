import SwiftUI
import AppKit

/// Onboarding "Engine" step: pick how speech is transcribed. Never blocks
/// Continue; skipping keeps whatever backend is already active.
struct OnboardingEngineStep: View {
    @ObservedObject private var models: ModelManagerClient = .shared
    @ObservedObject private var speech: SpeechModelSetup = .shared

    @State private var apiKey = ""
    @State private var isSavingKey = false
    @State private var keyError: String?

    private let cliService = CLIService()

    /// Best catalogue model: recommended first, then speed + accuracy
    /// (same ranking as GroqQuotaPrompt).
    private var bestLocal: ModelInfo? {
        models.models
            .filter { !$0.is_custom }
            .max { rank($0) < rank($1) }
    }

    private var readyLocal: ModelInfo? {
        models.models
            .filter { $0.is_downloaded && !$0.is_downloading }
            .max { rank($0) < rank($1) }
    }

    private func rank(_ m: ModelInfo) -> (Int, Float) {
        (m.is_recommended ? 1 : 0, m.speed_score + m.accuracy_score)
    }

    var body: some View {
        ScrollView {
            VStack(spacing: 10) {
                groqCard
                localCard
                if speech.isOnDeviceEngineAvailable { appleCard }
                Text("Set up later in Settings → Models")
                    .font(.caption)
                    .foregroundColor(.secondary)
                    .padding(.top, 2)
            }
            .padding(.horizontal, 20)
            .padding(.bottom, 4)
        }
        .onAppear { models.requestModelsList() }
    }

    // MARK: Cards

    private var groqCard: some View {
        card(
            title: "Groq (cloud)",
            detail: "Best accuracy and speed. Needs a free API key.",
            selected: models.activeBackend == "groq"
        ) {
            HStack(spacing: 8) {
                SecureField("gsk_…", text: $apiKey)
                    .textFieldStyle(.roundedBorder)
                    .onSubmit(saveKey)
                Button("Save & Use", action: saveKey)
                    .buttonStyle(.borderedProminent)
                    .controlSize(.small)
                    .disabled(isSavingKey || apiKey.trimmingCharacters(in: .whitespaces).isEmpty)
            }
            HStack {
                Link("Get a free key", destination: URL(string: "https://console.groq.com/keys")!)
                    .font(.caption)
                if isSavingKey { ProgressView().controlSize(.small) }
            }
            if let keyError {
                Text(keyError).font(.caption).foregroundColor(.red)
            }
            Text("Free tier: about 2,000 requests a day. Live transcript uses more of it.")
                .font(.caption2)
                .foregroundColor(.secondary)
        }
    }

    @ViewBuilder
    private var localCard: some View {
        if let ready = readyLocal {
            card(
                title: "Local model",
                detail: "Private and offline. \(ready.name) is ready.",
                selected: models.activeBackend.hasPrefix("local:")
            ) {
                Button("Use \(ready.name)") { models.setActive(backend: "local:\(ready.id)") }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.small)
            }
        } else if let best = bestLocal {
            card(
                title: "Local model",
                detail: "Private and offline. One-time download of \(best.name) (\(best.size_mb) MB).",
                selected: false
            ) {
                if let p = models.downloadProgress[best.id] {
                    HStack(spacing: 8) {
                        // downloadProgress is 0–100.
                        ProgressView(value: p, total: 100)
                        Text("\(Int(p))%").font(.caption).foregroundColor(.secondary)
                    }
                    Text("Keeps downloading while you continue.")
                        .font(.caption2)
                        .foregroundColor(.secondary)
                } else if models.installing.contains(best.id) {
                    HStack(spacing: 8) {
                        ProgressView().controlSize(.small)
                        Text("Installing…").font(.caption).foregroundColor(.secondary)
                    }
                } else {
                    Button("Download & Use") {
                        models.pendingBackend = "local:\(best.id)"
                        models.download(best.id)
                    }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.small)
                }
                if let err = models.errors[best.id] {
                    Text(err).font(.caption).foregroundColor(.red)
                }
            }
        } else {
            card(
                title: "Local model",
                detail: "Private and offline. Loading the model list…",
                selected: false
            ) { ProgressView().controlSize(.small) }
        }
    }

    private var appleCard: some View {
        card(
            title: "Apple on-device",
            detail: "Built into macOS. No key, no download. It works, but results aren't as good as Groq or a local model.",
            selected: models.activeBackend == "apple"
        ) {
            Button("Use Apple") { models.setActive(backend: "apple") }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
        }
    }

    private func card<Content: View>(
        title: String,
        detail: String,
        selected: Bool,
        @ViewBuilder content: () -> Content
    ) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text(title).font(.body.weight(.medium))
                Spacer()
                if selected {
                    Image(systemName: "checkmark.circle.fill")
                        .font(.system(size: 18))
                        .foregroundColor(.green)
                }
            }
            Text(detail)
                .font(.caption)
                .foregroundColor(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            content()
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, 16)
        .padding(.vertical, 10)
        .background(
            RoundedRectangle(cornerRadius: 12)
                .fill(Color.secondary.opacity(0.06))
                .overlay(
                    RoundedRectangle(cornerRadius: 12)
                        .stroke(selected ? Color.green.opacity(0.3) : Color.secondary.opacity(0.12), lineWidth: 1)
                )
        )
    }

    // MARK: Actions

    private func saveKey() {
        let key = apiKey.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !key.isEmpty, !isSavingKey else { return }
        isSavingKey = true
        keyError = nil
        Task {
            defer { isSavingKey = false }
            var request = URLRequest(url: URL(string: "https://api.groq.com/openai/v1/models")!)
            request.setValue("Bearer \(key)", forHTTPHeaderField: "Authorization")
            request.timeoutInterval = 15
            let result: GroqKeyValidationResult
            do {
                let (_, response) = try await URLSession.shared.data(for: request)
                result = groqKeyValidationResult(statusCode: (response as? HTTPURLResponse)?.statusCode)
            } catch {
                result = groqKeyValidationResult(error: error)
            }
            if case .invalid(let message) = result { keyError = message; return }
            do {
                // Same path as Settings' saveApiKey: persist, then restart
                // so the daemon picks the key up.
                _ = try await cliService.setConfig(key: "groq_api_key", value: key)
                _ = try await cliService.restartDaemon()
                // The restarted daemon gets the switch with its next catalog
                // snapshot (see pendingBackend): asked for here in case it
                // reconnected already, else sent on reconnect. Set only now,
                // after restartDaemon killed the old daemon, which would
                // reject Groq for lack of the key.
                models.pendingBackend = "groq"
                models.requestModelsList()
                apiKey = ""
            } catch {
                keyError = "Could not save the key: \(error.localizedDescription)"
            }
        }
    }
}
