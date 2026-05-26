import SwiftUI

struct AdvancedPanel: View {
    @ObservedObject var cliService: CLIService
    @Binding var config: Config

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                sensitivitySection
                Divider()
                shortcutsSection
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

            DisclosureGroup("Advanced") {
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
                        defaultValue: 2.0,
                        range: 0.1...10
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

    private var shortcutsSection: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Keyboard Shortcuts").font(.headline)
            KeyCaptureButton(
                label: "Pause / Resume",
                shortcut: $config.shortcutPause,
                onSave: { value in
                    _ = try? await cliService.setConfig(key: "shortcut_pause", value: value)
                }
            )
            KeyCaptureButton(
                label: "Toggle Auto-Enter",
                shortcut: $config.shortcutAutoEnter,
                onSave: { value in
                    _ = try? await cliService.setConfig(key: "shortcut_auto_enter", value: value)
                }
            )
            KeyCaptureButton(
                label: "Paste Last Filtered",
                shortcut: $config.shortcutForcePaste,
                onSave: { value in
                    _ = try? await cliService.setConfig(key: "shortcut_force_paste", value: value)
                }
            )
            KeyCaptureButton(
                label: "Correction Dialog",
                shortcut: $config.shortcutCorrectionDialog,
                onSave: { value in
                    _ = try? await cliService.setConfig(key: "shortcut_correction_dialog", value: value)
                }
            )
            Text("Shortcut changes take effect on next launch.")
                .font(.caption2)
                .foregroundColor(.secondary)
        }
    }
}
