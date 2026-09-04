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

            if currentPreset == nil {
                customSensitivityHint
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

            if currentPauseTolerance == nil {
                customPauseToleranceHint
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
                advancedThresholdsContent
            }
        }
    }

    // MARK: - Custom preset hints

    private var customSensitivityHint: some View {
        let hint = closestSensitivityHint()
        return HStack(spacing: 6) {
            Image(systemName: "wand.and.stars")
                .font(.caption2)
                .foregroundColor(.orange)
            Text(hint)
                .font(.caption2)
                .foregroundColor(.secondary)
        }
        .padding(.leading, 4)
    }

    private func closestSensitivityHint() -> String {
        let stt = config.sttEnergyThreshold
        let hear = config.hearEnergyThreshold
        var best: SensitivityPreset = .normal
        var bestDist = Double.infinity
        for p in SensitivityPreset.allCases {
            let (s, h) = p.thresholds
            let dist = abs(s - stt) + abs(h - hear)
            if dist < bestDist { bestDist = dist; best = p }
        }
        let (ps, ph) = best.thresholds
        return "Custom — closest to \(best.label) (STT \(String(format: "%.4f", ps)), Hear \(String(format: "%.4f", ph))). Yours: STT \(String(format: "%.4f", stt)), Hear \(String(format: "%.4f", hear))."
    }

    private var customPauseToleranceHint: some View {
        let closest = closestPauseTolerancePreset()
        return HStack(spacing: 6) {
            Image(systemName: "wand.and.stars")
                .font(.caption2)
                .foregroundColor(.orange)
            Text(closest)
                .font(.caption2)
                .foregroundColor(.secondary)
        }
        .padding(.leading, 4)
    }

    private func closestPauseTolerancePreset() -> String {
        let val = config.sttSilence
        var best: PauseTolerancePreset = .balanced
        var bestDist = Double.infinity
        for p in PauseTolerancePreset.allCases {
            let dist = abs(p.silenceSecs - val)
            if dist < bestDist { bestDist = dist; best = p }
        }
        return "Custom — closest to \(best.label) (\(String(format: "%.1f", best.silenceSecs))s). Yours: \(String(format: "%.2f", val))s."
    }

    // MARK: - Advanced thresholds

    private var advancedThresholdsContent: some View {
        VStack(alignment: .leading, spacing: 8) {
            NumericSettingRow(
                title: "STT Energy Threshold",
                help: "How loud your voice needs to be for Always to start transcribing. Lower = picks up quieter speech (but also more background noise). Higher = only loud, clear speech.",
                explanation: "How loud your voice needs to be for Always to start transcribing. Think of it as the 'volume gate' — below this level, Always stays silent. Lower if you speak softly, raise it if it's transcribing background noise.",
                formatter: SettingsWindow.energyFormatter,
                value: $config.sttEnergyThreshold,
                defaultValue: 0.012,
                range: 0.0001...0.5
            )
            NumericSettingRow(
                title: "Hear Energy Threshold",
                help: "How quiet a sound can be for Always to 'hear' that someone is speaking (before deciding if it's you). Lower = more sensitive to distant/quiet voices. Higher = ignores background chatter.",
                explanation: "A pre-check that decides 'is anyone speaking at all?' before running the full transcriber. Lower = catches quiet or distant voices. Higher = ignores background chatter and only reacts to clear, nearby speech.",
                formatter: SettingsWindow.energyFormatter,
                value: $config.hearEnergyThreshold,
                defaultValue: 0.001,
                range: 0.0001...0.5
            )
            NumericSettingRow(
                title: "Silence Timeout",
                help: "How many seconds of silence before Always finalizes what you said and pastes it. Lower = faster paste but may split sentences. Higher = waits longer so sentences stay together.",
                explanation: "How many seconds of silence before Always finalizes what you said and pastes it. Short = snappy but may cut you off mid-sentence. Long = patient, keeps sentences together, but slower to paste.",
                unit: "s",
                formatter: SettingsWindow.secondsFormatter,
                value: $config.sttSilence,
                defaultValue: 0.9,
                range: 0.3...15
            )
            NumericSettingRow(
                title: "Cooldown",
                help: "Minimum seconds between consecutive pastes. Prevents rapid-fire double-pastes. Lower = more responsive. Higher = more deliberate.",
                explanation: "A safety pause between consecutive pastes so two sentences don't get mashed together. Usually leave at default. Lower = more responsive, higher = more deliberate.",
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
                help: "How confident the voice activity detector must be that it hears actual speech (not noise). 0 = always on, 1 = only crystal-clear speech. 0.5 is a good middle ground.",
                explanation: "How confident the AI voice detector must be that it hears actual speech (not a door slam, keyboard, or fan). 0 = always on, 1 = only crystal-clear speech. 0.5 is a good middle ground. Raise it if it's transcribing background noise.",
                formatter: SettingsWindow.probabilityFormatter,
                value: $config.sileroThreshold,
                defaultValue: 0.5,
                range: 0...1
            )
        }
        .padding(.top, 6)
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
        }
    }
}
