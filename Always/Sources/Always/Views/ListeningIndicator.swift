import SwiftUI
import AppKit

/// Pure AppKit view that draws the indicator (loading or listening)
class ListeningIndicatorAppKitView: NSView {
    private var hostingView: NSHostingView<AnyView>?

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        self.wantsLayer = true
        updateIndicator(isLoading: false)
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) not implemented")
    }

    func updateIndicator(isLoading: Bool) {
        // Remove existing hosting view
        hostingView?.removeFromSuperview()

        // Create new hosting view with the appropriate indicator
        let indicatorView: AnyView = isLoading
            ? AnyView(LoadingIndicatorView())
            : AnyView(ListeningIndicatorView())

        hostingView = NSHostingView(rootView: indicatorView)
        hostingView!.frame = self.bounds
        hostingView!.autoresizingMask = [.width, .height]

        addSubview(hostingView!)
    }
}

extension NSBezierPath {
    var cgPath: CGPath {
        let path = CGMutablePath()
        var points = [CGPoint](repeating: .zero, count: 3)
        
        for i in 0..<elementCount {
            let type = element(at: i, associatedPoints: &points)
            switch type {
            case .moveTo:
                path.move(to: points[0])
            case .lineTo:
                path.addLine(to: points[0])
            case .curveTo:
                path.addCurve(to: points[2], control1: points[0], control2: points[1])
            case .closePath:
                path.closeSubpath()
            case .quadraticCurveTo:
                path.addQuadCurve(to: points[1], control: points[0])
            case .cubicCurveTo:
                path.addCurve(to: points[2], control1: points[0], control2: points[1])
            @unknown default:
                break
            }
        }
        return path
    }
}

/// Floating visual indicator showing voice detection or loading - appears inside the active text field
/// Using NSPanel instead of NSWindow because LSUIElement apps have window rendering restrictions
class ListeningIndicatorWindow: NSPanel {
    private var updateTimer: Timer?
    private var indicatorView: ListeningIndicatorAppKitView?

    init() {
        super.init(
            contentRect: NSRect(x: 0, y: 0, width: 40, height: 40),
            styleMask: [.borderless, .nonactivatingPanel],
            backing: .buffered,
            defer: false
        )

        print("ListeningIndicatorWindow: Initializing as NSPanel")

        self.backgroundColor = NSColor.clear
        self.isOpaque = false
        self.level = .statusBar  // NSPanel-specific level
        self.collectionBehavior = [.canJoinAllSpaces, .stationary, .fullScreenAuxiliary]
        self.ignoresMouseEvents = true
        self.hasShadow = true  // Enable shadow to make it more visible
        self.isFloatingPanel = true  // NSPanel-specific property
        self.becomesKeyOnlyIfNeeded = true

        // Create indicator view
        indicatorView = ListeningIndicatorAppKitView(frame: NSRect(x: 0, y: 0, width: 40, height: 40))
        self.contentView = indicatorView

        print("ListeningIndicatorWindow: Initial frame: \(self.frame)")
        print("ListeningIndicatorWindow: Window level: \(self.level.rawValue)")

        // Start position tracking
        startTrackingTextField()
    }

    func updateIndicatorState(isLoading: Bool) {
        indicatorView?.updateIndicator(isLoading: isLoading)
    }
    
    private func startTrackingTextField() {
        // Update position every 100ms to follow active text field
        updateTimer = Timer.scheduledTimer(withTimeInterval: 0.1, repeats: true) { [weak self] _ in
            self?.updatePositionInTextField()
        }
    }
    
    deinit {
        updateTimer?.invalidate()
    }
    
    private func updatePositionInTextField() {
        // Check for Accessibility permission first
        let options: NSDictionary = [kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String: false]
        let accessEnabled = AXIsProcessTrustedWithOptions(options)
        
        if !accessEnabled {
            print("ListeningIndicator: No Accessibility permission - falling back to screen corner")
            fallbackToScreenCorner()
            return
        }
        
        // Get focused text field from frontmost app
        guard let frontApp = NSWorkspace.shared.frontmostApplication else {
            print("ListeningIndicator: No frontmost application")
            fallbackToScreenCorner()
            return
        }
        
        let axApp = AXUIElementCreateApplication(frontApp.processIdentifier)
        var focusedUIElement: CFTypeRef?
        
        let result = AXUIElementCopyAttributeValue(
            axApp,
            kAXFocusedUIElementAttribute as CFString,
            &focusedUIElement
        )
        
        guard result == .success,
              let element = focusedUIElement else {
            print("ListeningIndicator: No focused UI element - falling back to screen corner")
            fallbackToScreenCorner()
            return
        }
        
        // Get the bounds of the focused text element
        var positionValue: CFTypeRef?
        var sizeValue: CFTypeRef?
        
        AXUIElementCopyAttributeValue(element as! AXUIElement, kAXPositionAttribute as CFString, &positionValue)
        AXUIElementCopyAttributeValue(element as! AXUIElement, kAXSizeAttribute as CFString, &sizeValue)
        
        guard let positionValue = positionValue,
              let sizeValue = sizeValue else {
            print("ListeningIndicator: Could not get position/size - falling back to screen corner")
            fallbackToScreenCorner()
            return
        }
        
        var position = CGPoint.zero
        var size = CGSize.zero
        
        AXValueGetValue(positionValue as! AXValue, .cgPoint, &position)
        AXValueGetValue(sizeValue as! AXValue, .cgSize, &size)
        
        print("ListeningIndicator: TextField position: (\(position.x), \(position.y)), size: (\(size.width) x \(size.height))")
        
        // Find which screen contains the text field
        let textFieldRect = CGRect(origin: position, size: size)
        var targetScreen: NSScreen?
        var maxIntersectionArea: CGFloat = 0
        
        for screen in NSScreen.screens {
            let intersection = textFieldRect.intersection(screen.frame)
            let area = intersection.width * intersection.height
            if area > maxIntersectionArea {
                maxIntersectionArea = area
                targetScreen = screen
            }
        }
        
        // Fallback to main screen if no intersection found
        guard let screen = targetScreen ?? NSScreen.main else {
            fallbackToScreenCorner()
            return
        }
        
        let screenFrame = screen.visibleFrame
        print("ListeningIndicator: Using screen with frame: \(screenFrame)")
        
        // Position indicator at bottom-right of screen (fixed position, easier to see)
        // macOS coordinates: origin at bottom-left, Y increases upward
        let targetX = screenFrame.maxX - 60  // 40px indicator + 20px padding from right
        let targetY = screenFrame.minY + 60  // 60px from bottom
        
        print("ListeningIndicator: Positioning at (\(targetX), \(targetY)) on screen")
        
        self.setFrameOrigin(NSPoint(x: targetX, y: targetY))
        self.orderFrontRegardless()
    }
    
    private func fallbackToScreenCorner() {
        if let screen = NSScreen.main {
            let screenFrame = screen.visibleFrame
            let x = screenFrame.maxX - 60
            let y = screenFrame.minY + 60
            self.setFrameOrigin(NSPoint(x: x, y: y))
            self.orderFrontRegardless()
        }
    }
}

struct LoadingIndicatorView: View {
    @State private var rotation: Double = 0

    var body: some View {
        ZStack {
            // Semi-transparent background
            Circle()
                .fill(Color.black.opacity(0.55))
                .frame(width: 36, height: 36)

            // Spinning loader
            Circle()
                .trim(from: 0, to: 0.7)
                .stroke(
                    LinearGradient(
                        colors: [.blue, .purple],
                        startPoint: .topLeading,
                        endPoint: .bottomTrailing
                    ),
                    style: StrokeStyle(lineWidth: 3, lineCap: .round)
                )
                .frame(width: 24, height: 24)
                .rotationEffect(.degrees(rotation))

            // Center dot
            Circle()
                .fill(.blue)
                .frame(width: 4, height: 4)
        }
        .shadow(color: .black.opacity(0.3), radius: 4, x: 0, y: 2)
        .onAppear {
            withAnimation(
                Animation.linear(duration: 1.0)
                    .repeatForever(autoreverses: false)
            ) {
                rotation = 360
            }
        }
    }
}

struct ListeningIndicatorView: View {
    @State private var pulseScale: CGFloat = 1.0
    @State private var rotation: Double = 0
    
    var body: some View {
        ZStack {
            // Outer pulse ring
            Circle()
                .stroke(
                    LinearGradient(
                        colors: [.blue.opacity(0.5), .purple.opacity(0.5)],
                        startPoint: .topLeading,
                        endPoint: .bottomTrailing
                    ),
                    lineWidth: 2
                )
                .frame(width: 32, height: 32)
                .scaleEffect(pulseScale)
                .opacity(2.0 - pulseScale)
            
            // Middle glow
            Circle()
                .fill(
                    RadialGradient(
                        colors: [.blue.opacity(0.6), .purple.opacity(0.4)],
                        center: .center,
                        startRadius: 3,
                        endRadius: 12
                    )
                )
                .frame(width: 24, height: 24)
                .blur(radius: 1)
            
            // Inner core with slight rotation
            Circle()
                .fill(
                    LinearGradient(
                        colors: [.blue, .purple],
                        startPoint: .topLeading,
                        endPoint: .bottomTrailing
                    )
                )
                .frame(width: 20, height: 20)
                .rotationEffect(.degrees(rotation))
            
            // Microphone waveform icon
            Image(systemName: "waveform")
                .font(.system(size: 10, weight: .semibold))
                .foregroundColor(.white)
        }
        .shadow(color: .black.opacity(0.3), radius: 4, x: 0, y: 2)
        .onAppear {
            // Continuous pulse animation
            withAnimation(
                Animation.easeInOut(duration: 1.0)
                    .repeatForever(autoreverses: true)
            ) {
                pulseScale = 1.4
            }
            
            // Subtle rotation for visual interest
            withAnimation(
                Animation.linear(duration: 3.0)
                    .repeatForever(autoreverses: false)
            ) {
                rotation = 360
            }
        }
    }
}

/// Global controller for the listening indicator
class ListeningIndicatorController {
    static let shared = ListeningIndicatorController()

    private var window: ListeningIndicatorWindow?
    private var isLoading: Bool = false

    private init() {}

    func show() {
        DispatchQueue.main.async { [weak self] in
            if self?.window == nil {
                self?.window = ListeningIndicatorWindow()
            }
            self?.window?.updateIndicatorState(isLoading: self?.isLoading ?? false)
            self?.window?.orderFrontRegardless()
        }
    }

    func hide() {
        DispatchQueue.main.async { [weak self] in
            self?.window?.orderOut(nil)
            self?.window = nil
        }
    }

    func setLoading(_ loading: Bool) {
        isLoading = loading
        DispatchQueue.main.async { [weak self] in
            self?.window?.updateIndicatorState(isLoading: loading)
        }
    }
}
