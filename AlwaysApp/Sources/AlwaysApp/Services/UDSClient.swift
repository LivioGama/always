import Foundation
import Darwin
import Combine
import os.log

// Event types matching Rust DaemonEvent enum
enum DaemonEventType: String, Codable {
    case listeningStarted = "ListeningStarted"
    case listeningStopped = "ListeningStopped"
    case processingStarted = "ProcessingStarted"
    case processingStopped = "ProcessingStopped"
    case transcribingStarted = "TranscribingStarted"
    case transcribingStopped = "TranscribingStopped"
    case transcriptChunk = "TranscriptChunk"
    case transcriptFinal = "TranscriptFinal"
    case paused = "Paused"
    case resumed = "Resumed"
    case autoEnterEnabled = "AutoEnterEnabled"
    case autoEnterDisabled = "AutoEnterDisabled"
    case voiceActivityDetected = "VoiceActivityDetected"
    case voiceActivityEnded = "VoiceActivityEnded"
    case transcriptionFiltered = "TranscriptionFiltered"
    case heartbeat = "Heartbeat"
}

// Event data structures
struct TranscriptChunkData: Codable {
    let text: String
}

struct TranscriptFinalData: Codable {
    let text: String
}

// Main event structure - matches Rust serde tagged enum format
// Rust uses #[serde(tag = "type", content = "data")]
// This produces JSON like: {"type":"ListeningStarted"} or {"type":"TranscriptFinal","data":{"text":"hello"}}
struct DaemonEvent: Codable {
    let type: DaemonEventType
    
    // Data is nil for events without payloads, or contains the payload struct
    let data: [String: String]?
    
    enum CodingKeys: String, CodingKey {
        case type
        case data
    }
    
    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.type = try container.decode(DaemonEventType.self, forKey: .type)
        self.data = try container.decodeIfPresent([String: String].self, forKey: .data)
    }
    
    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(type, forKey: .type)
        try container.encodeIfPresent(data, forKey: .data)
    }
}

// UDS client for connecting to daemon
class UDSClient: ObservableObject {
    @Published var isConnected: Bool = false
    @Published var connectionError: String? = nil
    /// True when watchdog has detected the daemon may be dead/stalled.
    /// UI can show a "Reconnecting…" indicator.
    @Published var isDegraded: Bool = false

    private let socketPath: String
    private var socketFD: Int32 = -1
    private var queue: DispatchQueue
    private var reconnectScheduled = false
    private var reconnectAttempts: Int = 0
    private var receiveSource: DispatchSourceRead?
    private var watchdogTimer: DispatchSourceTimer?
    private var lastEventTime: Date = Date()
    private let logger = Logger(subsystem: "com.always.app", category: "uds-client")

    /// Called when watchdog suspects daemon process is dead AND reconnect
    /// has failed repeatedly. The callback should respawn the daemon
    /// (e.g. via CLIService.startDaemon()).
    var onDaemonNeedsRespawn: (() -> Void)?

    // Watchdog tuning
    private let watchdogCheckInterval: TimeInterval = 5.0
    /// If no event (incl. heartbeat) received in this window, treat connection as stalled.
    /// Daemon emits Heartbeat every 5s, so 15s = 3 missed heartbeats.
    private let heartbeatTimeout: TimeInterval = 15.0
    /// After this many failed reconnects in a row, ask caller to respawn daemon.
    /// Lowered from 5 → 3 so stale daemons are killed faster.
    private let maxReconnectAttemptsBeforeRespawn: Int = 3
    
    /// Get the default socket path based on the platform
    static func defaultSocketPath() -> String {
        #if os(macOS)
        let home = FileManager.default.homeDirectoryForCurrentUser
        return home
            .appendingPathComponent("Library")
            .appendingPathComponent("Caches")
            .appendingPathComponent("Always")
            .appendingPathComponent("always.sock")
            .path
        #else
        // Linux: Use XDG_RUNTIME_DIR or fallback to /tmp
        if let runtimeDir = ProcessInfo.processInfo.environment["XDG_RUNTIME_DIR"] {
            return "\(runtimeDir)/always.sock"
        } else {
            return "/tmp/always.sock"
        }
        #endif
    }
    
    private func log(_ message: String) {
        logger.debug("\(message)")

        // Also write to file for debugging
        let logPath = "/tmp/udsclient.log"
        let timestamp = ISO8601DateFormatter().string(from: Date())
        let line = "[\(timestamp)] \(message)\n"

        if let data = line.data(using: .utf8) {
            if FileManager.default.fileExists(atPath: logPath) {
                if let handle = FileHandle(forWritingAtPath: logPath) {
                    handle.seekToEndOfFile()
                    handle.write(data)
                    handle.closeFile()
                }
            } else {
                try? data.write(to: URL(fileURLWithPath: logPath))
            }
        }
    }
    
    init(socketPath: String? = nil) {
        self.socketPath = socketPath ?? UDSClient.defaultSocketPath()
        self.queue = DispatchQueue(label: "com.always.udsclient")
        logger.info("Initializing with socket path: \(self.socketPath)")

        // Clean up old log file on launch
        try? FileManager.default.removeItem(atPath: "/tmp/udsclient.log")

        // Log initialization
        log("UDSClient initialized with socket: \(self.socketPath)")

        connect()
    }
    
    deinit {
        disconnect()
    }
    
    func connect() {
        guard !isConnected else { return }

        log("Attempting to connect to \(self.socketPath)")

        // Use POSIX socket APIs for Unix domain sockets
        let sock = socket(AF_UNIX, SOCK_STREAM, 0)
        guard sock >= 0 else {
            let err = errno
            let errMsg = "Failed to create socket: \(String(cString: strerror(err)))"
            logger.error("\(errMsg)")
            log(errMsg)
            scheduleReconnect()
            return
        }

        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)

        // Copy path into sun_path (max 104 bytes on macOS)
        let pathBytes = socketPath.utf8
        guard pathBytes.count < MemoryLayout.size(ofValue: addr.sun_path) else {
            logger.error("Socket path too long: \(self.socketPath)")
            close(sock)
            scheduleReconnect()
            return
        }

        _ = withUnsafeMutablePointer(to: &addr.sun_path) { pathPtr in
            memcpy(pathPtr, socketPath, pathBytes.count)
        }

        // Connect to socket
        let connectResult = withUnsafeBytes(of: &addr) { addrBytes in
            Darwin.connect(sock, addrBytes.baseAddress!.assumingMemoryBound(to: sockaddr.self), socklen_t(MemoryLayout<sockaddr_un>.size))
        }

        guard connectResult == 0 else {
            let err = errno
            let errMsg = "Failed to connect to socket: \(String(cString: strerror(err)))"
            logger.error("\(errMsg)")
            log(errMsg)
            close(sock)
            scheduleReconnect()
            return
        }

        self.socketFD = sock
        self.reconnectAttempts = 0
        self.lastEventTime = Date()
        log("Connected to daemon via Unix socket fd=\(sock)")
        DispatchQueue.main.async {
            self.isConnected = true
            self.isDegraded = false
            self.connectionError = nil
            self.logger.info("Connected to daemon")
        }

        startReceiving()
        startWatchdog()
    }
    
    func disconnect() {
        if socketFD >= 0 {
            close(socketFD)
            socketFD = -1
        }
        receiveSource?.cancel()
        receiveSource = nil
        stopWatchdog()

        DispatchQueue.main.async {
            self.isConnected = false
        }
    }

    private func scheduleReconnect() {
        DispatchQueue.main.async { [weak self] in
            guard let self = self else { return }
            if self.reconnectScheduled { return }
            self.reconnectScheduled = true
            self.reconnectAttempts += 1
            self.isDegraded = true

            // Exponential backoff: 1, 2, 4, 8, 16, max 30 seconds.
            let delay = min(30.0, pow(2.0, Double(self.reconnectAttempts - 1)))
            self.log("Scheduling reconnect attempt #\(self.reconnectAttempts) in \(delay)s")

            // After repeated failures, the daemon process is probably gone.
            // Ask the host (CLIService) to respawn it before retrying again.
            if self.reconnectAttempts >= self.maxReconnectAttemptsBeforeRespawn {
                self.log("Reconnect attempts exhausted — requesting daemon respawn")
                self.onDaemonNeedsRespawn?()
            }

            DispatchQueue.main.asyncAfter(deadline: .now() + delay) { [weak self] in
                guard let self = self else { return }
                self.reconnectScheduled = false
                self.connect()
            }
        }
    }

    /// Watchdog: if no event (heartbeat or otherwise) arrives within
    /// `heartbeatTimeout`, the daemon is presumed dead/stalled. Force a
    /// reconnect, which will trigger respawn after enough failures.
    private func startWatchdog() {
        stopWatchdog()
        let timer = DispatchSource.makeTimerSource(queue: queue)
        timer.schedule(deadline: .now() + watchdogCheckInterval,
                       repeating: watchdogCheckInterval)
        timer.setEventHandler { [weak self] in
            guard let self = self else { return }
            let elapsed = Date().timeIntervalSince(self.lastEventTime)
            if elapsed > self.heartbeatTimeout {
                self.log("Watchdog: no event for \(Int(elapsed))s — forcing reconnect")
                DispatchQueue.main.async { self.isDegraded = true }
                self.disconnect()
                self.scheduleReconnect()
            }
        }
        timer.resume()
        watchdogTimer = timer
    }

    private func stopWatchdog() {
        watchdogTimer?.cancel()
        watchdogTimer = nil
    }
    
    private func startReceiving() {
        guard socketFD >= 0 else {
            log("startReceiving() called but socketFD invalid")
            return
        }
        log("startReceiving() called with fd=\(socketFD)")
        logger.debug("startReceiving() called")

        let source = DispatchSource.makeReadSource(fileDescriptor: socketFD, queue: queue)
        source.setEventHandler { [weak self] in
            guard let self = self else { return }

            var buffer = [UInt8](repeating: 0, count: 65536)
            let bytesRead = recv(self.socketFD, &buffer, buffer.count, 0)
            self.log("recv() returned \(bytesRead) bytes")

            if bytesRead < 0 {
                let err = errno
                let errMsg = "Receive error: \(String(cString: strerror(err)))"
                self.logger.error("\(errMsg)")
                self.log(errMsg)
                self.disconnect()
                self.scheduleReconnect()
                return
            }

            if bytesRead == 0 {
                self.log("Connection closed by server")
                self.logger.info("Connection closed by server")
                self.disconnect()
                self.scheduleReconnect()
                return
            }

            let data = Data(bytes: buffer, count: bytesRead)
            if let jsonString = String(data: data, encoding: .utf8) {
                self.log("Received data: \(jsonString)")
                self.logger.debug("Received data: \(jsonString, privacy: .public)")
                self.processMessage(jsonString)
            }
        }

        source.setCancelHandler { [weak self] in
            self?.logger.debug("Receive source cancelled")
        }

        receiveSource = source
        source.resume()
    }
    
    private func processMessage(_ jsonString: String) {
        // Handle multiple JSON lines in one message
        let lines = jsonString.components(separatedBy: "\n").filter { !$0.isEmpty }
        
        for line in lines {
            if let data = line.data(using: .utf8) {
                do {
                    let event = try JSONDecoder().decode(DaemonEvent.self, from: data)
                    handleEvent(event)
                } catch {
                    self.logger.error("Failed to decode event: \(error.localizedDescription)")
                }
            }
        }
    }
    
    private func handleEvent(_ event: DaemonEvent) {
        // Any event (incl. Heartbeat) proves the daemon is alive.
        lastEventTime = Date()
        logger.debug("handleEvent - type: \(event.type.rawValue, privacy: .public)")

        // Heartbeat is purely a liveness signal — don't spam NotificationCenter.
        if event.type == .heartbeat { return }

        DispatchQueue.main.async {
            NotificationCenter.default.post(name: .daemonEvent, object: event)
        }
    }

    /// Send a JSON command line to the daemon over the UDS socket.
    /// The daemon executes it in-process and broadcasts the resulting
    /// DaemonEvent back to all connected subscribers.
    func sendCommand(_ commandType: String) {
        guard socketFD >= 0, isConnected else {
            self.logger.warning("sendCommand(\(commandType)) skipped — not connected")
            return
        }

        let json = "{\"type\":\"\(commandType)\"}\n"
        guard let data = json.data(using: .utf8) else { return }

        let bytesWritten = data.withUnsafeBytes { buffer in
            send(socketFD, buffer.baseAddress!, buffer.count, 0)
        }

        if bytesWritten < 0 {
            let err = errno
            logger.error("sendCommand(\(commandType)) failed: \(String(cString: strerror(err)))")
        } else {
            logger.debug("Sent command: \(commandType)")
        }
    }
}

// Notification name for daemon events
extension Notification.Name {
    static let daemonEvent = Notification.Name("daemonEvent")
}
