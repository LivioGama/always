import SwiftUI

struct ModelsPanel: View {
    @Binding var config: Config
    @Binding var apiKey: String
    @Binding var showApiKey: Bool
    @Binding var isSavingApiKey: Bool
    @FocusState.Binding var focusedField: SettingsWindow.Field?
    var saveApiKey: () -> Void

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                ModelsSection()
                Divider()
                groqKeySection
            }
            .padding(20)
        }
    }

    /// Groq API key lives on the Models tab because Groq is itself a
    /// model backend — the key is what enables the remote option.
    private var groqKeySection: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Groq API Key").font(.headline)
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
            Text("Required for the remote Groq backend and for smart postprocessing.")
                .font(.caption2)
                .foregroundColor(.secondary)
        }
    }
}
