import SwiftUI

struct ShortcutsPanel: View {
    @ObservedObject var cliService: CLIService
    @Binding var config: Config

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 10) {
                Text("Keyboard Shortcuts")
                    .font(.system(size: 13, weight: .semibold))
                    .padding(.bottom, 4)

                KeyCaptureButton(
                    label: "Master Pause / Mute",
                    description: "Globally pause or resume all dictation.",
                    shortcut: $config.shortcutMasterPause,
                    onSave: { value in
                        _ = try? await cliService.setConfig(key: "shortcut_master_pause", value: value)
                    }
                )
                KeyCaptureButton(
                    label: "Pause / Resume",
                    description: "Toggle pause for the currently focused app only.",
                    shortcut: $config.shortcutPause,
                    onSave: { value in
                        _ = try? await cliService.setConfig(key: "shortcut_pause", value: value)
                    }
                )
                KeyCaptureButton(
                    label: "Toggle Auto-Enter",
                    description: "Enable or disable automatic Enter after pasting.",
                    shortcut: $config.shortcutAutoEnter,
                    onSave: { value in
                        _ = try? await cliService.setConfig(key: "shortcut_auto_enter", value: value)
                    }
                )
                KeyCaptureButton(
                    label: "Paste Last Filtered",
                    description: "Re-paste the most recent filtered transcript.",
                    shortcut: $config.shortcutForcePaste,
                    onSave: { value in
                        _ = try? await cliService.setConfig(key: "shortcut_force_paste", value: value)
                    }
                )
                KeyCaptureButton(
                    label: "Correction Dialog",
                    description: "Open a dialog to correct the last transcript.",
                    shortcut: $config.shortcutCorrectionDialog,
                    onSave: { value in
                        _ = try? await cliService.setConfig(key: "shortcut_correction_dialog", value: value)
                    }
                )

                Text("Shortcut changes take effect on next launch.")
                    .font(.caption2)
                    .foregroundColor(.secondary)
                    .padding(.top, 8)
            }
            .padding(20)
        }
    }
}
