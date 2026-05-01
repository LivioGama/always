import AppKit

enum OverlayState: String, CaseIterable {
    case paused = "Paused"
    case resumed = "Resumed"
    case autoEnterOn = "Auto-Enter On"
    case autoEnterOff = "Auto-Enter Off"
    case transcribing = "Transcribing"
    case processing = "Processing"
    case voiceActivity = "Listening"

    var iconName: String {
        switch self {
        case .paused: return "pause.fill"
        case .resumed: return "play.fill"
        case .autoEnterOn: return "checkmark.circle.fill"
        case .autoEnterOff: return "circle"
        case .transcribing: return "waveform.circle.fill"
        case .processing: return "waveform.circle"
        case .voiceActivity: return "waveform"
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
    private let iconView: NSImageView
    private let dotWaveView: DotWaveView
    private let label: NSTextField

    fileprivate static let iconSize: CGFloat = 56
    fileprivate static let cornerRadius: CGFloat = 24
    fileprivate static let topPadding: CGFloat = 28
    fileprivate static let iconLabelSpacing: CGFloat = 16
    fileprivate static let bottomPadding: CGFloat = 22
    fileprivate static let horizontalPadding: CGFloat = 18

    var state: OverlayState = .voiceActivity {
        didSet {
            applyState()
        }
    }

    override init(frame frameRect: NSRect) {
        self.blurView = NSVisualEffectView(frame: frameRect)
        self.iconView = NSImageView()
        self.dotWaveView = DotWaveView()
        self.label = NSTextField(labelWithString: "")
        super.init(frame: frameRect)

        wantsLayer = true
        layer?.cornerRadius = StatusOverlayView.cornerRadius
        layer?.masksToBounds = true

        // Frosted backdrop, same material the system volume HUD uses.
        blurView.autoresizingMask = [.width, .height]
        blurView.material = .hudWindow
        blurView.blendingMode = .behindWindow
        blurView.state = .active
        blurView.wantsLayer = true
        blurView.layer?.cornerRadius = StatusOverlayView.cornerRadius
        blurView.layer?.masksToBounds = true
        addSubview(blurView)

        iconView.imageScaling = .scaleProportionallyUpOrDown
        iconView.translatesAutoresizingMaskIntoConstraints = false
        addSubview(iconView)

        dotWaveView.translatesAutoresizingMaskIntoConstraints = false
        dotWaveView.isHidden = true
        addSubview(dotWaveView)

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
        addSubview(label)

        NSLayoutConstraint.activate([
            iconView.centerXAnchor.constraint(equalTo: centerXAnchor),
            iconView.topAnchor.constraint(equalTo: topAnchor, constant: StatusOverlayView.topPadding),
            iconView.widthAnchor.constraint(equalToConstant: StatusOverlayView.iconSize),
            iconView.heightAnchor.constraint(equalToConstant: StatusOverlayView.iconSize),

            // dotWaveView occupies the same 56x56 slot as iconView.
            dotWaveView.centerXAnchor.constraint(equalTo: iconView.centerXAnchor),
            dotWaveView.centerYAnchor.constraint(equalTo: iconView.centerYAnchor),
            dotWaveView.widthAnchor.constraint(equalTo: iconView.widthAnchor),
            dotWaveView.heightAnchor.constraint(equalTo: iconView.heightAnchor),

            label.topAnchor.constraint(equalTo: iconView.bottomAnchor, constant: StatusOverlayView.iconLabelSpacing),
            label.leadingAnchor.constraint(equalTo: leadingAnchor, constant: StatusOverlayView.horizontalPadding),
            label.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -StatusOverlayView.horizontalPadding),
            label.bottomAnchor.constraint(lessThanOrEqualTo: bottomAnchor, constant: -StatusOverlayView.bottomPadding)
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

    static let overlayWidth: CGFloat = 200
    static let overlayHeight: CGFloat = 200

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
        self.collectionBehavior = [.canJoinAllSpaces, .stationary]
        self.ignoresMouseEvents = true
        self.hasShadow = true
        self.isReleasedWhenClosed = false

        positionAtBottomMiddle()
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
                self.positionAtBottomMiddle()
            }

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

    private func positionAtBottomMiddle() {
        guard let screen = NSScreen.main else { return }

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

class StatusOverlayController {
    static let shared = StatusOverlayController()

    private var window: StatusOverlayWindow?
    private var hideWorkItem: DispatchWorkItem?

    private init() {
        // Don't create window here - wait until first use
    }

    private func ensureWindow() {
        if window == nil {
            NSLog("StatusOverlayController: Creating window on first use")
            window = StatusOverlayWindow()
        }
    }

    /// Show the overlay and keep it visible until explicitly hidden. Used
    /// for ongoing states like transcribing or voice activity.
    func show(state: OverlayState) {
        ensureWindow()
        cancelPendingHide()
        window?.show(state: state)
    }

    /// Show the overlay briefly then auto-hide. Used for transient
    /// notifications like Pause/Resume or Auto-Enter on/off toggles.
    func flash(state: OverlayState, duration: TimeInterval = 3.5) {
        ensureWindow()
        cancelPendingHide()
        window?.show(state: state)

        let work = DispatchWorkItem { [weak self] in
            self?.window?.hide()
        }
        hideWorkItem = work
        DispatchQueue.main.asyncAfter(deadline: .now() + duration, execute: work)
    }

    func hide() {
        cancelPendingHide()
        window?.hide()
    }

    private func cancelPendingHide() {
        hideWorkItem?.cancel()
        hideWorkItem = nil
    }
}
