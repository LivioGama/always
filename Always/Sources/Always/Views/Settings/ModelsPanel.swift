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

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                groqKeySection
                Divider()
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

    /// Apple Intelligence works on-device with no API key; the Groq
    /// backend needs a key. Matches the daemon's `can_correct()` gate.
    private var usesApple: Bool {
        config.postprocessProvider == "apple"
    }

    private var postprocessSection: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Postprocessing").font(.headline)

            Picker("Provider", selection: $config.postprocessProvider) {
                Text("Apple Intelligence (on-device)").tag("apple")
                Text("Groq (cloud)").tag("groq")
            }
            .pickerStyle(.menu)
            .onChange(of: config.postprocessProvider) { oldValue, newValue in
                if oldValue != newValue {
                    savePostprocessProvider()
                }
            }
            .help("Which engine fixes grammar and glossary terms after transcription.")

            Toggle(
                "Smart Postprocess (LLM grammar + glossary fixes)",
                isOn: $config.postprocessEnabled
            )
            .disabled(usesGroqWithoutKey)

            if usesGroqWithoutKey {
                Text("Enter and save a Groq API key above to enable smart postprocessing.")
                    .font(.caption)
                    .foregroundColor(.orange)
                    .fixedSize(horizontal: false, vertical: true)
            } else if usesApple && !config.appleIntelligenceAvailable {
                Text("Apple Intelligence isn't ready on this Mac yet. Enable it and download the language model in System Settings → Apple Intelligence & Siri, then the on-device model will run automatically.")
                    .font(.caption)
                    .foregroundColor(.orange)
                    .fixedSize(horizontal: false, vertical: true)
            } else if usesApple {
                Text("Runs on-device using Apple Intelligence — no API key needed (macOS 15+). Fixes grammar and applies your glossary automatically.")
                    .font(.caption)
                    .foregroundColor(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            } else {
                Text("Runs after each transcription — fixes grammar and applies your glossary automatically.")
                    .font(.caption)
                    .foregroundColor(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    /// True when the Groq backend is selected but no key is present —
    /// the only state in which the toggle is disabled.
    private var usesGroqWithoutKey: Bool {
        !usesApple && !hasGroqKey
    }
}
