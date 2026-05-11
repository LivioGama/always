import AppKit
import Foundation

/// Modal sheet that lets the user type the *intended* spelling for the
/// most recently transcribed word, after triggering the
/// `CorrectionDialogRequested` event from the daemon (default ⌃⌥W).
///
/// We deliberately keep this dumb: the daemon does the actual diffing
/// against `last_pasted` once it receives `LogCorrection { intended }`.
/// The dialog only collects text and surfaces the last transcript as a
/// hint so the user remembers what was just pasted.
final class CorrectionDialog {
    static let shared = CorrectionDialog()

    private var window: NSWindow?

    private init() {}

    func present(lastTranscript: String, onSubmit: @escaping (String) -> Void) {
        DispatchQueue.main.async {
            self.show(lastTranscript: lastTranscript, onSubmit: onSubmit)
        }
    }

    private func show(lastTranscript: String, onSubmit: @escaping (String) -> Void) {
        // Dismiss any previous instance — only one dialog at a time.
        window?.close()

        let panel = NSPanel(
            contentRect: NSRect(x: 0, y: 0, width: 420, height: 160),
            styleMask: [.titled, .closable, .utilityWindow],
            backing: .buffered,
            defer: false
        )
        panel.title = "Correct last transcript"
        panel.isFloatingPanel = true
        panel.level = .floating
        panel.center()

        let container = NSView(frame: panel.contentView!.bounds)
        container.autoresizingMask = [.width, .height]

        let hint = NSTextField(labelWithString: lastTranscript.isEmpty
            ? "(no recent transcript captured)"
            : "Last: “\(lastTranscript)”")
        hint.frame = NSRect(x: 16, y: 110, width: 388, height: 36)
        hint.font = .systemFont(ofSize: 11)
        hint.textColor = .secondaryLabelColor
        hint.lineBreakMode = .byTruncatingTail
        hint.maximumNumberOfLines = 2
        hint.cell?.wraps = true
        container.addSubview(hint)

        let input = NSTextField(frame: NSRect(x: 16, y: 64, width: 388, height: 28))
        input.placeholderString = "Type the intended word…"
        input.font = .systemFont(ofSize: 14)
        input.bezelStyle = .roundedBezel
        input.focusRingType = .default
        container.addSubview(input)

        let cancel = NSButton(title: "Cancel", target: nil, action: nil)
        cancel.frame = NSRect(x: 232, y: 16, width: 80, height: 32)
        cancel.bezelStyle = .rounded
        cancel.keyEquivalent = "\u{1B}" // Esc
        container.addSubview(cancel)

        let submit = NSButton(title: "Apply", target: nil, action: nil)
        submit.frame = NSRect(x: 324, y: 16, width: 80, height: 32)
        submit.bezelStyle = .rounded
        submit.keyEquivalent = "\r"
        container.addSubview(submit)

        panel.contentView = container

        // Use a holder to keep target alive while window is open.
        let target = ButtonTarget()
        cancel.target = target
        cancel.action = #selector(ButtonTarget.cancelTapped(_:))
        submit.target = target
        submit.action = #selector(ButtonTarget.submitTapped(_:))
        target.input = input
        target.onSubmit = { text in
            onSubmit(text)
            panel.close()
            self.window = nil
        }
        target.onCancel = {
            panel.close()
            self.window = nil
        }
        // Retain target via associated object — set as represented object.
        panel.contentView?.layer?.setValue(target, forKey: "buttonTarget")

        self.window = panel
        NSApp.activate(ignoringOtherApps: true)
        panel.makeKeyAndOrderFront(nil)
        panel.makeFirstResponder(input)
    }
}

/// Helper target/holder for the dialog buttons. Lives only as long as
/// the dialog is on screen (held by the panel's contentView layer).
final class ButtonTarget: NSObject {
    var onSubmit: ((String) -> Void)?
    var onCancel: (() -> Void)?
    weak var input: NSTextField?

    @objc func submitTapped(_ sender: Any) {
        let text = input?.stringValue ?? ""
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        if trimmed.isEmpty {
            onCancel?()
            return
        }
        onSubmit?(trimmed)
    }

    @objc func cancelTapped(_ sender: Any) {
        onCancel?()
    }
}
