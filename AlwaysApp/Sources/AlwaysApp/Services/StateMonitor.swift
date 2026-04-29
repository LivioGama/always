import Foundation
import Combine
import Darwin

struct DaemonState: Codable {
    var listening: Bool
    var processing: Bool
    var transcribing: Bool?  // Make optional for backward compatibility
    var paused: Bool
    var autoEnter: Bool
    var lastTranscript: String?
    var lastUpdated: UInt64
    var version: UInt64?  // Make optional for backward compatibility

    enum CodingKeys: String, CodingKey {
        case listening
        case processing
        case transcribing
        case paused
        case autoEnter = "auto_enter"
        case lastTranscript = "last_transcript"
        case lastUpdated = "last_updated"
        case version
    }
}

class StateMonitor: ObservableObject {
    @Published var isListening: Bool = false
    @Published var isProcessing: Bool = false
    @Published var isTranscribing: Bool = false
    @Published var isPaused: Bool = false
    @Published var isAutoEnter: Bool = false
    @Published var lastTranscript: String? = nil
    @Published var lastUpdated: UInt64 = 0
    @Published var showNotification: Bool = false

    private var timer: Timer?
    private var dispatchSource: DispatchSourceFileSystemObject?
    private let stateFilePath: String
    private let notificationFilePath: String

    init() {
        // Path to state file: ~/.config/always/state.json
        let home = FileManager.default.homeDirectoryForCurrentUser
        stateFilePath = home.appendingPathComponent(".config/always/state.json").path
        notificationFilePath = home.appendingPathComponent(".config/always/notification.txt").path

        startMonitoring()
    }
    
    deinit {
        stopMonitoring()
    }

    func startMonitoring() {
        // Check if state file exists at startup
        let exists = FileManager.default.fileExists(atPath: stateFilePath)
        print("StateMonitor: Starting monitoring at \(stateFilePath)")
        print("StateMonitor: State file exists: \(exists)")

        if exists {
            // Read initial state
            checkState()
            print("StateMonitor: Initial state - listening: \(isListening), processing: \(isProcessing)")
        }

        // Use polling with 33ms interval (30fps) for responsive state detection
        timer = Timer.scheduledTimer(withTimeInterval: 0.033, repeats: true) { [weak self] _ in
            self?.checkState()
        }
        // Add timer to common run loop mode so it works when app is in background
        RunLoop.main.add(timer!, forMode: .common)
        print("StateMonitor: Timer scheduled (33ms interval)")
    }
    
    func stopMonitoring() {
        timer?.invalidate()
        timer = nil
    }
    
    private func checkState() {
        guard FileManager.default.fileExists(atPath: stateFilePath) else {
            return
        }

        do {
            let data = try Data(contentsOf: URL(fileURLWithPath: stateFilePath))
            let state = try JSONDecoder().decode(DaemonState.self, from: data)

            DispatchQueue.main.async {
                // Update published properties (triggers Combine)
                self.isListening = state.listening
                self.isProcessing = state.processing
                self.isTranscribing = state.transcribing ?? false
                self.isPaused = state.paused
                self.isAutoEnter = state.autoEnter
                self.lastTranscript = state.lastTranscript
                self.lastUpdated = state.lastUpdated
            }
        } catch {
            print("StateMonitor: ERROR decoding state - \(error)")
        }

        // Check for notification trigger file
        if FileManager.default.fileExists(atPath: notificationFilePath) {
            DispatchQueue.main.async {
                self.showNotification = true
                // Delete the trigger file after reading
                try? FileManager.default.removeItem(atPath: self.notificationFilePath)
            }
        }
    }

    // Public method for forcing immediate state update (for keyboard shortcuts)
    func forceStateUpdate() {
        print("StateMonitor: Force state update requested")
        checkState()
    }
}
