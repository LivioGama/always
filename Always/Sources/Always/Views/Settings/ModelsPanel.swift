import SwiftUI

struct ModelsPanel: View {
    @Binding var config: Config
    @Binding var apiKey: String
    @Binding var showApiKey: Bool
    @Binding var isSavingApiKey: Bool
    @Binding var isDeletingApiKey: Bool
    @FocusState.Binding var focusedField: SettingsWindow.Field?
    var saveApiKey: () -> Void
    var deleteApiKey: () -> Void
    var savePostprocessProvider: () -> Void
    @ObservedObject private var models: ModelManagerClient = .shared

    /// The key (and today's Groq usage) only matter when Groq does some of
    /// the work: as the transcription engine or as the Postprocess mode
    /// (owner request 2026-11-25).
    private var usesGroq: Bool {
        models.activeBackend == "groq" || config.resolvedPostprocessMode == "groq"
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                if usesGroq {
                    groqKeySection
                    Divider()
                }
                postprocessSection
                Divider()
                ModelsSection(config: $config)
            }
            .padding(20)
        }
    }

    /// Groq API key lives on the Models tab because Groq is itself a
    /// model backend — the key is what enables the remote option.
    /// When a key is already saved, the field is disabled and only a
    /// Delete button is shown. When no key is set, the field is
    /// editable with Save and Show buttons.
    private var groqKeySection: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Groq API Key").font(.headline)
            if hasGroqKey {
                groqKeySetState
            } else {
                groqKeyEditState
            }
            Text("Required for the remote Groq backend and for smart postprocessing.")
                .font(.caption2)
                .foregroundColor(.secondary)
            if hasGroqKey {
                GroqUsageBar()
                    .padding(.top, 4)
            }
        }
    }

    /// Key is saved — show a disabled masked field and a Delete button.
    /// No Save, no Show, no editing.
    private var groqKeySetState: some View {
        HStack {
            SecureField("Groq API Key", text: .constant("••••••••"))
                .textFieldStyle(.roundedBorder)
                .disabled(true)

            if isDeletingApiKey {
                ProgressView()
                    .scaleEffect(0.6)
                    .frame(width: 14, height: 14)
            }

            Button(role: .destructive) {
                deleteApiKey()
            } label: {
                Image(systemName: "trash")
            }
            .buttonStyle(.borderless)
            .help("Delete API Key")
            .disabled(isDeletingApiKey)
        }
    }

    /// No key saved — editable field with Show/Save buttons.
    private var groqKeyEditState: some View {
        HStack {
            ZStack(alignment: .trailing) {
                Group {
                    if showApiKey {
                        TextField("Groq API Key", text: $apiKey)
                            .textFieldStyle(.roundedBorder)
                    } else {
                        SecureField("Groq API Key", text: $apiKey)
                            .textFieldStyle(.roundedBorder)
                    }
                }
                .focused($focusedField, equals: .apiKey)
                .onChange(of: apiKey) { _, newValue in
                    config.groqApiKey = newValue.isEmpty ? nil : newValue
                }
                if isSavingApiKey {
                    ProgressView()
                        .scaleEffect(0.6)
                        .frame(width: 14, height: 14)
                        .padding(.trailing, 8)
                }
            }

            Button {
                showApiKey.toggle()
                focusedField = nil
            } label: {
                Image(systemName: showApiKey ? "eye.slash" : "eye")
            }
            .buttonStyle(.borderless)

            Button {
                saveApiKey()
            } label: {
                Image(systemName: "arrow.down.doc")
            }
            .buttonStyle(.borderless)
            .help("Save API Key")
            .disabled(isSavingApiKey)
        }
    }

    private var hasGroqKey: Bool {
        config.groqKeySaved
    }

    /// One explicit Postprocess mode (SPEC §9/§10), offered as the same
    /// cards as the transcription engines. Changing it restarts the daemon,
    /// like the provider switch it replaces.
    private var postprocessSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            VStack(alignment: .leading, spacing: 2) {
                Text("Postprocessing").font(.headline)
                Text("How a transcript is cleaned up before it is pasted.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            ForEach(Self.postprocessModes, id: \.self) { mode in
                ChoiceCard(
                    title: Self.postprocessTitle(mode),
                    detail: "\(Self.postprocessDescription(mode)) \(Self.postprocessLatency(mode))",
                    isActive: config.resolvedPostprocessMode == mode,
                    warning: postprocessWarning(mode),
                    onUse: { postprocessModeBinding.wrappedValue = mode }
                )
            }
        }
    }

    private static let postprocessModes = ["fast", "apple", "groq", "off"]

    private static func postprocessTitle(_ mode: String) -> String {
        switch mode {
        case "fast": return "Always Fast"
        case "apple": return "Apple Intelligence"
        case "groq": return "Groq (Cloud)"
        default: return "Off"
        }
    }

    private func postprocessWarning(_ mode: String) -> String? {
        guard config.resolvedPostprocessMode == mode else { return nil }
        if mode == "apple" && !config.appleIntelligenceAvailable {
            return "Apple Intelligence isn't ready on this Mac yet. Enable it and download the language model in System Settings → Apple Intelligence & Siri. Until then Always Fast corrections are used."
        }
        if mode == "groq" && !hasGroqKey {
            return "Enter and save a Groq API key above. Until then Always Fast corrections are used."
        }
        return nil
    }

    private var postprocessModeBinding: Binding<String> {
        Binding(
            get: { config.resolvedPostprocessMode },
            set: { newValue in
                guard newValue != config.resolvedPostprocessMode else { return }
                config.postprocessMode = newValue
                savePostprocessProvider()
            }
        )
    }

    private static func postprocessDescription(_ mode: String) -> String {
        switch mode {
        case "fast":
            return "Built-in corrector: your glossary and learned corrections, casing, stray punctuation and recognizer artifacts. No model, nothing leaves the Mac."
        case "apple":
            return "Always Fast first, plus Apple Intelligence on-device grammar cleanup when its result is ready by paste time. Pasting never waits for it."
        case "groq":
            return "Always Fast first, plus Groq cloud grammar cleanup when its result is ready by paste time. Pasting never waits for it."
        default:
            return "The raw transcript, with only filler words and repeated words removed."
        }
    }

    private static func postprocessLatency(_ mode: String) -> String {
        switch mode {
        case "fast": return "Adds about 1 ms before paste."
        case "apple", "groq": return "Adds about 1 ms before paste; the model runs in the background."
        default: return "Adds under 1 ms before paste."
        }
    }
}
