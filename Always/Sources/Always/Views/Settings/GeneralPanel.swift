import SwiftUI
import AppKit

struct GeneralPanel: View {
    @ObservedObject var cliService: CLIService
    @ObservedObject var stateMonitor: StateMonitor
    @ObservedObject var focusedApp: FocusedAppMonitor
    @Binding var config: Config
    @Binding var apiKey: String
    @Binding var showApiKey: Bool
    @Binding var isSavingApiKey: Bool
    @FocusState.Binding var focusedField: SettingsWindow.Field?
    var saveApiKey: () -> Void
    @State private var vocabularyTermCount: Int = 0
    @State private var vocabularyPath: String = ""

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                PermissionsBanner()
                statusAndPauseRow
                Divider()
                perAppSection
                Divider()
                behaviorRow
                Divider()
                apiSection
                Divider()
                vocabularySection
            }
            .padding(20)
        }
        .onAppear {
            refreshVocabularyInfo()
        }
    }

    // MARK: Sections

    private var statusAndPauseRow: some View {
        let isRunning = stateMonitor.isDaemonConnected
        let isDegraded = stateMonitor.isDaemonDegraded
        let displayLabel: String
        let displayColor: Color
        let symbol: String
        if isRunning && !isDegraded {
            displayLabel = "Running"
            displayColor = .green
            symbol = "checkmark.circle.fill"
        } else if isDegraded {
            displayLabel = "Reconnecting…"
            displayColor = .orange
            symbol = "arrow.triangle.2.circlepath"
        } else {
            displayLabel = "Disconnected"
            displayColor = .orange
            symbol = "exclamationmark.triangle.fill"
        }
        return HStack(spacing: 12) {
            Image(systemName: symbol)
                .foregroundColor(displayColor)
            Text(displayLabel)
                .font(.headline)
                .foregroundColor(displayColor)
            Spacer()
        }
    }

    private var behaviorRow: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Toggle(
                    "Auto-Enter After Paste",
                    isOn: Binding(
                        get: { stateMonitor.isAutoEnter },
                        set: { newValue in
                            config.sttAutoEnter = newValue
                            stateMonitor.setAutoEnter(newValue)
                        }
                    )
                )
                .disabled(!stateMonitor.isDaemonConnected)
                Spacer()
            }
            NumericSettingRow(
                title: "Auto-Enter Delay",
                help: "Seconds before Return is pressed. Set to 0 for immediate Return. Use the toggle above to disable auto-enter.",
                unit: "s",
                formatter: SettingsWindow.intFormatter,
                value: Binding(
                    get: { config.autoEnterDelayMs / 1000 },
                    set: { config.autoEnterDelayMs = $0 * 1000 }
                ),
                defaultValue: 4,
                range: 0...60
            )
            .padding(.top, 4)
            HStack {
                Toggle(
                    "Smart Postprocess (LLM grammar + glossary fixes)",
                    isOn: $config.postprocessEnabled
                )
                Spacer()
            }

            Divider().padding(.vertical, 2)

            Text("Idle Auto-Pause")
                .font(.subheadline.bold())
                .foregroundColor(.secondary)

            Text(
                "Pauses listening after a stretch with no speech. Does not flip the global master switch — switch to a resumed app or speak to wake. Changes apply on next daemon restart."
            )
            .font(.caption)
            .foregroundColor(.secondary)
            .fixedSize(horizontal: false, vertical: true)

            NumericSettingRow(
                title: "Pause After Inactivity",
                help: "Seconds of no voice before idle auto-pause. Default 600 (10 min). Set to 0 to disable.",
                unit: "s",
                formatter: SettingsWindow.intFormatter,
                value: $config.idlePauseSecs,
                defaultValue: 600,
                range: 0...86_400
            )

            HStack {
                Text("On Idle Timeout:")
                    .help("What happens when idle timeout occurs. 'Pause' just pauses listening. 'Pause + Mute' also mutes input audio.")
                Picker("", selection: $config.idlePauseAction) {
                    Text("Pause Only").tag("pause")
                    Text("Pause + Mute").tag("pause_and_mute")
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                Spacer()
            }
            .padding(.top, 2)
        }
    }

    private var perAppSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .firstTextBaseline) {
                Text("Voice Typing Allowlist").font(.headline)
                Spacer()
                Text("\(stateMonitor.resumedBundleIds.count) app\(stateMonitor.resumedBundleIds.count == 1 ? "" : "s") resumed")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }
            Text("Always is paused by default. Add an app here to resume voice typing while you're in it.")
                .font(.caption)
                .foregroundColor(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            masterPauseControl
            focusedAppCard

            if !stateMonitor.resumedBundleIds.isEmpty {
                Divider().padding(.vertical, 2)
                Text("Resumed apps")
                    .font(.subheadline.bold())
                    .foregroundColor(.secondary)
                VStack(alignment: .leading, spacing: 4) {
                    ForEach(
                        stateMonitor.resumedBundleIds.sorted(),
                        id: \.self
                    ) { bundleId in
                        resumedAppRow(bundleId: bundleId)
                    }
                }
            }
        }
    }

    @ViewBuilder
    private var masterPauseControl: some View {
        let masterPaused = stateMonitor.isMasterPaused
        HStack(spacing: 10) {
            Image(systemName: masterPaused ? "exclamationmark.octagon.fill" : "checkmark.shield.fill")
                .font(.title3)
                .foregroundColor(masterPaused ? .orange : .accentColor)
            VStack(alignment: .leading, spacing: 2) {
                Text(masterPaused ? "Paused everywhere" : "Allowlist active")
                    .font(.subheadline.bold())
                Text(
                    masterPaused
                        ? "Master kill switch is on — every resumed app is force-paused."
                        : "Resumed apps below are listening; everything else is paused."
                )
                .font(.caption)
                .foregroundColor(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            }
            Spacer()
            Button {
                stateMonitor.togglePause()
            } label: {
                Text(masterPaused ? "Lift pause" : "Pause everything")
            }
            .controlSize(.small)
            .disabled(!stateMonitor.isDaemonConnected)
        }
        .padding(10)
        .background(
            (masterPaused ? Color.orange : Color.accentColor).opacity(0.08)
        )
        .cornerRadius(6)
    }

    @ViewBuilder
    private var focusedAppCard: some View {
        let bundleId = focusedApp.currentBundleId
        let name = focusedApp.currentAppName ?? bundleId ?? "—"
        let isResumed = bundleId.map { stateMonitor.resumedBundleIds.contains($0) } ?? false
        let isPaused = stateMonitor.isPaused
        HStack(spacing: 10) {
            Image(systemName: isResumed ? "checkmark.circle.fill" : "pause.circle.fill")
                .font(.title3)
                .foregroundColor(isResumed && !isPaused ? .green : .orange)
            VStack(alignment: .leading, spacing: 2) {
                Text(bundleId == nil ? "No focused app" : "Currently focused")
                    .font(.caption)
                    .foregroundColor(.secondary)
                Text(name)
                    .font(.subheadline.bold())
                Text(currentAppStateText(isResumed: isResumed, isPaused: isPaused))
                    .font(.caption)
                    .foregroundColor(.secondary)
            }
            Spacer()
            if let bundle = bundleId {
                Button(action: {
                    let newPaused: Bool? = isResumed ? nil : false
                    stateMonitor.setAppPaused(bundleId: bundle, paused: newPaused)
                }) {
                    Text(isResumed ? "Remove from allowlist" : "Resume for this app")
                }
                .controlSize(.small)
            }
        }
        .padding(10)
        .background(Color.secondary.opacity(0.08))
        .cornerRadius(6)
    }

    private func currentAppStateText(isResumed: Bool, isPaused: Bool) -> String {
        if stateMonitor.isMasterPaused {
            return "Globally paused — use \"Lift pause\" above to resume all apps."
        }
        if stateMonitor.isIdleAutoPaused {
            return "Idle timeout — speak or switch to a resumed app to wake listening."
        }
        if isResumed && isPaused {
            return "On the allowlist but paused for another reason."
        }
        if isResumed { return "Voice typing is active here." }
        return "Paused (not on the allowlist)."
    }

    @ViewBuilder
    private func resumedAppRow(bundleId: String) -> some View {
        let isFocused = focusedApp.currentBundleId == bundleId
        HStack(spacing: 8) {
            Image(systemName: isFocused ? "arrowtriangle.right.fill" : "checkmark.circle")
                .font(.caption)
                .foregroundColor(isFocused ? .accentColor : .green)
                .frame(width: 14)
            Text(appNameForBundle(bundleId))
                .font(.callout)
            Spacer()
            Button(role: .destructive) {
                stateMonitor.setAppPaused(bundleId: bundleId, paused: nil)
            } label: {
                Image(systemName: "trash")
            }
            .buttonStyle(.plain)
            .help("Remove from allowlist (this app will be paused again)")
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 4)
        .background(
            isFocused
                ? Color.accentColor.opacity(0.10)
                : Color.clear
        )
        .cornerRadius(4)
    }

    private func appNameForBundle(_ bundleId: String) -> String {
        let workspace = NSWorkspace.shared
        if let bundle = NSRunningApplication.runningApplications(withBundleIdentifier: bundleId).first {
            return bundle.localizedName ?? bundleId
        }
        if let app = workspace.urlForApplication(withBundleIdentifier: bundleId),
           let bundle = Bundle(url: app),
           let displayName = bundle.infoDictionary?["CFBundleDisplayName"] as? String {
            return displayName
        }
        if let app = workspace.urlForApplication(withBundleIdentifier: bundleId) {
            return FileManager.default.displayName(atPath: app.path)
        }
        return bundleId
    }

    private var apiSection: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("API Configuration").font(.headline)
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
            Text("Required for transcription")
                .font(.caption2)
                .foregroundColor(.secondary)
        }
    }

    private var vocabularySection: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text("Vocabulary").font(.headline)
                Spacer()
                if vocabularyTermCount > 0 {
                    Text("\(vocabularyTermCount) terms")
                        .font(.caption)
                        .foregroundColor(.secondary)
                }
            }
            HStack(spacing: 8) {
                Button(action: openVocabularyFile) {
                    Label("Open glossary.json", systemImage: "doc.text")
                }
                .buttonStyle(.bordered)
                .controlSize(.small)

                Button(action: revealVocabularyInFinder) {
                    Label("Show in Finder", systemImage: "folder")
                }
                .buttonStyle(.bordered)
                .controlSize(.small)

                Button(action: refreshVocabularyInfo) {
                    Image(systemName: "arrow.clockwise")
                }
                .buttonStyle(.borderless)
                .help("Refresh vocabulary info")
                Spacer()
            }
            if !vocabularyPath.isEmpty {
                Text(vocabularyPath)
                    .font(.system(.caption2, design: .monospaced))
                    .foregroundColor(.secondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
        }
    }

    private func userGlossaryURL() -> URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent(".always")
            .appendingPathComponent("glossary.json")
    }

    private func ensureGlossaryExists(at url: URL) {
        let dir = url.deletingLastPathComponent()
        try? FileManager.default.createDirectory(
            at: dir, withIntermediateDirectories: true
        )
        if !FileManager.default.fileExists(atPath: url.path) {
            try? "[]".write(to: url, atomically: true, encoding: .utf8)
        }
    }

    private func openVocabularyFile() {
        let url = userGlossaryURL()
        ensureGlossaryExists(at: url)
        NSWorkspace.shared.open(url)
    }

    private func revealVocabularyInFinder() {
        let url = userGlossaryURL()
        ensureGlossaryExists(at: url)
        NSWorkspace.shared.activateFileViewerSelecting([url])
    }

    private func refreshVocabularyInfo() {
        let url = userGlossaryURL()
        vocabularyPath = url.path

        if FileManager.default.fileExists(atPath: url.path),
           let data = try? Data(contentsOf: url),
           let json = try? JSONSerialization.jsonObject(with: data) as? [[String: Any]] {
            vocabularyTermCount = json.count
        } else {
            vocabularyTermCount = 0
        }
    }
}
