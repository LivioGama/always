import SwiftUI

struct BehaviorPanel: View {
    @ObservedObject var cliService: CLIService
    @ObservedObject var stateMonitor: StateMonitor
    @Binding var config: Config

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                autoEnterSection
                Divider()
                postprocessSection
                Divider()
                idleAutoPauseSection
            }
            .padding(20)
        }
    }

    private var autoEnterSection: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Auto-Enter").font(.headline)
            HStack {
                Toggle(
                    "Press Return after each paste",
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
                title: "Delay",
                help: "Seconds before Return is pressed. Set to 0 for immediate Return.",
                unit: "s",
                formatter: SettingsWindow.intFormatter,
                value: Binding(
                    get: { config.autoEnterDelayMs / 1000 },
                    set: { config.autoEnterDelayMs = $0 * 1000 }
                ),
                defaultValue: 4,
                range: 0...60
            )
        }
    }

    private var postprocessSection: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Postprocessing").font(.headline)
            Toggle(
                "Smart Postprocess (LLM grammar + glossary fixes)",
                isOn: $config.postprocessEnabled
            )
            Text("Runs after each transcription. Requires a Groq API key (Advanced tab).")
                .font(.caption)
                .foregroundColor(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    private var idleAutoPauseSection: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Idle Auto-Pause").font(.headline)
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
}
