import SwiftUI
import AppKit
import CoreGraphics

/// Format `"ctrl+alt+p"` → `"⌃⌥P"` for display.
func formatShortcut(_ s: String) -> String {
    let symbolMap: [String: String] = [
        "ctrl": "⌃", "control": "⌃",
        "alt": "⌥", "option": "⌥",
        "shift": "⇧",
        "meta": "⌘", "cmd": "⌘", "command": "⌘",
        "fn": "Fn"
    ]
    let parts = s.lowercased().split(separator: "+").map(String.init)
    return parts.map { symbolMap[$0] ?? $0.uppercased() }.joined()
}

/// Map a macOS keycode to a lowercase shortcut character.
private func keyCodeToChar(_ keyCode: Int64) -> String? {
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

/// Fn key keycode on macOS.
private let FN_KEYCODE: Int64 = 63

/// Holds the recording state and callbacks for the CGEventTap capture.
/// Must be a class (reference type) so the C callback can hold an
/// unretained reference and so `objc_setAssociatedObject` is not needed.
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

/// A row that captures the next keystroke the user makes after pressing
/// it, then persists the resulting `mod+mod+key` string via `onSave`.
/// Used by the Shortcuts section in `SettingsWindow`.
///
/// Uses a `CGEventTap` (not `NSEvent.addLocalMonitorForEvents`) so it can
/// capture the Fn/Globe key and modifier-only shortcuts that the standard
/// NSEvent local monitor cannot see. The tap is listen-only and lives
/// only while recording is active — it is torn down the moment a shortcut
/// is captured or recording is cancelled.
struct KeyCaptureButton: View {
    let label: String
    @Binding var shortcut: String
    let onSave: (String) async -> Void

    @State private var isRecording = false
    @State private var captureCtx = CaptureContext()
    @State private var eventTap: CFMachPort?
    @State private var runLoopSource: CFRunLoopSource?
    @State private var nseventMonitor: Any?

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
        startCGEventTap()
    }

    // MARK: - CGEventTap capture

    /// Start a listen-only CGEventTap that sees all key events including Fn.
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

                if type == .flagsChanged {
                    let keyCode = event.getIntegerValueField(.keyboardEventKeycode)
                    // Fn key fires as flagsChanged with keycode 63.
                    if keyCode == FN_KEYCODE {
                        ctx.fire("fn")
                    }
                    return Unmanaged.passRetained(event)
                }

                if type == .keyDown {
                    let flags = event.flags
                    let keyCode = event.getIntegerValueField(.keyboardEventKeycode)

                    // Fn key can also fire as keyDown.
                    if keyCode == FN_KEYCODE {
                        ctx.fire("fn")
                        return Unmanaged.passRetained(event)
                    }

                    var parts: [String] = []
                    if flags.contains(.maskControl) { parts.append("ctrl") }
                    if flags.contains(.maskAlternate) { parts.append("alt") }
                    if flags.contains(.maskShift) { parts.append("shift") }
                    if flags.contains(.maskCommand) { parts.append("meta") }

                    if let keyChar = keyCodeToChar(keyCode), !parts.isEmpty {
                        let newShortcut = (parts + [keyChar]).joined(separator: "+")
                        ctx.fire(newShortcut)
                    }
                    return Unmanaged.passRetained(event)
                }

                return Unmanaged.passRetained(event)
            },
            userInfo: pointer
        ) else {
            // CGEventTap creation failed — likely no Input Monitoring
            // permission. Fall back to NSEvent monitor so standard
            // shortcuts still work (just not Fn).
            Unmanaged<CaptureContext>.fromOpaque(pointer).release()
            startNSEventFallback()
            return
        }

        eventTap = tap
        let rlSource = CFMachPortCreateRunLoopSource(kCFAllocatorDefault, tap, 0)
        runLoopSource = rlSource
        CFRunLoopAddSource(CFRunLoopGetCurrent(), rlSource, .commonModes)
        CGEvent.tapEnable(tap: tap, enable: true)
    }

    /// Fallback: NSEvent local monitor for when CGEventTap isn't available.
    /// Can't see Fn, but handles all standard ctrl+alt+shift+key combos.
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
        captureCtx.handler = nil
    }
}
