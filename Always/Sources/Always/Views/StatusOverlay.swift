import AppKit
import CoreGraphics
import os.log

private let overlayLogger = Logger(subsystem: "com.always.app", category: "status-overlay")

/// Resolves the display containing the frontmost app's main window — more
/// reliable than mouse position when a native fullscreen app owns a Space.
private enum OverlayScreenPlacement {
    /// CGWindow bounds use a top-left origin; AppKit screens use bottom-left.
    private static func appKitFrame(fromCGWindowBounds cgBounds: CGRect) -> CGRect {
        let globalMaxY = NSScreen.screens.map(\.frame.maxY).max() ?? 0
        return CGRect(
            x: cgBounds.origin.x,
            y: globalMaxY - cgBounds.origin.y - cgBounds.height,
            width: cgBounds.width,
            height: cgBounds.height
        )
    }

    private static func screenContaining(point: NSPoint) -> NSScreen? {
        NSScreen.screens.first { NSMouseInRect(point, $0.frame, false) } ?? NSScreen.main
    }

    /// The display the mouse cursor is currently on. Always resolves to a
    /// screen (falls back to main), and — unlike the frontmost window — is
    /// valid even over fullscreen Spaces, so it's a safe primary signal for
    /// "put the HUD where the user is looking right now".
    static func screenForMouse() -> NSScreen? {
        screenContaining(point: NSEvent.mouseLocation)
    }

    static func screenForFrontmostApp() -> NSScreen? {
        guard let frontApp = NSWorkspace.shared.frontmostApplication else {
            return screenContaining(point: NSEvent.mouseLocation)
        }
        let pid = frontApp.processIdentifier
        guard let windowList = CGWindowListCopyWindowInfo(
            [.optionOnScreenOnly, .excludeDesktopElements],
            kCGNullWindowID
        ) as? [[String: Any]] else {
            return screenContaining(point: NSEvent.mouseLocation)
        }

        var bestFrame = CGRect.zero
        var bestArea: CGFloat = 0

        for entry in windowList {
            guard let ownerPID = entry[kCGWindowOwnerPID as String] as? Int32,
                  ownerPID == pid,
                  let layer = entry[kCGWindowLayer as String] as? Int,
                  layer == 0,
                  let boundsDict = entry[kCGWindowBounds as String] as? [String: CGFloat],
                  let x = boundsDict["X"],
                  let y = boundsDict["Y"],
                  let w = boundsDict["Width"],
                  let h = boundsDict["Height"] else {
                continue
            }
            let frame = appKitFrame(fromCGWindowBounds: CGRect(x: x, y: y, width: w, height: h))
            let area = frame.width * frame.height
            if area > bestArea {
                bestArea = area
                bestFrame = frame
            }
        }

        if bestArea > 0 {
            var targetScreen: NSScreen?
            var maxIntersection: CGFloat = 0
            for screen in NSScreen.screens {
                let intersection = bestFrame.intersection(screen.frame)
                let area = intersection.width * intersection.height
                if area > maxIntersection {
                    maxIntersection = area
                    targetScreen = screen
                }
            }
            if let targetScreen {
                return targetScreen
            }
            return screenContaining(point: NSPoint(x: bestFrame.midX, y: bestFrame.midY))
        }

        return screenContaining(point: NSEvent.mouseLocation)
    }

    static func volumeHudOrigin(on screen: NSScreen, width: CGFloat, height: CGFloat) -> NSPoint {
        let screenFrame = screen.visibleFrame
        let targetX = (screenFrame.width - width) / 2 + screenFrame.minX
        let targetY = screenFrame.minY + 140
        return NSPoint(x: targetX, y: targetY)
    }

    static func bottomRightOrigin(on screen: NSScreen, width: CGFloat, height: CGFloat, margin: CGFloat = 20) -> NSPoint {
        let screenFrame = screen.visibleFrame
        let targetX = screenFrame.maxX - width - margin
        let targetY = screenFrame.minY + margin
        return NSPoint(x: targetX, y: targetY)
    }
}

/// User-facing size/visibility of the status HUD ("Listening" /
/// "Transcribing" / flashes). GUI-side preference in UserDefaults — the
/// daemon has no say in presentation. Read at every show so a settings
/// change applies to the next indicator without a restart.
enum OverlayDisplayMode: String, CaseIterable, Identifiable {
    case normal
    case compact
    case hidden

    var id: String { rawValue }

    var label: String {
        switch self {
        case .normal: return "Normal"
        case .compact: return "Compact"
        case .hidden: return "Hidden"
        }
    }

    static let defaultsKey = "overlayDisplayMode"

    static var current: OverlayDisplayMode {
        OverlayDisplayMode(
            rawValue: UserDefaults.standard.string(forKey: defaultsKey) ?? ""
        ) ?? .normal
    }
}

enum OverlayState: Equatable, Hashable {
    case paused
    case resumed
    case autoEnterOn
    case autoEnterOff
    case transcribing
    case processing
    case voiceActivity
    case filtered(reason: String)
    case correctionSaved(wrong: String, right: String)
    case correctionEmpty(reason: String)
    /// Auto-enter countdown overlay. Whole seconds remaining.
    case autoEnterCountdown(secondsRemaining: Int)
    /// Idle auto-pause notice (briefly shown when daemon goes idle).
    case idleAutoPaused(seconds: Int)
    /// Microphone volume warning - energy level is too low for reliable detection.
    case lowMicrophoneVolume(energy: Double)
    /// Async grammar correction silently replaced the pasted text.
    case grammarCorrected
    case transcriptionFailed(message: String)
    /// Groq unreachable — daemon degraded to the named local model.
    case sttFallback(model: String)
    /// A pause hotkey resolved to a concrete scope — flash exactly what
    /// was toggled so the chord never feels like it did something random.
    /// `target` is "everywhere" for master scope or the app's display
    /// name for app scope.
    case pauseScope(target: String, paused: Bool)
    /// Per-app pause chord fired with nothing to toggle (no real app
    /// focused, or Always itself frontmost).
    case pauseScopeNoApp
    /// Continuous recording crossed the warning threshold; shows when
    /// the hard cap will cut it.
    case longRecording(capMinutes: Int)
    /// Transcription has been running for a while — same persistent HUD
    /// as `.transcribing` but with the elapsed time so a long wait
    /// reads as progress, not a hang.
    case transcribingElapsed(seconds: Int)
    /// A watchdog paused listening — explains WHY ("Zoom is using the
    /// mic") instead of the generic Paused badge.
    case pausedExternal(reason: String)

    /// Persistent HUD states that should appear instantly (no fade-in).
    var isInstantShow: Bool {
        switch self {
        case .voiceActivity, .transcribing, .transcribingElapsed, .autoEnterCountdown:
            return true
        default:
            return false
        }
    }

    var rawValue: String {
        switch self {
        case .paused: return "Paused"
        case .resumed: return "Resumed"
        case .autoEnterOn: return "Auto-Enter On"
        case .autoEnterOff: return "Auto-Enter Off"
        case .transcribing: return "Transcribing"
        case .processing: return "Processing"
        case .voiceActivity: return "Listening"
        case .filtered(let reason): return reason.isEmpty ? "Filtered" : "Filtered · \(reason)"
        case .correctionSaved(let wrong, let right): return "Saved: \(wrong) → \(right)"
        case .correctionEmpty(let reason): return reason.isEmpty ? "Nothing to fix" : reason
        case .autoEnterCountdown(let s): return "Auto-Enter in \(s)s · any key cancels"
        case .idleAutoPaused(let s): return "Idle for \(s)s · paused"
        case .lowMicrophoneVolume(let energy): return String(format: "Low mic volume · energy %.3f", energy)
        case .grammarCorrected: return "✓ Grammar corrected"
        case .transcriptionFailed(let message): return message.isEmpty ? "Transcription failed" : message
        case .sttFallback(let model): return "Offline · using \(model)"
        case .pauseScope(let target, let paused):
            return paused ? "Paused · \(target)" : "Resumed · \(target)"
        case .pauseScopeNoApp: return "No app focused · ⌃⌥⇧P pauses everywhere"
        case .longRecording(let capMinutes): return "Long recording · cuts at \(capMinutes):00"
        case .transcribingElapsed(let s): return "Transcribing… \(s)s"
        case .pausedExternal(let reason): return "Paused · \(reason)"
        }
    }

    var iconName: String {
        switch self {
        case .paused: return "pause.fill"
        case .resumed: return "play.fill"
        case .autoEnterOn: return "checkmark.circle.fill"
        case .autoEnterOff: return "circle"
        case .transcribing: return "waveform.circle.fill"
        case .processing: return "waveform.circle"
        case .voiceActivity: return "waveform"
        case .filtered: return "xmark.octagon.fill"
        case .correctionSaved: return "checkmark.seal.fill"
        case .correctionEmpty: return "questionmark.circle"
        case .autoEnterCountdown: return "return"
        case .idleAutoPaused: return "moon.zzz.fill"
        case .lowMicrophoneVolume: return "speaker.slash.fill"
        case .grammarCorrected: return "sparkles"
        case .transcriptionFailed: return "exclamationmark.triangle.fill"
        case .sttFallback: return "wifi.slash"
        case .pauseScope(_, let paused): return paused ? "pause.fill" : "play.fill"
        case .pauseScopeNoApp: return "questionmark.circle"
        case .longRecording: return "timer"
        case .transcribingElapsed: return "waveform.circle.fill"
        case .pausedExternal: return "pause.circle.fill"
        }
    }

    var color: NSColor {
        switch self {
        case .paused: return .systemOrange
        case .resumed: return .systemTeal
        case .autoEnterOn: return .systemGreen
        case .autoEnterOff: return .systemGray
        case .transcribing: return .systemPurple
        case .processing: return .systemBlue
        case .voiceActivity: return .systemRed
        case .filtered: return .systemPink
        case .correctionSaved: return .systemGreen
        case .correctionEmpty: return .systemGray
        case .autoEnterCountdown: return .systemYellow
        case .idleAutoPaused: return .systemOrange
        case .lowMicrophoneVolume: return .systemRed
        case .grammarCorrected: return .systemTeal
        case .transcriptionFailed: return .systemRed
        case .sttFallback: return .systemOrange
        case .pauseScope(_, let paused): return paused ? .systemOrange : .systemTeal
        case .pauseScopeNoApp: return .systemGray
        case .longRecording: return .systemYellow
        case .transcribingElapsed: return .systemPurple
        case .pausedExternal: return .systemOrange
        }
    }
}

/// Three-dot wave animation used for ongoing states (listening / processing /
/// transcribing). Three white circles bounce vertically in a sine-wave loop
/// with a 1/3-period phase offset between neighbors so the wave appears to
/// ripple across.
fileprivate class DotWaveView: NSView {
    private let dotCount = 3
    private let dotDiameter: CGFloat
    private let dotSpacing: CGFloat
    private let amplitude: CGFloat
    private let period: CFTimeInterval = 0.9

    private var dotLayers: [CAShapeLayer] = []
    private var isAnimating = false

    init(frame frameRect: NSRect, compact: Bool = false) {
        self.dotDiameter = compact ? 5 : 9
        self.dotSpacing = compact ? 4 : 7
        self.amplitude = compact ? 3 : 6
        super.init(frame: frameRect)
        wantsLayer = true
        layer?.masksToBounds = false
        buildDots()
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) not implemented")
    }

    private func buildDots() {
        guard let host = layer else { return }
        for layer in dotLayers { layer.removeFromSuperlayer() }
        dotLayers.removeAll()

        for _ in 0..<dotCount {
            let dot = CAShapeLayer()
            let path = CGPath(ellipseIn: CGRect(x: 0, y: 0, width: dotDiameter, height: dotDiameter), transform: nil)
            dot.path = path
            dot.fillColor = NSColor.white.cgColor
            dot.bounds = CGRect(x: 0, y: 0, width: dotDiameter, height: dotDiameter)
            // Anchor at center so position drives center coordinates.
            dot.anchorPoint = CGPoint(x: 0.5, y: 0.5)
            host.addSublayer(dot)
            dotLayers.append(dot)
        }

        layoutDots()
    }

    override func layout() {
        super.layout()
        layoutDots()
    }

    private func layoutDots() {
        let totalWidth = CGFloat(dotCount) * dotDiameter + CGFloat(dotCount - 1) * dotSpacing
        let startX = (bounds.width - totalWidth) / 2 + dotDiameter / 2
        let centerY = bounds.height / 2

        for (i, dot) in dotLayers.enumerated() {
            let x = startX + CGFloat(i) * (dotDiameter + dotSpacing)
            // Disable implicit animation while we pin the resting position.
            CATransaction.begin()
            CATransaction.setDisableActions(true)
            dot.position = CGPoint(x: x, y: centerY)
            CATransaction.commit()
        }
    }

    /// Begin the wave animation. Idempotent — calling again while already
    /// running is a no-op so transitions among listening/processing/
    /// transcribing don't reset the wave.
    func start() {
        if isAnimating { return }
        isAnimating = true
        layoutDots()

        let centerY = bounds.height / 2
        let key = "dotWave"

        for (i, dot) in dotLayers.enumerated() {
            // Build a key-frame path: one full sine cycle over `period`.
            let steps = 60
            var values: [CGFloat] = []
            var keyTimes: [NSNumber] = []
            let phase = Double(i) / Double(dotCount) // 0, 1/3, 2/3
            for s in 0...steps {
                let t = Double(s) / Double(steps)
                let theta = 2.0 * .pi * (t + phase)
                let y = centerY + amplitude * CGFloat(sin(theta))
                values.append(y)
                keyTimes.append(NSNumber(value: t))
            }

            let anim = CAKeyframeAnimation(keyPath: "position.y")
            anim.values = values
            anim.keyTimes = keyTimes
            anim.duration = period
            anim.repeatCount = .infinity
            anim.calculationMode = .linear
            anim.isRemovedOnCompletion = false
            dot.add(anim, forKey: key)
        }
    }

    /// Stop the wave and clear any running animations.
    func stop() {
        if !isAnimating { return }
        isAnimating = false
        for dot in dotLayers {
            dot.removeAllAnimations()
        }
        layoutDots()
    }
}

/// Glass overlay content view shaped like the macOS volume HUD: a near-square
/// frosted block with a large SF Symbol icon at the top and a label beneath.
class StatusOverlayView: NSView {
    private let blurView: NSVisualEffectView
    private let stackView: NSStackView
    private let iconContainer: NSView
    private let iconView: NSImageView
    private let dotWaveView: DotWaveView
    private let label: NSTextField

    /// Compact mode: a small horizontal pill (icon beside label) instead
    /// of the volume-HUD block — for users who find the full HUD
    /// intrusive during constant dictation.
    private let compact: Bool

    private var iconSize: CGFloat { compact ? 18 : 42 }
    private var cornerRadius: CGFloat { compact ? 12 : 22 }
    private var iconLabelSpacing: CGFloat { compact ? 6 : 10 }
    private var verticalPadding: CGFloat { compact ? 6 : 14 }
    private var horizontalPadding: CGFloat { compact ? 12 : 20 }

    var state: OverlayState = .voiceActivity {
        didSet {
            applyState()
        }
    }

    init(frame frameRect: NSRect, compact: Bool = false) {
        self.compact = compact
        self.blurView = NSVisualEffectView(frame: frameRect)
        self.stackView = NSStackView()
        self.iconContainer = NSView()
        self.iconView = NSImageView()
        self.dotWaveView = DotWaveView(frame: .zero, compact: compact)
        self.label = NSTextField(labelWithString: "")
        super.init(frame: frameRect)

        wantsLayer = true
        layer?.cornerRadius = cornerRadius
        layer?.masksToBounds = true

        // Frosted backdrop, same material the system volume HUD uses.
        blurView.autoresizingMask = [.width, .height]
        // Solid panel — `.hudWindow` vibrancy triggers RenderBox shader failures on
        // macOS 26 that can prevent the menu-bar status item from rendering at all.
        blurView.material = .windowBackground
        blurView.isEmphasized = false
        blurView.blendingMode = .withinWindow
        blurView.state = .active
        blurView.alphaValue = 0.92
        blurView.wantsLayer = true
        blurView.layer?.cornerRadius = cornerRadius
        blurView.layer?.masksToBounds = true
        addSubview(blurView)

        iconView.imageScaling = .scaleProportionallyUpOrDown
        iconView.translatesAutoresizingMaskIntoConstraints = false
        iconContainer.addSubview(iconView)

        dotWaveView.translatesAutoresizingMaskIntoConstraints = false
        dotWaveView.isHidden = true
        iconContainer.addSubview(dotWaveView)

        label.font = .systemFont(ofSize: compact ? 12 : 15, weight: .medium)
        label.textColor = .secondaryLabelColor
        label.backgroundColor = .clear
        label.isBezeled = false
        label.isEditable = false
        label.isSelectable = false
        label.drawsBackground = false
        label.alignment = .center
        label.lineBreakMode = .byTruncatingTail
        label.translatesAutoresizingMaskIntoConstraints = false

        iconContainer.translatesAutoresizingMaskIntoConstraints = false
        // Compact: horizontal pill (icon beside label); normal: the
        // volume-HUD vertical block (icon above label).
        stackView.orientation = compact ? .horizontal : .vertical
        stackView.alignment = compact ? .centerY : .centerX
        stackView.spacing = iconLabelSpacing
        stackView.translatesAutoresizingMaskIntoConstraints = false
        stackView.addArrangedSubview(iconContainer)
        stackView.addArrangedSubview(label)
        addSubview(stackView)

        NSLayoutConstraint.activate([
            stackView.centerXAnchor.constraint(equalTo: centerXAnchor),
            stackView.centerYAnchor.constraint(equalTo: centerYAnchor),
            stackView.topAnchor.constraint(greaterThanOrEqualTo: topAnchor, constant: verticalPadding),
            stackView.bottomAnchor.constraint(lessThanOrEqualTo: bottomAnchor, constant: -verticalPadding),
            stackView.leadingAnchor.constraint(greaterThanOrEqualTo: leadingAnchor, constant: horizontalPadding),
            stackView.trailingAnchor.constraint(lessThanOrEqualTo: trailingAnchor, constant: -horizontalPadding),

            iconContainer.widthAnchor.constraint(equalToConstant: iconSize),
            iconContainer.heightAnchor.constraint(equalToConstant: iconSize),

            iconView.centerXAnchor.constraint(equalTo: iconContainer.centerXAnchor),
            iconView.centerYAnchor.constraint(equalTo: iconContainer.centerYAnchor),
            iconView.widthAnchor.constraint(equalToConstant: iconSize),
            iconView.heightAnchor.constraint(equalToConstant: iconSize),

            // dotWaveView occupies the same slot as iconView.
            dotWaveView.centerXAnchor.constraint(equalTo: iconView.centerXAnchor),
            dotWaveView.centerYAnchor.constraint(equalTo: iconView.centerYAnchor),
            dotWaveView.widthAnchor.constraint(equalTo: iconView.widthAnchor),
            dotWaveView.heightAnchor.constraint(equalTo: iconView.heightAnchor),

            label.widthAnchor.constraint(lessThanOrEqualTo: widthAnchor, constant: -2 * horizontalPadding)
        ])

        applyState()
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) not implemented")
    }

    private static let waveStates: Set<OverlayState> = [.voiceActivity, .processing, .transcribing]

    private func applyState() {
        label.stringValue = state.rawValue

        if StatusOverlayView.waveStates.contains(state) {
            // Ongoing state — show animated dot wave instead of static icon.
            iconView.isHidden = true
            iconView.image = nil
            dotWaveView.isHidden = false
            dotWaveView.start()
        } else {
            // Transient state — show static SF Symbol in white, stop wave.
            dotWaveView.stop()
            dotWaveView.isHidden = true

            let config = NSImage.SymbolConfiguration(pointSize: iconSize, weight: .regular)
            let image = NSImage(systemSymbolName: state.iconName, accessibilityDescription: state.rawValue)?
                .withSymbolConfiguration(config)
            image?.isTemplate = true
            iconView.image = image
            iconView.contentTintColor = .white
            iconView.isHidden = false
        }
    }

    /// Stop the wave animation explicitly. Called when the overlay is hidden
    /// so we don't keep firing CA animations against an offscreen layer.
    fileprivate func stopAnimations() {
        dotWaveView.stop()
    }
}

class StatusOverlayWindow: NSPanel {
    private var overlayView: StatusOverlayView?

    /// Repeating poll that lets the HUD follow the cursor across displays.
    /// nil whenever the HUD is hidden.
    private var mouseFollowTimer: Timer?
    /// The display the HUD is currently parked on — lets the follow poll skip
    /// redundant reposition work until the cursor actually changes screen.
    private var currentScreen: NSScreen?

    static let overlayWidth: CGFloat = 230
    static let overlayHeight: CGFloat = 130
    static let compactWidth: CGFloat = 190
    static let compactHeight: CGFloat = 40

    /// The display mode the current content view was built for. The mode
    /// is re-read at every `show`; a change rebuilds the view + resizes.
    private var builtCompact = false

    private var hudSize: NSSize {
        builtCompact
            ? NSSize(width: StatusOverlayWindow.compactWidth, height: StatusOverlayWindow.compactHeight)
            : NSSize(width: StatusOverlayWindow.overlayWidth, height: StatusOverlayWindow.overlayHeight)
    }

    /// (Re)build the content view when it doesn't exist yet or the user
    /// switched between Normal and Compact since it was built.
    private func ensureContentView() {
        let wantCompact = OverlayDisplayMode.current == .compact
        if overlayView == nil || builtCompact != wantCompact {
            builtCompact = wantCompact
            let frame = NSRect(origin: .zero, size: hudSize)
            let view = StatusOverlayView(frame: frame, compact: wantCompact)
            overlayView = view
            contentView = view
        }
    }

    init() {
        super.init(
            contentRect: NSRect(x: 0, y: 0, width: StatusOverlayWindow.overlayWidth, height: StatusOverlayWindow.overlayHeight),
            styleMask: [.borderless, .nonactivatingPanel],
            backing: .buffered,
            defer: false
        )

        self.backgroundColor = NSColor.clear
        self.isOpaque = false
        self.level = .floating
        // `.fullScreenAuxiliary` is required for the HUD to appear over native
        // fullscreen apps (same Space). NSPanel + nonactivatingPanel matches
        // ListeningIndicator, which renders correctly in fullscreen Spaces.
        self.collectionBehavior = [.canJoinAllSpaces, .stationary, .fullScreenAuxiliary]
        self.ignoresMouseEvents = true
        self.hasShadow = true
        self.isReleasedWhenClosed = false
        self.isFloatingPanel = true
        self.becomesKeyOnlyIfNeeded = true

        positionOnActiveScreen()
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) not implemented")
    }

    /// Reposition on the screen the cursor is on and re-front (fullscreen Spaces).
    func repositionAndFront() {
        positionOnActiveScreen()
        orderFrontRegardless()
    }

    /// Build the content view and realize the window-server surface once at
    /// launch (invisible: alpha 0 + ignoresMouseEvents) so the first real
    /// `show` only animates alpha instead of paying view creation, first
    /// layout, and order-in (~30-80ms on first show).
    func prewarmContent() {
        ensureContentView()
        alphaValue = 0.0
        orderFrontRegardless()
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.2) { [weak self] in
            guard let self = self else { return }
            // Only park it back out if no real show grabbed it meanwhile
            // (a real show immediately animates alpha above 0).
            if self.alphaValue == 0.0 {
                self.orderOut(nil)
            }
        }
    }

    func show(state: OverlayState, instant: Bool = false) {
        DispatchQueue.main.async { [weak self] in
            guard let self = self else { return }

            // Create overlay view if needed (or rebuild on mode change).
            self.ensureContentView()
            self.positionOnActiveScreen()
            self.startFollowingMouse()

            // Update state on the existing view so consecutive flashes
            // (e.g. pause then auto-enter) reuse the same window instead
            // of stacking.
            self.overlayView?.state = state

            let snapIn = instant || state.isInstantShow
            let wasVisible = self.isVisible
            if !wasVisible && !snapIn {
                self.alphaValue = 0.0
            }
            self.orderFrontRegardless()
            if snapIn {
                self.alphaValue = 1.0
            } else {
                NSAnimationContext.runAnimationGroup({ ctx in
                    ctx.duration = wasVisible ? 0.0 : 0.15
                    ctx.timingFunction = CAMediaTimingFunction(name: .easeOut)
                    self.animator().alphaValue = 1.0
                })
            }
        }
    }

    func hide() {
        // Smooth fade-out so flashes don't pop off-screen abruptly.
        fadeOut(duration: 0.4)
    }

    /// Fade the window's alpha to 0 over `duration` seconds, then hide.
    /// Calling show() during the fade restores alpha to 1 (see show()).
    func fadeOut(duration: TimeInterval = 0.4) {
        DispatchQueue.main.async { [weak self] in
            guard let self = self, self.isVisible else { return }
            NSAnimationContext.runAnimationGroup({ ctx in
                ctx.duration = duration
                ctx.timingFunction = CAMediaTimingFunction(name: .easeOut)
                self.animator().alphaValue = 0.0
            }, completionHandler: { [weak self] in
                guard let self = self else { return }
                // Only actually hide if we're still at zero alpha (i.e. no
                // show() interrupted us).
                if self.alphaValue == 0.0 {
                    self.orderOut(nil)
                    self.alphaValue = 1.0
                    // Stop following the cursor + any running CA animations
                    // once we're offscreen.
                    self.stopFollowingMouse()
                    self.overlayView?.stopAnimations()
                }
            })
        }
    }

    /// Park the HUD on the screen the mouse cursor is currently on. The cursor
    /// is the best proxy for "where the user is looking" and is always valid,
    /// even over fullscreen Spaces. `startFollowingMouse` keeps this in sync as
    /// the pointer moves between displays while the HUD is visible.
    private func positionOnActiveScreen() {
        guard let screen = OverlayScreenPlacement.screenForMouse() else { return }
        currentScreen = screen
        let size = hudSize
        let origin = OverlayScreenPlacement.volumeHudOrigin(
            on: screen,
            width: size.width,
            height: size.height
        )
        self.setFrame(
            NSRect(x: origin.x, y: origin.y, width: size.width, height: size.height),
            display: true
        )
    }

    /// Begin polling the cursor so the HUD hops to whichever display the mouse
    /// is on. Deliberately a lightweight ~6 Hz timer that only moves the window
    /// when the cursor actually crosses to another screen (compared by frame,
    /// not object identity, since `NSScreen.screens` can hand back fresh
    /// instances). Only runs while the HUD is visible; torn down on hide.
    func startFollowingMouse() {
        stopFollowingMouse()
        let timer = Timer(timeInterval: 0.15, repeats: true) { [weak self] _ in
            guard let self = self, self.isVisible else { return }
            guard let screen = OverlayScreenPlacement.screenForMouse() else { return }
            if screen.frame != self.currentScreen?.frame {
                self.positionOnActiveScreen()
            }
        }
        RunLoop.main.add(timer, forMode: .common)
        mouseFollowTimer = timer
    }

    func stopFollowingMouse() {
        mouseFollowTimer?.invalidate()
        mouseFollowTimer = nil
    }
}

/// Small persistent corner widget shown after idle timeout animation.
/// Contains moon icon and play button to manually resume.
class IdleResumeWidget: NSView {
    private let blurView: NSVisualEffectView
    private let stackView: NSStackView
    private let iconView: NSImageView
    private let playButton: NSButton

    private static let widgetWidth: CGFloat = 60
    private static let widgetHeight: CGFloat = 50
    private static let cornerRadius: CGFloat = 12
    private static let iconSize: CGFloat = 24

    var onPlayButtonClicked: (() -> Void)?

    override init(frame frameRect: NSRect) {
        self.blurView = NSVisualEffectView(frame: frameRect)
        self.stackView = NSStackView()
        self.iconView = NSImageView()
        self.playButton = NSButton()
        super.init(frame: frameRect)

        wantsLayer = true
        layer?.cornerRadius = Self.cornerRadius
        layer?.masksToBounds = true

        // Frosted backdrop
        blurView.autoresizingMask = [.width, .height]
        // Solid panel — `.hudWindow` vibrancy triggers RenderBox shader failures on
        // macOS 26 that can prevent the menu-bar status item from rendering at all.
        blurView.material = .windowBackground
        blurView.isEmphasized = false
        blurView.blendingMode = .withinWindow
        blurView.state = .active
        blurView.alphaValue = 0.92
        blurView.wantsLayer = true
        blurView.layer?.cornerRadius = Self.cornerRadius
        blurView.layer?.masksToBounds = true
        addSubview(blurView)

        // Moon icon
        let config = NSImage.SymbolConfiguration(pointSize: Self.iconSize, weight: .regular)
        let image = NSImage(systemSymbolName: "moon.zzz.fill", accessibilityDescription: "Paused")?
            .withSymbolConfiguration(config)
        image?.isTemplate = true
        iconView.image = image
        iconView.contentTintColor = .white
        iconView.translatesAutoresizingMaskIntoConstraints = false

        // Play button
        playButton.title = "▶"
        playButton.setButtonType(.momentaryPushIn)
        playButton.bezelStyle = .circular
        playButton.isBordered = true
        playButton.target = self
        playButton.action = #selector(playButtonClicked)
        playButton.translatesAutoresizingMaskIntoConstraints = false

        // Stack layout
        stackView.orientation = .horizontal
        stackView.alignment = .centerY
        stackView.spacing = 4
        stackView.translatesAutoresizingMaskIntoConstraints = false
        stackView.addArrangedSubview(iconView)
        stackView.addArrangedSubview(playButton)
        addSubview(stackView)

        NSLayoutConstraint.activate([
            stackView.centerXAnchor.constraint(equalTo: centerXAnchor),
            stackView.centerYAnchor.constraint(equalTo: centerYAnchor),
            iconView.widthAnchor.constraint(equalToConstant: Self.iconSize),
            iconView.heightAnchor.constraint(equalToConstant: Self.iconSize),
        ])
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) not implemented")
    }

    @objc private func playButtonClicked() {
        onPlayButtonClicked?()
    }
}

class IdleResumeWindow: NSPanel {
    private var widgetView: IdleResumeWidget?

    static let widgetWidth: CGFloat = 60
    static let widgetHeight: CGFloat = 50

    init() {
        super.init(
            contentRect: NSRect(x: 0, y: 0, width: Self.widgetWidth, height: Self.widgetHeight),
            styleMask: [.borderless, .nonactivatingPanel],
            backing: .buffered,
            defer: false
        )

        self.backgroundColor = NSColor.clear
        self.isOpaque = false
        self.level = .floating
        self.collectionBehavior = [.canJoinAllSpaces, .stationary, .fullScreenAuxiliary]
        self.ignoresMouseEvents = false
        self.hasShadow = true
        self.isReleasedWhenClosed = false
        self.isFloatingPanel = true
        self.becomesKeyOnlyIfNeeded = true

        positionOnFrontmostScreen()
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) not implemented")
    }

    func repositionAndFront() {
        positionOnFrontmostScreen()
        orderFrontRegardless()
    }

    func show(onPlayClicked: @escaping () -> Void) {
        DispatchQueue.main.async { [weak self] in
            guard let self = self else { return }

            if self.widgetView == nil {
                let frame = NSRect(x: 0, y: 0,
                                   width: Self.widgetWidth,
                                   height: Self.widgetHeight)
                let widget = IdleResumeWidget(frame: frame)
                widget.onPlayButtonClicked = onPlayClicked
                self.widgetView = widget
                self.contentView = widget
            }

            self.positionOnFrontmostScreen()

            if !self.isVisible {
                self.alphaValue = 0.0
                self.orderFrontRegardless()
            }

            NSAnimationContext.runAnimationGroup({ ctx in
                ctx.duration = 0.15
                ctx.timingFunction = CAMediaTimingFunction(name: .easeOut)
                self.animator().alphaValue = 1.0
            })
        }
    }

    func hide() {
        fadeOut(duration: 0.3)
    }

    func fadeOut(duration: TimeInterval = 0.3) {
        DispatchQueue.main.async { [weak self] in
            guard let self = self, self.isVisible else { return }
            NSAnimationContext.runAnimationGroup({ ctx in
                ctx.duration = duration
                ctx.timingFunction = CAMediaTimingFunction(name: .easeOut)
                self.animator().alphaValue = 0.0
            }, completionHandler: { [weak self] in
                guard let self = self else { return }
                if self.alphaValue == 0.0 {
                    self.orderOut(nil)
                    self.alphaValue = 1.0
                }
            })
        }
    }

    private func positionOnFrontmostScreen() {
        guard let screen = OverlayScreenPlacement.screenForFrontmostApp() else { return }
        let origin = OverlayScreenPlacement.bottomRightOrigin(
            on: screen,
            width: Self.widgetWidth,
            height: Self.widgetHeight
        )
        self.setFrame(
            NSRect(x: origin.x, y: origin.y, width: Self.widgetWidth, height: Self.widgetHeight),
            display: true
        )
    }
}

class StatusOverlayController {
    static let shared = StatusOverlayController()

    private var window: StatusOverlayWindow?
    private var idleResumeWindow: IdleResumeWindow?
    private var hideWorkItem: DispatchWorkItem?
    private var idleAnimationWorkItem: DispatchWorkItem?
    private var flashEndsAt: Date?
    private var pendingShowState: OverlayState?
    private var spaceObservers: [NSObjectProtocol] = []

    private init() {
        let workspaceCenter = NSWorkspace.shared.notificationCenter
        spaceObservers.append(
            workspaceCenter.addObserver(
                forName: NSWorkspace.activeSpaceDidChangeNotification,
                object: nil,
                queue: .main
            ) { [weak self] _ in
                self?.refreshVisibleOverlayPlacement()
            }
        )
        spaceObservers.append(
            NotificationCenter.default.addObserver(
                forName: NSApplication.didChangeScreenParametersNotification,
                object: nil,
                queue: .main
            ) { [weak self] _ in
                self?.refreshVisibleOverlayPlacement()
            }
        )
    }

    private func refreshVisibleOverlayPlacement() {
        if window?.isVisible == true {
            window?.repositionAndFront()
        }
        if idleResumeWindow?.isVisible == true {
            idleResumeWindow?.repositionAndFront()
        }
    }

    private func ensureWindow() {
        if window == nil {
            overlayLogger.info("creating overlay window on first use")
            window = StatusOverlayWindow()
        }
    }

    /// Create the HUD window AND its content view at launch so the first
    /// show is instant — see `StatusOverlayWindow.prewarmContent`.
    func prewarm() {
        DispatchQueue.main.async { [weak self] in
            guard let self = self else { return }
            self.ensureWindow()
            self.window?.prewarmContent()
        }
    }

    /// Show the overlay and keep it visible until explicitly hidden. Used
    /// for ongoing states like transcribing or voice activity.
    /// If a flash is currently active, defer until the flash completes
    /// so the user actually sees the toggle confirmation.
    func show(state: OverlayState) {
        // Hidden mode: the user opted out of the HUD entirely.
        guard OverlayDisplayMode.current != .hidden else {
            window?.hide()
            return
        }
        ensureWindow()
        if isFlashActive() {
            pendingShowState = state
            return
        }
        cancelPendingHide()
        window?.show(state: state, instant: state.isInstantShow)
    }

    /// Show the overlay briefly then auto-hide. Used for transient
    /// notifications like Pause/Resume or Auto-Enter on/off toggles.
    /// Always lasts the full `duration` regardless of voice activity.
    func flash(state: OverlayState, duration: TimeInterval = 1.5, forceVisible: Bool = false) {
        guard forceVisible || OverlayDisplayMode.current != .hidden else {
            window?.hide()
            return
        }
        ensureWindow()
        cancelPendingHide()
        window?.show(state: state)

        let endsAt = Date(timeIntervalSinceNow: duration)
        flashEndsAt = endsAt

        let work = DispatchWorkItem { [weak self] in
            guard let self = self else { return }
            self.flashEndsAt = nil
            // If a persistent show was deferred during the flash, honor it now
            // instead of hiding (avoids a flicker between flash hide and show).
            if let deferred = self.pendingShowState {
                self.pendingShowState = nil
                self.window?.show(state: deferred, instant: deferred.isInstantShow)
            } else {
                self.window?.hide()
            }
        }
        hideWorkItem = work
        DispatchQueue.main.asyncAfter(deadline: .now() + duration, execute: work)
    }

    /// Special handler for idle timeout: two-phase animation.
    /// Phase 1 (0-2s): Show full overlay with idle state
    /// Phase 2 (2s+): Hide main overlay, animate corner widget with play button
    func showIdleTimeoutAnimation(seconds: Int) {
        // Hidden mode: skip the HUD phase but keep the corner resume
        // widget — it's a functional control, not a status flash.
        guard OverlayDisplayMode.current != .hidden else {
            showIdleResumeWidget()
            return
        }
        ensureWindow()
        cancelPendingHide()
        cancelIdleAnimation()

        // Phase 1: Show full overlay for 2 seconds
        window?.show(state: .idleAutoPaused(seconds: seconds))

        let endsAt = Date(timeIntervalSinceNow: 2.0)
        flashEndsAt = endsAt

        // Schedule phase 2 transition at 2-second mark
        let work = DispatchWorkItem { [weak self] in
            guard let self = self else { return }
            self.flashEndsAt = nil
            self.window?.hide()

            // Phase 2: Show corner widget with play button
            self.showIdleResumeWidget()
        }
        idleAnimationWorkItem = work
        DispatchQueue.main.asyncAfter(deadline: .now() + 2.0, execute: work)
    }

    /// Show the persistent corner widget for manual resume during idle timeout.
    private func showIdleResumeWidget() {
        if idleResumeWindow == nil {
            overlayLogger.info("creating idle resume widget")
            idleResumeWindow = IdleResumeWindow()
        }

        idleResumeWindow?.show { [weak self] in
            self?.handleIdleResumeClicked()
        }
    }

    /// Called when user clicks the play button in the idle resume widget.
    private func handleIdleResumeClicked() {
        // Send toggle-pause command to resume (unpause) the daemon. The
        // bundled daemon is `always-daemon` (not `always`) — the GUI is
        // `Always`, and on case-insensitive APFS a binary named `always`
        // would collide with the GUI binary, so build.sh writes the
        // daemon to `always-daemon`. Resolve via Bundle.main so we don't
        // hardcode the install location either.
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            let daemonURL = Bundle.main.bundleURL
                .appendingPathComponent("Contents/MacOS/always-daemon")
            let task = Process()
            task.executableURL = daemonURL
            task.arguments = ["toggle-pause"]
            do {
                try task.run()
                task.waitUntilExit()

                // Hide the widget after successful unpause
                DispatchQueue.main.async {
                    self?.idleResumeWindow?.hide()
                }
            } catch {
                overlayLogger.error("toggle-pause command failed: \(error.localizedDescription, privacy: .public)")
            }
        }
    }

    /// Cancel any in-flight idle animation.
    private func cancelIdleAnimation() {
        idleAnimationWorkItem?.cancel()
        idleAnimationWorkItem = nil
    }

    func hide() {
        // If a flash is active, let it complete naturally — don't kill it
        // mid-flash because of a stale voice-activity-ended.
        if isFlashActive() {
            pendingShowState = nil
            return
        }
        cancelPendingHide()
        cancelIdleAnimation()
        window?.hide()
        idleResumeWindow?.hide()
    }

    /// Internal so `@testable import Always` can verify flash protection
    /// (a flash must outlive subsequent `show(state:)` calls during its
    /// duration). Outside of tests this is an implementation detail.
    func isFlashActive() -> Bool {
        guard let endsAt = flashEndsAt else { return false }
        return endsAt > Date()
    }

    private func cancelPendingHide() {
        hideWorkItem?.cancel()
        hideWorkItem = nil
        flashEndsAt = nil
        pendingShowState = nil
        cancelIdleAnimation()
    }
}
