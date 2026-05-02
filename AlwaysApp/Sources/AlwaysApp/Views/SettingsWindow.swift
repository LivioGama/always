import SwiftUI

struct SettingsWindow: View {
    @ObservedObject var cliService: CLIService
    @ObservedObject private var stateMonitor: StateMonitor = .shared
    @State private var config: Config = Config.defaultConfig
    @State private var status: DaemonStatus?
    @State private var isLoading = false
    @State private var apiKey: String = ""
    @State private var showApiKey = false
    @State private var isSavingApiKey = false
    @FocusState private var focusedField: Field?
    @State private var vocabularyTermCount: Int = 0
    @State private var vocabularyPath: String = ""
    
    enum Field {
        case apiKey
    }

    var body: some View {
        Form {
            Section(header: Text("Daemon Control")) {
                HStack {
                    Button(action: toggleDaemon) {
                        HStack {
                            Image(systemName: status?.isRunning == true ? "stop.circle.fill" : "play.circle.fill")
                            Text(status?.isRunning == true ? "Stop Daemon" : "Start Daemon")
                        }
                    }
                    .disabled(isLoading)
                    .buttonStyle(.borderedProminent)

                    Spacer()

                    Text(status?.isRunning == true ? "Running" : "Stopped")
                        .foregroundColor(status?.isRunning == true ? .green : .red)
                }
            }

            Section(header: Text("Behavior")) {
                HStack {
                    // Bind the toggle to the daemon's authoritative state.
                    // Tapping fires a UDS command; the resulting AutoEnter*
                    // event from the daemon updates stateMonitor.isAutoEnter
                    // which flips the toggle.
                    Toggle("Auto-Enter After Paste", isOn: Binding(
                        get: { stateMonitor.isAutoEnter },
                        set: { _ in stateMonitor.toggleAutoEnter() }
                    ))
                }

                HStack {
                    Button(action: {
                        stateMonitor.togglePause()
                    }) {
                        HStack {
                            Image(systemName: stateMonitor.isPaused ? "play.circle.fill" : "pause.circle.fill")
                            Text(stateMonitor.isPaused ? "Resume" : "Pause")
                        }
                    }
                    .buttonStyle(.bordered)
                }
            }

            Section(header: Text("API Configuration")) {
                HStack {
                    ZStack(alignment: .trailing) {
                        if showApiKey {
                            TextField("Groq API Key", text: $apiKey)
                                .focused($focusedField, equals: .apiKey)
                                .textFieldStyle(.plain)
                                .onChange(of: apiKey) { _, newValue in
                                    config.groqApiKey = newValue.isEmpty ? nil : newValue
                                }
                        } else {
                            SecureField("Groq API Key", text: $apiKey)
                                .focused($focusedField, equals: .apiKey)
                                .textFieldStyle(.plain)
                                .onChange(of: apiKey) { _, newValue in
                                    config.groqApiKey = newValue.isEmpty ? nil : newValue
                                }
                        }

                        // Show spinner inside the text field when saving
                        if isSavingApiKey {
                            HStack {
                                Spacer()
                                ProgressView()
                                    .scaleEffect(0.7)
                                    .frame(width: 16, height: 16)
                                    .padding(.trailing, 8)
                            }
                        }
                    }

                    Button(action: {
                        showApiKey.toggle()
                        focusedField = nil // Clear focus when toggling visibility
                    }) {
                        Image(systemName: showApiKey ? "eye.slash" : "eye")
                    }
                    .buttonStyle(.plain)

                    Button(action: {
                        saveApiKey()
                    }) {
                        Image(systemName: "arrow.down.doc")
                    }
                    .buttonStyle(.plain)
                    .help("Save API Key")
                    .disabled(isSavingApiKey)
                }

                Text("Required for transcription")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }
            
            Section(header: Text("Vocabulary")) {
                VStack(alignment: .leading, spacing: 12) {
                    HStack {
                        Image(systemName: "book.fill")
                            .foregroundColor(.blue)
                        Text("Glossary")
                            .font(.headline)
                        Spacer()
                        if vocabularyTermCount > 0 {
                            Text("\(vocabularyTermCount) terms")
                                .font(.caption)
                                .foregroundColor(.secondary)
                        }
                    }
                    
                    Text("Custom vocabulary improves transcription accuracy for technical terms, names, and project-specific language.")
                        .font(.caption)
                        .foregroundColor(.secondary)
                    
                    HStack(spacing: 12) {
                        Button(action: openVocabularyFile) {
                            HStack {
                                Image(systemName: "doc.text")
                                Text("Open glossary.json")
                            }
                        }
                        .buttonStyle(.bordered)
                        
                        Button(action: revealVocabularyInFinder) {
                            HStack {
                                Image(systemName: "folder")
                                Text("Show in Finder")
                            }
                        }
                        .buttonStyle(.bordered)
                        
                        Button(action: refreshVocabularyInfo) {
                            Image(systemName: "arrow.clockwise")
                        }
                        .buttonStyle(.borderless)
                        .help("Refresh vocabulary info")
                    }
                    
                    if !vocabularyPath.isEmpty {
                        Text(vocabularyPath)
                            .font(.system(.caption, design: .monospaced))
                            .foregroundColor(.secondary)
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }
                }
                .padding(.vertical, 4)
            }

            Section(header: Text("Sensitivity Settings")) {
                HStack(spacing: 20) {
                    VStack(alignment: .leading) {
                        Text("STT Energy Threshold")
                            .font(.headline)
                        Text("\(config.sttEnergyThreshold, specifier: "%.4f")")
                            .font(.caption)
                            .foregroundColor(.secondary)
                        Slider(value: $config.sttEnergyThreshold, in: 0.001...0.05, step: 0.001)
                        Text("Lower = more sensitive")
                            .font(.caption)
                            .foregroundColor(.secondary)
                    }

                    VStack(alignment: .leading) {
                        Text("Hear Energy Threshold")
                            .font(.headline)
                        Text("\(config.hearEnergyThreshold, specifier: "%.4f")")
                            .font(.caption)
                            .foregroundColor(.secondary)
                        Slider(value: $config.hearEnergyThreshold, in: 0.0005...0.01, step: 0.0005)
                        Text("Lower = more sensitive")
                            .font(.caption)
                            .foregroundColor(.secondary)
                    }
                }

                HStack(spacing: 20) {
                    VStack(alignment: .leading) {
                        Text("Silence Timeout")
                            .font(.headline)
                        Text("\(config.sttSilence, specifier: "%.1f")s")
                            .font(.caption)
                            .foregroundColor(.secondary)
                        Slider(value: $config.sttSilence, in: 0.1...3.0, step: 0.1)
                    }

                    VStack(alignment: .leading) {
                        Text("Cooldown")
                            .font(.headline)
                        Text("\(config.sttCooldownMs)ms")
                            .font(.caption)
                            .foregroundColor(.secondary)
                        Slider(value: Binding(
                            get: { Double(config.sttCooldownMs) },
                            set: { config.sttCooldownMs = Int($0) }
                        ), in: 50...3000, step: 50)
                    }
                }

                VStack(alignment: .leading) {
                    Text("Silero VAD Threshold")
                        .font(.headline)
                    Text("\(config.sileroThreshold, specifier: "%.2f")")
                        .font(.caption)
                        .foregroundColor(.secondary)
                    Slider(value: $config.sileroThreshold, in: 0.1...0.9, step: 0.05)
                    Text("Higher = stricter speech detection (less noise, may miss quiet speech)")
                        .font(.caption)
                        .foregroundColor(.secondary)
                }
            }

            Section(header: Text("Keyboard Shortcuts")) {
                HStack {
                    Text("Pause/Unpause")
                    Spacer()
                    Text("Ctrl+Shift+P")
                        .foregroundColor(.secondary)
                }

                HStack {
                    Text("Toggle Auto-Enter")
                    Spacer()
                    Text("Ctrl+Shift+A")
                        .foregroundColor(.secondary)
                }

                HStack {
                    Text("Stop Daemon")
                    Spacer()
                    Text("Ctrl+C")
                        .foregroundColor(.secondary)
                }
            }
        }
        .formStyle(.grouped)
        .frame(width: 800, height: 750)
        .onAppear {
            // Clear focus immediately and after a delay to prevent autofocus
            focusedField = nil
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.1) {
                focusedField = nil
            }
            refreshVocabularyInfo()
            Task {
                await loadConfig()
                await refreshStatus()
            }
        }
        .onChange(of: config.sttEnergyThreshold) { _, _ in saveConfig() }
        .onChange(of: config.hearEnergyThreshold) { _, _ in saveConfig() }
        .onChange(of: config.sttSilence) { _, _ in saveConfig() }
        .onChange(of: config.sttCooldownMs) { _, _ in saveConfig() }
        .onChange(of: config.sileroThreshold) { _, _ in saveConfig() }
    }

    private func toggleDaemon() {
        isLoading = true
        Task {
            do {
                if status?.isRunning == true {
                    _ = try await cliService.stopDaemon()
                } else {
                    _ = try await cliService.startDaemon()
                }
                await refreshStatus()
            } catch {
                print("Error toggling daemon: \(error)")
            }
            isLoading = false
        }
    }

    private func loadConfig() async {
        isLoading = true
        do {
            config = try await cliService.getConfig()
            // Mask the API key with stars
            if let key = config.groqApiKey, !key.isEmpty {
                apiKey = String(repeating: "•", count: key.count)
            } else {
                apiKey = ""
            }
        } catch {
            print("Error loading config: \(error)")
        }
        isLoading = false
    }

    private func refreshStatus() async {
        do {
            status = try await cliService.getStatus()
        } catch {
            print("Error refreshing status: \(error)")
            status = DaemonStatus(isRunning: false, pid: nil, logPath: nil)
        }
    }

    private func saveApiKey() {
        isSavingApiKey = true
        Task {
            do {
                // Only save API key if it's not masked (doesn't contain only dots)
                if !apiKey.allSatisfy({ c in c == "•" }) {
                    _ = try await cliService.setConfig(key: "groq_api_key", value: apiKey.isEmpty ? "" : apiKey)
                }
                // Add a small delay to make the loader visible for testing
                try await Task.sleep(nanoseconds: 1_000_000_000) // 1 second
            } catch {
                print("Error saving API key: \(error)")
            }
            isSavingApiKey = false
        }
    }

    private func saveConfig() {
        Task {
            do {
                _ = try await cliService.setConfig(key: "stt_energy_threshold", value: String(config.sttEnergyThreshold))
                _ = try await cliService.setConfig(key: "hear_energy_threshold", value: String(config.hearEnergyThreshold))
                _ = try await cliService.setConfig(key: "stt_silence", value: String(config.sttSilence))
                _ = try await cliService.setConfig(key: "stt_cooldown_ms", value: String(config.sttCooldownMs))
                _ = try await cliService.setConfig(key: "stt_auto_enter", value: String(config.sttAutoEnter))
                _ = try await cliService.setConfig(key: "silero_threshold", value: String(config.sileroThreshold))
                // Only save API key if it's not masked (doesn't contain only dots)
                if !apiKey.allSatisfy({ c in c == "•" }) {
                    _ = try await cliService.setConfig(key: "groq_api_key", value: apiKey.isEmpty ? "" : apiKey)
                }
            } catch {
                print("Error saving config: \(error)")
            }
        }
    }
    
    private func openVocabularyFile() {
        let projectPath = findProjectRoot()
        let glossaryPath = projectPath.appendingPathComponent("glossary.json")
        
        if FileManager.default.fileExists(atPath: glossaryPath.path) {
            NSWorkspace.shared.open(glossaryPath)
        } else {
            // Create empty glossary if doesn't exist
            let emptyGlossary = "[]"
            try? emptyGlossary.write(to: glossaryPath, atomically: true, encoding: .utf8)
            NSWorkspace.shared.open(glossaryPath)
        }
    }
    
    private func revealVocabularyInFinder() {
        let projectPath = findProjectRoot()
        let glossaryPath = projectPath.appendingPathComponent("glossary.json")
        
        if FileManager.default.fileExists(atPath: glossaryPath.path) {
            NSWorkspace.shared.activateFileViewerSelecting([glossaryPath])
        } else {
            NSWorkspace.shared.activateFileViewerSelecting([projectPath])
        }
    }
    
    private func refreshVocabularyInfo() {
        let projectPath = findProjectRoot()
        let glossaryPath = projectPath.appendingPathComponent("glossary.json")
        vocabularyPath = glossaryPath.path
        
        if FileManager.default.fileExists(atPath: glossaryPath.path),
           let data = try? Data(contentsOf: glossaryPath),
           let json = try? JSONSerialization.jsonObject(with: data) as? [[String: Any]] {
            vocabularyTermCount = json.count
        } else {
            vocabularyTermCount = 0
        }
    }
    
    private func findProjectRoot() -> URL {
        // Path to the always binary - same logic as CLIService
        let currentPath = URL(fileURLWithPath: #file)
        return currentPath
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
    }
}
