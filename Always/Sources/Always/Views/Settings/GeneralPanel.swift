import SwiftUI
import AppKit

struct GeneralPanel: View {
    @ObservedObject var stateMonitor: StateMonitor
    @ObservedObject var focusedApp: FocusedAppMonitor
    @Binding var config: Config
    @AppStorage(OverlayDisplayMode.defaultsKey)
    private var overlayDisplayMode = OverlayDisplayMode.normal.rawValue

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                PermissionsBanner()
                soundCuesSection
                Divider()
                overlaySection
                Divider()
                allowlistSection
            }
            .padding(20)
        }
    }

    private var soundCuesSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 10) {
                Image(systemName: config.audibleStatusSound == "off" ? "speaker.slash.fill" : "speaker.wave.2.fill")
                    .font(.title3)
                    .foregroundColor(config.audibleStatusSound == "off" ? .secondary : .accentColor)
                    .frame(width: 24)
                VStack(alignment: .leading, spacing: 2) {
                    Text("Sound")
                        .font(.headline)
                    Text("Play distinct sounds for listening, transcribing, success, and failure.")
                        .font(.caption)
                        .foregroundColor(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
                Spacer()
                Picker("", selection: $config.audibleStatusSound) {
                    Text("Off").tag("off")
                    Text("Low").tag("low")
                    Text("Medium").tag("medium")
                    Text("High").tag("high")
                }
                    .pickerStyle(.segmented)
                    .labelsHidden()
                    .frame(width: 260)
            }
        }
        .padding(10)
        .background(Color.secondary.opacity(0.08))
        .cornerRadius(6)
    }

    private var overlaySection: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Status Overlay").font(.headline)
            HStack {
                Text("Listening / transcribing indicator")
                    .font(.body)
                    .help(
                        "The floating indicator shown while you dictate. Compact shrinks it to a small pill; Hidden never shows it — the menu bar icon still reflects listening and transcribing."
                    )
                Spacer()
                Picker("", selection: $overlayDisplayMode) {
                    ForEach(OverlayDisplayMode.allCases) { mode in
                        Text(mode.label).tag(mode.rawValue)
                    }
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .frame(width: 280)
            }
            Text("Takes effect the next time the indicator appears — no restart needed. The menu bar icon always shows the live state (mic while hearing you, waveform circle while transcribing).")
                .font(.caption)
                .foregroundColor(.secondary)
        }
    }

    private var allowlistSection: some View {
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
}
