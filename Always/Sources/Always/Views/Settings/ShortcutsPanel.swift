import SwiftUI

struct ShortcutsPanel: View {
    @ObservedObject var cliService: CLIService
    @Binding var config: Config

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 8) {
                Text("Keyboard Shortcuts").font(.headline)
                KeyCaptureButton(
                    label: "Master Pause / Mute",
                    shortcut: $config.shortcutMasterPause,
                    onSave: { value in
                        _ = try? await cliService.setConfig(key: "shortcut_master_pause", value: value)
                    }
                )
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
            .padding(20)
        }
    }
}
