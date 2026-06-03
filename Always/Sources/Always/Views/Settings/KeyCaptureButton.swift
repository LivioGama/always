import SwiftUI
import AppKit

/// Format `"ctrl+alt+p"` → `"⌃⌥P"` for display.
func formatShortcut(_ s: String) -> String {
    let symbolMap: [String: String] = [
        "ctrl": "⌃", "control": "⌃",
        "alt": "⌥", "option": "⌥",
        "shift": "⇧",
        "meta": "⌘", "cmd": "⌘", "command": "⌘"
    ]
    let parts = s.lowercased().split(separator: "+").map(String.init)
    return parts.map { symbolMap[$0] ?? $0.uppercased() }.joined()
}

/// A row that captures the next keystroke the user makes after pressing
/// it, then persists the resulting `mod+mod+key` string via `onSave`.
/// Used by the Shortcuts section in `SettingsWindow`.
struct KeyCaptureButton: View {
    let label: String
    @Binding var shortcut: String
    let onSave: (String) async -> Void

    @State private var isRecording = false
    @State private var monitor: Any?

    var body: some View {
        HStack {
            Text(label)
            Spacer()
            Button(action: toggleRecording) {
                Text(isRecording ? "Press keys…" : formatShortcut(shortcut))
                    .monospacedDigit()
                    .foregroundColor(isRecording ? .orange : .secondary)
                    .padding(.horizontal, 8)
                    .padding(.vertical, 3)
                    .background(isRecording ? Color.orange.opacity(0.08) : Color.clear)
                    .overlay(
                        RoundedRectangle(cornerRadius: 5)
                            .stroke(
                                isRecording ? Color.orange : Color.secondary.opacity(0.35),
                                lineWidth: 1
                            )
                    )
                    .cornerRadius(5)
            }
            .buttonStyle(.plain)
        }
    }

    private func toggleRecording() {
        if isRecording { stopRecording(); return }
        isRecording = true
        monitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { event in
            let mods = event.modifierFlags
            var parts: [String] = []
            if mods.contains(.control) { parts.append("ctrl") }
            if mods.contains(.option)  { parts.append("alt") }
            if mods.contains(.shift)   { parts.append("shift") }
            if mods.contains(.command) { parts.append("meta") }

            let keyChar = event.charactersIgnoringModifiers?.lowercased() ?? ""
            // Require at least one modifier + a single printable key.
            if !keyChar.isEmpty, keyChar.count == 1, !parts.isEmpty {
                let newShortcut = (parts + [keyChar]).joined(separator: "+")
                shortcut = newShortcut
                Task { await onSave(newShortcut) }
            }
            stopRecording()
            return nil
        }
    }

    private func stopRecording() {
        isRecording = false
        if let m = monitor { NSEvent.removeMonitor(m); monitor = nil }
    }
}
