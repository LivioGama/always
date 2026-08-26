import SwiftUI

struct BehaviorPanel: View {
    @ObservedObject var cliService: CLIService
    @ObservedObject var stateMonitor: StateMonitor
    @Binding var config: Config

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                sensitivitySection
                Divider()
                autoEnterSection
                Divider()
                postprocessSection
                Divider()
                idleAutoPauseSection
            }
            .padding(20)
        }
    }

    private var sensitivitySection: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Sensitivity").font(.headline)

            HStack {
                Text("Mic Sensitivity")
                    .font(.body)
                Spacer()
                Picker("", selection: presetBinding) {
                    ForEach(SensitivityPreset.allCases) { preset in
                        Text(preset.label).tag(Optional(preset))
                    }
                    if currentPreset == nil {
                        Text("Custom").tag(Optional<SensitivityPreset>.none)
                    }
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .frame(width: 280)
            }

            HStack {
                Text("Pause Tolerance")
                    .font(.body)
                    .help(
                        "How long a mid-sentence pause can be before the utterance is finalized and pasted. Higher = fewer split sentences, slightly later paste."
                    )
                Spacer()
                Picker("", selection: pauseToleranceBinding) {
                    ForEach(PauseTolerancePreset.allCases) { preset in
                        Text(preset.label).tag(Optional(preset))
                    }
                    if currentPauseTolerance == nil {
                        Text("Custom").tag(Optional<PauseTolerancePreset>.none)
                    }
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .frame(width: 280)
            }
            Toggle(
                "Extend automatically when a sentence sounds unfinished",
                isOn: $config.sttAdaptiveSilence
            )
            .help(
                "When the transcript-so-far ends mid-thought (no punctuation, trailing 'and'/'which'/…), the pause window is temporarily doubled so the sentence isn't split."
            )
            .font(.caption)
            Toggle(
                "Show live transcript while you speak",
                isOn: $config.sttLivePreview
            )
            .help(
                "Periodically transcribes what you've said so far and shows the provisional text in the overlay while you're still talking. Uses a few extra transcription calls per utterance on the cloud backend."
            )
            .font(.caption)

            DisclosureGroup("Advanced thresholds") {
                VStack(alignment: .leading, spacing: 8) {
                    NumericSettingRow(
                        title: "STT Energy Threshold",
                        help: "Lower = more sensitive to quiet speech",
                        formatter: SettingsWindow.energyFormatter,
                        value: $config.sttEnergyThreshold,
                        defaultValue: 0.012,
                        range: 0.0001...0.5
                    )
                    NumericSettingRow(
                        title: "Hear Energy Threshold",
                        help: "Lower = picks up quieter background voice",
                        formatter: SettingsWindow.energyFormatter,
                        value: $config.hearEnergyThreshold,
                        defaultValue: 0.001,
                        range: 0.0001...0.5
                    )
                    NumericSettingRow(
                        title: "Silence Timeout",
                        help: "Seconds of silence before utterance ends",
                        unit: "s",
                        formatter: SettingsWindow.secondsFormatter,
                        value: $config.sttSilence,
                        defaultValue: 0.9,
                        range: 0.3...15
                    )
                    NumericSettingRow(
                        title: "Cooldown",
                        help: "Min seconds between consecutive pastes",
                        unit: "s",
                        formatter: SettingsWindow.cooldownSecondsFormatter,
                        value: Binding(
                            get: { Double(config.sttCooldownMs) / 1000.0 },
                            set: { config.sttCooldownMs = Int(($0 * 1000).rounded()) }
                        ),
                        defaultValue: 0.150,
                        range: 0...60.0
                    )
                    NumericSettingRow(
                        title: "Silero VAD Threshold",
                        help: "Higher = stricter speech detection",
                        formatter: SettingsWindow.probabilityFormatter,
                        value: $config.sileroThreshold,
                        defaultValue: 0.5,
                        range: 0...1
                    )
                }
                .padding(.top, 6)
            }
        }
    }

    private var pauseToleranceBinding: Binding<PauseTolerancePreset?> {
        Binding(
            get: { self.currentPauseTolerance },
            set: { newValue in
                guard let preset = newValue else { return }
                config.sttSilence = preset.silenceSecs
            }
        )
    }

    private var currentPauseTolerance: PauseTolerancePreset? {
        PauseTolerancePreset.from(silenceSecs: config.sttSilence)
    }

    private var presetBinding: Binding<SensitivityPreset?> {
        Binding(
            get: { self.currentPreset },
            set: { newValue in
                guard let preset = newValue else { return }
                let (stt, hear) = preset.thresholds
                config.sttEnergyThreshold = stt
                config.hearEnergyThreshold = hear
            }
        )
    }

    private var currentPreset: SensitivityPreset? {
        SensitivityPreset.from(
            stt: config.sttEnergyThreshold,
            hear: config.hearEnergyThreshold
        )
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
