import AppKit
import os.log

private let overlayLogger = Logger(subsystem: "com.always.app", category: "status-overlay")

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
        }
    }
}

/// Three-dot wave animation used for ongoing states (listening / processing /
/// transcribing). Three white circles bounce vertically in a sine-wave loop
/// with a 1/3-period phase offset between neighbors so the wave appears to
/// ripple across.
fileprivate class DotWaveView: NSView {
    private let dotCount = 3
    private let dotDiameter: CGFloat = 9
    private let dotSpacing: CGFloat = 7
    private let amplitude: CGFloat = 6
    private let period: CFTimeInterval = 0.9

    private var dotLayers: [CAShapeLayer] = []
    private var isAnimating = false

    override init(frame frameRect: NSRect) {
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

    fileprivate static let iconSize: CGFloat = 42
    fileprivate static let cornerRadius: CGFloat = 22
    fileprivate static let iconLabelSpacing: CGFloat = 10
    fileprivate static let verticalPadding: CGFloat = 14
    fileprivate static let horizontalPadding: CGFloat = 20

    var state: OverlayState = .voiceActivity {
        didSet {
            applyState()
        }
    }

    override init(frame frameRect: NSRect) {
        self.blurView = NSVisualEffectView(frame: frameRect)
        self.stackView = NSStackView()
        self.iconContainer = NSView()
        self.iconView = NSImageView()
        self.dotWaveView = DotWaveView()
        self.label = NSTextField(labelWithString: "")
        super.init(frame: frameRect)

        wantsLayer = true
        layer?.cornerRadius = StatusOverlayView.cornerRadius
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
        blurView.layer?.cornerRadius = StatusOverlayView.cornerRadius
        blurView.layer?.masksToBounds = true
        addSubview(blurView)

        iconView.imageScaling = .scaleProportionallyUpOrDown
        iconView.translatesAutoresizingMaskIntoConstraints = false
        iconContainer.addSubview(iconView)

        dotWaveView.translatesAutoresizingMaskIntoConstraints = false
        dotWaveView.isHidden = true
        iconContainer.addSubview(dotWaveView)

        label.font = .systemFont(ofSize: 15, weight: .medium)
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
        stackView.orientation = .vertical
        stackView.alignment = .centerX
        stackView.spacing = StatusOverlayView.iconLabelSpacing
        stackView.translatesAutoresizingMaskIntoConstraints = false
        stackView.addArrangedSubview(iconContainer)
        stackView.addArrangedSubview(label)
        addSubview(stackView)

        NSLayoutConstraint.activate([
            stackView.centerXAnchor.constraint(equalTo: centerXAnchor),
            stackView.centerYAnchor.constraint(equalTo: centerYAnchor),
            stackView.topAnchor.constraint(greaterThanOrEqualTo: topAnchor, constant: StatusOverlayView.verticalPadding),
            stackView.bottomAnchor.constraint(lessThanOrEqualTo: bottomAnchor, constant: -StatusOverlayView.verticalPadding),
            stackView.leadingAnchor.constraint(greaterThanOrEqualTo: leadingAnchor, constant: StatusOverlayView.horizontalPadding),
            stackView.trailingAnchor.constraint(lessThanOrEqualTo: trailingAnchor, constant: -StatusOverlayView.horizontalPadding),

            iconContainer.widthAnchor.constraint(equalToConstant: StatusOverlayView.iconSize),
            iconContainer.heightAnchor.constraint(equalToConstant: StatusOverlayView.iconSize),

            iconView.centerXAnchor.constraint(equalTo: iconContainer.centerXAnchor),
            iconView.centerYAnchor.constraint(equalTo: iconContainer.centerYAnchor),
            iconView.widthAnchor.constraint(equalToConstant: StatusOverlayView.iconSize),
            iconView.heightAnchor.constraint(equalToConstant: StatusOverlayView.iconSize),

            // dotWaveView occupies the same slot as iconView.
            dotWaveView.centerXAnchor.constraint(equalTo: iconView.centerXAnchor),
            dotWaveView.centerYAnchor.constraint(equalTo: iconView.centerYAnchor),
            dotWaveView.widthAnchor.constraint(equalTo: iconView.widthAnchor),
            dotWaveView.heightAnchor.constraint(equalTo: iconView.heightAnchor),

            label.widthAnchor.constraint(lessThanOrEqualTo: widthAnchor, constant: -2 * StatusOverlayView.horizontalPadding)
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

            let config = NSImage.SymbolConfiguration(pointSize: StatusOverlayView.iconSize, weight: .regular)
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

class StatusOverlayWindow: NSWindow {
    private var overlayView: StatusOverlayView?

    static let overlayWidth: CGFloat = 230
    static let overlayHeight: CGFloat = 130

    init() {
        super.init(
            contentRect: NSRect(x: 0, y: 0, width: StatusOverlayWindow.overlayWidth, height: StatusOverlayWindow.overlayHeight),
            styleMask: [.borderless],
            backing: .buffered,
            defer: false
        )

        self.backgroundColor = NSColor.clear
        self.isOpaque = false
        self.level = .popUpMenu
        // `.fullScreenAuxiliary` is required for the HUD to appear over native
        // fullscreen apps (same Space). ListeningIndicator already uses this.
        self.collectionBehavior = [.canJoinAllSpaces, .stationary, .fullScreenAuxiliary]
        self.ignoresMouseEvents = true
        self.hasShadow = true
        self.isReleasedWhenClosed = false

        positionOnMouseScreen()
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) not implemented")
    }

    func show(state: OverlayState) {
        DispatchQueue.main.async { [weak self] in
            guard let self = self else { return }

            // Create overlay view if needed.
            if self.overlayView == nil {
                let frame = NSRect(x: 0, y: 0,
                                   width: StatusOverlayWindow.overlayWidth,
                                   height: StatusOverlayWindow.overlayHeight)
                self.overlayView = StatusOverlayView(frame: frame)
                self.contentView = self.overlayView
            }
            self.positionOnMouseScreen()

            // Update state on the existing view so consecutive flashes
            // (e.g. pause then auto-enter) reuse the same window instead
            // of stacking.
            self.overlayView?.state = state

            // Cancel any in-flight fade-out so we don't disappear mid-show,
            // then fade in from current alpha to fully opaque.
            let wasVisible = self.isVisible
            if !wasVisible {
                self.alphaValue = 0.0
                self.orderFrontRegardless()
            }
            NSAnimationContext.runAnimationGroup({ ctx in
                ctx.duration = wasVisible ? 0.0 : 0.15
                ctx.timingFunction = CAMediaTimingFunction(name: .easeOut)
                self.animator().alphaValue = 1.0
            })
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
                    // Stop any running CA animations once we're offscreen.
                    self.overlayView?.stopAnimations()
                }
            })
        }
    }

    private func positionOnMouseScreen() {
        let mouse = NSEvent.mouseLocation
        let screen = NSScreen.screens.first(where: { NSMouseInRect(mouse, $0.frame, false) })
            ?? NSScreen.main
        guard let screen = screen else { return }

        // Place where the system volume HUD appears: horizontally centered,
        // ~140pt above the bottom of the visible frame.
        let screenFrame = screen.visibleFrame
        let windowWidth = StatusOverlayWindow.overlayWidth
        let windowHeight = StatusOverlayWindow.overlayHeight

        let targetX = (screenFrame.width - windowWidth) / 2 + screenFrame.minX
        let targetY = screenFrame.minY + 140

        self.setFrame(NSRect(x: targetX, y: targetY, width: windowWidth, height: windowHeight), display: true)
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

class IdleResumeWindow: NSWindow {
    private var widgetView: IdleResumeWidget?

    static let widgetWidth: CGFloat = 60
    static let widgetHeight: CGFloat = 50

    init() {
        super.init(
            contentRect: NSRect(x: 0, y: 0, width: Self.widgetWidth, height: Self.widgetHeight),
            styleMask: [.borderless],
            backing: .buffered,
            defer: false
        )

        self.backgroundColor = NSColor.clear
        self.isOpaque = false
        self.level = .popUpMenu
        // `.fullScreenAuxiliary` is required for the HUD to appear over native
        // fullscreen apps (same Space). ListeningIndicator already uses this.
        self.collectionBehavior = [.canJoinAllSpaces, .stationary, .fullScreenAuxiliary]
        self.ignoresMouseEvents = false
        self.hasShadow = true
        self.isReleasedWhenClosed = false

        positionInBottomRight()
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) not implemented")
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

            self.positionInBottomRight()

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

    private func positionInBottomRight() {
        let mouse = NSEvent.mouseLocation
        let screen = NSScreen.screens.first(where: { NSMouseInRect(mouse, $0.frame, false) })
            ?? NSScreen.main
        guard let screen = screen else { return }

        let screenFrame = screen.visibleFrame
        let margin: CGFloat = 20
        let targetX = screenFrame.maxX - Self.widgetWidth - margin
        let targetY = screenFrame.minY + margin

        self.setFrame(NSRect(x: targetX, y: targetY, width: Self.widgetWidth, height: Self.widgetHeight), display: true)
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

    private init() {}

    private func ensureWindow() {
        if window == nil {
            overlayLogger.info("creating overlay window on first use")
            window = StatusOverlayWindow()
        }
    }

    /// Show the overlay and keep it visible until explicitly hidden. Used
    /// for ongoing states like transcribing or voice activity.
    /// If a flash is currently active, defer until the flash completes
    /// so the user actually sees the toggle confirmation.
    func show(state: OverlayState) {
        ensureWindow()
        if isFlashActive() {
            pendingShowState = state
            return
        }
        cancelPendingHide()
        window?.show(state: state)
    }

    /// Show the overlay briefly then auto-hide. Used for transient
    /// notifications like Pause/Resume or Auto-Enter on/off toggles.
    /// Always lasts the full `duration` regardless of voice activity.
    func flash(state: OverlayState, duration: TimeInterval = 1.5) {
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
                self.window?.show(state: deferred)
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
