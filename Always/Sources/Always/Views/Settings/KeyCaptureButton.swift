import SwiftUI
import AppKit
import CoreGraphics

// MARK: - Shortcut parsing / formatting helpers

/// Split `"ctrl+alt+shift+p"` → `["ctrl", "alt", "shift", "p"]`
func parseShortcutParts(_ s: String) -> [String] {
    s.lowercased().split(separator: "+").map { $0.trimmingCharacters(in: .whitespaces) }
}

/// Map a part string to its display symbol.
func partSymbol(_ part: String) -> String {
    switch part {
    case "ctrl", "control": return "⌃"
    case "alt", "option": return "⌥"
    case "shift": return "⇧"
    case "meta", "cmd", "command": return "⌘"
    case "fn": return "Fn"
    case "space": return "Space"
    default: return part.uppercased()
    }
}

// MARK: - KeyCaptureButton

/// A shortcut recorder row inspired by iris-sama's settings-shortcuts.
///
/// Features:
/// - Displays the current shortcut as individual keycaps (⌃ ⌥ ⇧ P)
/// - Click to start recording — button highlights, shows "Press keys…"
/// - Captures any key combo via CGEventTap (including Fn/Globe key)
/// - Clear (×) button to unset the shortcut
/// - Falls back to NSEvent monitor if CGEventTap isn't available
struct KeyCaptureButton: View {
    let label: String
    let description: String?
    @Binding var shortcut: String
    let onSave: (String) async -> Void

    @State private var isRecording = false
    @State private var captureCtx = CaptureContext()
    @State private var eventTap: CFMachPort?
    @State private var runLoopSource: CFRunLoopSource?
    @State private var nseventMonitor: Any?
    @State private var captureCtxPointer: UnsafeMutableRawPointer?

    init(label: String,
         description: String? = nil,
         shortcut: Binding<String>,
         onSave: @escaping (String) async -> Void) {
        self.label = label
        self.description = description
        self._shortcut = shortcut
        self.onSave = onSave
    }

    var body: some View {
        HStack(spacing: 14) {
            // Left: name + description
            VStack(alignment: .leading, spacing: 3) {
                Text(label)
                    .font(.system(size: 13, weight: .medium))
                    .foregroundColor(.primary)
                if let desc = description {
                    Text(desc)
                        .font(.system(size: 10.5))
                        .foregroundColor(.secondary)
                }
            }
            Spacer(minLength: 10)

            // Right: recorder + clear
            HStack(spacing: 7) {
                recorderButton
                clearButton
            }
        }
        .padding(.vertical, 6)
    }

    // MARK: Recorder button

    private var recorderButton: some View {
        let parts = parseShortcutParts(shortcut)
        return Button(action: toggleRecording) {
            HStack(spacing: 4) {
                if isRecording {
                    Text("Press keys…")
                        .font(.system(size: 12))
                        .foregroundColor(.orange)
                } else if parts.isEmpty {
                    Text("Not set")
                        .font(.system(size: 12))
                        .foregroundColor(.secondary.opacity(0.6))
                } else {
                    ForEach(parts, id: \.self) { part in
                        keycap(partSymbol(part))
                    }
                }
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 6)
            .frame(minWidth: 120, minHeight: 30)
            .background(
                RoundedRectangle(cornerRadius: 7)
                    .fill(isRecording
                          ? Color.orange.opacity(0.12)
                          : Color(NSColor.controlBackgroundColor))
            )
            .overlay(
                RoundedRectangle(cornerRadius: 7)
                    .stroke(isRecording
                            ? Color.orange
                            : Color.secondary.opacity(0.3),
                            lineWidth: 1)
            )
        }
        .buttonStyle(.plain)
    }

    /// A single keycap chip: rounded rect with the symbol centered.
    private func keycap(_ symbol: String) -> some View {
        Text(symbol)
            .font(.system(size: 11, weight: .medium))
            .foregroundColor(.primary)
            .padding(.horizontal, 6)
            .frame(minWidth: 24, minHeight: 22)
            .background(
                RoundedRectangle(cornerRadius: 5)
                    .fill(Color(NSColor.windowBackgroundColor).opacity(0.8))
            )
            .overlay(
                RoundedRectangle(cornerRadius: 5)
                    .stroke(Color.secondary.opacity(0.35), lineWidth: 1)
            )
    }

    // MARK: Clear button

    private var clearButton: some View {
        Button(action: clearShortcut) {
            Text("×")
                .font(.system(size: 15, weight: .medium))
                .foregroundColor(.secondary)
                .frame(width: 28, height: 28)
                .background(
                    RoundedRectangle(cornerRadius: 7)
                        .stroke(Color.secondary.opacity(0.3), lineWidth: 1)
                )
        }
        .buttonStyle(.plain)
        .opacity(shortcut.isEmpty ? 0 : 1)
        .disabled(shortcut.isEmpty)
        .help("Clear shortcut")
    }

    private func clearShortcut() {
        shortcut = ""
        Task { await onSave("") }
    }

    // MARK: Recording

    private func toggleRecording() {
        if isRecording { stopRecording(); return }
        isRecording = true
        startCGEventTap()
    }

    // MARK: CGEventTap capture

    private static let FN_KEYCODE: Int64 = 63

    private final class CaptureContext {
        var handler: ((String?) -> Void)?
        private var fired = false
        func fire(_ result: String?) {
            guard !fired else { return }
            fired = true
            handler?(result)
        }
        func reset() { fired = false }
    }

    /// Map a macOS keycode to a lowercase shortcut character.
    private static func keyCodeToChar(_ keyCode: Int64) -> String? {
        let map: [Int64: String] = [
            0: "a", 1: "s", 2: "d", 3: "f", 4: "h", 5: "g", 6: "z", 7: "x",
            8: "c", 9: "v", 11: "b", 12: "q", 13: "w", 14: "e", 15: "r",
            16: "y", 17: "t", 31: "o", 32: "u", 34: "i", 35: "p", 37: "l",
            38: "j", 40: "k", 45: "n", 46: "m",
            18: "1", 19: "2", 20: "3", 21: "4", 23: "5", 22: "6", 26: "7",
            28: "8", 25: "9", 29: "0",
            49: "space",
        ]
        return map[keyCode]
    }

    private func startCGEventTap() {
        let mask = (1 << CGEventType.keyDown.rawValue)
            | (1 << CGEventType.flagsChanged.rawValue)

        captureCtx.reset()
        captureCtx.handler = { [self] result in
            DispatchQueue.main.async {
                if let newShortcut = result {
                    shortcut = newShortcut
                    Task { await onSave(newShortcut) }
                }
                stopRecording()
            }
        }

        let pointer = Unmanaged.passRetained(captureCtx).toOpaque()
        captureCtxPointer = pointer

        guard let tap = CGEvent.tapCreate(
            tap: .cgSessionEventTap,
            place: .headInsertEventTap,
            options: .listenOnly,
            eventsOfInterest: CGEventMask(mask),
            callback: { _, type, event, refcon in
                guard let refcon = refcon else { return Unmanaged.passRetained(event) }
                let ctx = Unmanaged<CaptureContext>.fromOpaque(refcon).takeUnretainedValue()

                if type == .tapDisabledByTimeout || type == .tapDisabledByUserInput {
                    return Unmanaged.passRetained(event)
                }

                let keyCode = event.getIntegerValueField(.keyboardEventKeycode)
                let flags = event.flags

                // Fn key fires as flagsChanged with keycode 63.
                if type == .flagsChanged {
                    if keyCode == Self.FN_KEYCODE {
                        ctx.fire("fn")
                    }
                    return Unmanaged.passRetained(event)
                }

                if type == .keyDown {
                    // Fn key can also fire as keyDown.
                    if keyCode == Self.FN_KEYCODE {
                        ctx.fire("fn")
                        return Unmanaged.passRetained(event)
                    }

                    var parts: [String] = []
                    if flags.contains(.maskControl) { parts.append("ctrl") }
                    if flags.contains(.maskAlternate) { parts.append("alt") }
                    if flags.contains(.maskShift) { parts.append("shift") }
                    if flags.contains(.maskCommand) { parts.append("meta") }

                    if let keyChar = Self.keyCodeToChar(keyCode), !parts.isEmpty {
                        ctx.fire((parts + [keyChar]).joined(separator: "+"))
                    }
                    return Unmanaged.passRetained(event)
                }

                return Unmanaged.passRetained(event)
            },
            userInfo: pointer
        ) else {
            Unmanaged<CaptureContext>.fromOpaque(pointer).release()
            captureCtxPointer = nil
            startNSEventFallback()
            return
        }

        eventTap = tap
        let rlSource = CFMachPortCreateRunLoopSource(kCFAllocatorDefault, tap, 0)
        runLoopSource = rlSource
        CFRunLoopAddSource(CFRunLoopGetCurrent(), rlSource, .commonModes)
        CGEvent.tapEnable(tap: tap, enable: true)
    }

    private func startNSEventFallback() {
        nseventMonitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { event in
            let mods = event.modifierFlags
            var parts: [String] = []
            if mods.contains(.control) { parts.append("ctrl") }
            if mods.contains(.option)  { parts.append("alt") }
            if mods.contains(.shift)   { parts.append("shift") }
            if mods.contains(.command) { parts.append("meta") }

            let keyChar = event.charactersIgnoringModifiers?.lowercased() ?? ""
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
        if let tap = eventTap {
            CGEvent.tapEnable(tap: tap, enable: false)
            if let source = runLoopSource {
                CFRunLoopRemoveSource(CFRunLoopGetCurrent(), source, .commonModes)
            }
            eventTap = nil
            runLoopSource = nil
        }
        if let monitor = nseventMonitor {
            NSEvent.removeMonitor(monitor)
            nseventMonitor = nil
        }
        if let pointer = captureCtxPointer {
            Unmanaged<CaptureContext>.fromOpaque(pointer).release()
            captureCtxPointer = nil
        }
        captureCtx.handler = nil
    }
}
