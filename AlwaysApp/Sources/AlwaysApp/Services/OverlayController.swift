import SwiftUI
import AppKit
import Combine

enum OverlayState {
    case hidden
    case idle
    case listening
    case processing
    case transcribing  // New state specifically for transcription
    case paused
    case autoEnter
    case notification  // New state for showing notifications with animation
}

class OverlayController: ObservableObject {
    private var overlayWindow: NSWindow?
    private var hostingView: NSHostingView<OverlayView>?
    @Published private(set) var state: OverlayState = .listening
    private var cancellables = Set<AnyCancellable>()
    private var overlaySize = CGSize(width: 40, height: 40)
    private var stateBinding: Binding<OverlayState> {
        Binding(
            get: { self.state },
            set: { self.state = $0 }
        )
    }

    init() {
        setupWindow()
        // observeScreenChanges() // Disabled to prevent position override
    }

    deinit {
        overlayWindow?.close()
    }

    private func setupWindow() {
        print("OverlayController: setupWindow called")

        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: overlaySize.width, height: overlaySize.height),
            styleMask: [.borderless],
            backing: .buffered,
            defer: false
        )

        print("OverlayController: Window created")

        window.backgroundColor = .clear
        window.isOpaque = false
        window.level = .floating
        window.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .stationary]
        window.ignoresMouseEvents = true
        window.hasShadow = true
        window.isReleasedWhenClosed = false

        // Position at visible location
        window.setFrameOrigin(NSPoint(x: 100, y: 100))

        print("OverlayController: Window positioned at (100, 100)")

        window.orderFront(nil)

        print("OverlayController: Window ordered front")

        let overlayView = OverlayView(state: stateBinding)
        hostingView = NSHostingView(rootView: overlayView)
        hostingView?.frame = NSRect(x: 0, y: 0, width: overlaySize.width, height: overlaySize.height)
        window.contentView = hostingView

        print("OverlayController: Content view set")

        overlayWindow = window
        print("OverlayController: Window configured, window exists: \(overlayWindow != nil)")
    }

    private func centerOverlay() {
        guard let window = overlayWindow else { return }

        if let screen = NSScreen.main {
            let screenFrame = screen.visibleFrame
            let windowFrame = window.frame

            // Calculate center position
            let x = screenFrame.midX - (windowFrame.width / 2)
            let y = screenFrame.midY - (windowFrame.height / 2)

            window.setFrameOrigin(NSPoint(x: x, y: y))
            print("OverlayController: Centered overlay at (\(x), \(y))")
        }
    }

    private func observeScreenChanges() {
        NotificationCenter.default.publisher(for: NSApplication.didChangeScreenParametersNotification)
            .sink { [weak self] _ in
                self?.updatePosition()
            }
            .store(in: &cancellables)

        NotificationCenter.default.publisher(for: NSWorkspace.screensDidWakeNotification)
            .sink { [weak self] _ in
                self?.updatePosition()
            }
            .store(in: &cancellables)
    }

    func setState(_ newState: OverlayState) {
        print("OverlayController: setState called with \(newState)")
        state = newState

        DispatchQueue.main.async { [weak self] in
            switch newState {
            case .hidden:
                self?.hideOverlay()
            case .idle, .listening, .processing, .transcribing, .paused, .autoEnter, .notification:
                self?.showOverlay()
            }
        }
    }

    func showNotification() {
        print("OverlayController: showNotification called")

        // Resize window for notification pill
        guard let window = overlayWindow else { return }
        window.setFrame(NSRect(x: 0, y: 0, width: 200, height: 60), display: true)

        // Center the overlay
        centerOverlay()

        setState(.notification)
    }

    private func showOverlay() {
        guard let window = overlayWindow else { return }
        // Don't call updatePosition() to prevent position override
        window.orderFront(nil)
        window.makeKey()
        print("OverlayController: Overlay shown, state: \(state), window level: \(window.level), visible: \(window.isVisible)")
    }

    private func hideOverlay() {
        overlayWindow?.orderOut(nil)
        print("OverlayController: Overlay hidden")
    }

    private func updatePosition() {
        guard let window = overlayWindow, state != .hidden else { return }

        let screen = getActiveScreen()
        let position = calculatePosition(for: screen)
        window.setFrameOrigin(position)
        print("OverlayController: Position updated to \(position) on screen: \(screen?.frame ?? .zero)")
    }

    private func getActiveScreen() -> NSScreen? {
        // First try to get screen from mouse location
        let mouseLocation = NSEvent.mouseLocation
        for screen in NSScreen.screens {
            if screen.frame.contains(mouseLocation) {
                return screen
            }
        }

        // Fallback to main screen
        return NSScreen.main
    }

    private func calculatePosition(for screen: NSScreen?) -> NSPoint {
        // Use fixed position for now - screen-aware positioning is calculating off-screen coordinates
        let x: CGFloat = 500
        let y: CGFloat = 500
        return NSPoint(x: x, y: y)
    }
}

struct OverlayView: View {
    @Binding var state: OverlayState

    var body: some View {
        ZStack {
            if state == .notification {
                // Liquid glass notification pill
                RoundedRectangle(cornerRadius: 20)
                    .fill(
                        LinearGradient(
                            gradient: Gradient(colors: [Color.blue.opacity(0.3), Color.blue.opacity(0.1)]),
                            startPoint: .topLeading,
                            endPoint: .bottomTrailing
                        )
                    )
                    .frame(width: 200, height: 60)
                    .overlay(
                        RoundedRectangle(cornerRadius: 20)
                            .stroke(Color.white.opacity(0.2), lineWidth: 1)
                    )
                    .shadow(color: Color.black.opacity(0.2), radius: 10, x: 0, y: 5)
                    .overlay(
                        Text("Notification")
                            .font(.system(size: 14, weight: .medium))
                            .foregroundColor(.white)
                    )
                    .scaleEffect(scaleForState)
                    .opacity(opacityForState)
                    .animation(animationForState, value: state)
            } else {
                // Regular circle indicator
                Circle()
                    .fill(colorForState)
                    .frame(width: 40, height: 40)
                    .shadow(radius: 5)
                    .scaleEffect(scaleForState)
                    .opacity(opacityForState)
                    .animation(animationForState, value: state)
            }
        }
    }

    private var colorForState: Color {
        switch state {
        case .hidden:
            return .clear
        case .idle:
            return .gray
        case .listening:
            return .red
        case .processing:
            return .blue
        case .transcribing:
            return .purple  // Distinct color for transcription
        case .paused:
            return .orange
        case .autoEnter:
            return .green
        case .notification:
            return .blue  // Liquid glass blue for notifications
        }
    }

    private var scaleForState: CGFloat {
        switch state {
        case .hidden:
            return 0
        case .idle:
            return 1.0
        case .listening:
            return 1.2  // Slightly larger when listening
        case .processing:
            return 1.0
        case .transcribing:
            return 1.1  // Medium size for transcription
        case .paused:
            return 1.0
        case .autoEnter:
            return 1.0
        case .notification:
            return 2.0  // Larger for notification
        }
    }

    private var opacityForState: Double {
        switch state {
        case .hidden:
            return 0
        case .idle:
            return 0.5
        case .listening:
            return 1.0
        case .processing:
            return 0.8
        case .transcribing:
            return 0.9
        case .paused:
            return 0.7
        case .autoEnter:
            return 0.8
        case .notification:
            return 0.9
        }
    }

    private var animationForState: Animation? {
        switch state {
        case .hidden:
            return .easeOut(duration: 0.2)
        case .idle:
            return .default
        case .listening:
            return .easeInOut(duration: 0.3).repeatForever(autoreverses: true)
        case .processing:
            return .linear(duration: 1.0).repeatForever(autoreverses: false)
        case .transcribing:
            return .easeInOut(duration: 0.5).repeatForever(autoreverses: true)
        case .paused:
            return .default
        case .autoEnter:
            return .default
        case .notification:
            return .spring(response: 0.5, dampingFraction: 0.7).repeatForever(autoreverses: true)
        }
    }
}
