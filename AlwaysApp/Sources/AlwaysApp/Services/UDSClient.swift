import Foundation
import Network
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
    
    private let socketPath: String
    private var connection: NWConnection?
    private var queue: DispatchQueue
    private var reconnectScheduled = false
    private let logger = Logger(subsystem: "com.always.app", category: "uds-client")
    
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
        
        // Clean up old log file on launch
        try? FileManager.default.removeItem(atPath: "/tmp/udsclient.log")
    }
    
    init(socketPath: String? = nil) {
        self.socketPath = socketPath ?? UDSClient.defaultSocketPath()
        self.queue = DispatchQueue(label: "com.always.udsclient")
        logger.info("Initializing with socket path: \(self.socketPath)")
        
        // Clean up old log file on launch
        try? FileManager.default.removeItem(atPath: "/tmp/udsclient.log")
        
        connect()
    }
    
    deinit {
        disconnect()
    }
    
    func connect() {
        guard !isConnected else { return }
        
        logger.debug("Attempting to connect to \(self.socketPath)")
        let endpoint = NWEndpoint.unix(path: socketPath)
        
        // CRITICAL: Use .tcp for Unix sockets, not default parameters
        let parameters = NWParameters.tcp
        parameters.allowLocalEndpointReuse = true
        connection = NWConnection(to: endpoint, using: parameters)
        
        connection?.stateUpdateHandler = { [weak self] state in
            guard let self = self else { return }
            let stateDescription: String
            switch state {
            case .ready:
                stateDescription = "ready"
            case .failed(let error):
                stateDescription = "failed(\(error.localizedDescription))"
            case .waiting(let error):
                stateDescription = "waiting(\(error.localizedDescription))"
            case .setup:
                stateDescription = "setup"
            case .cancelled:
                stateDescription = "cancelled"
            case .preparing:
                stateDescription = "preparing"
            @unknown default:
                stateDescription = "unknown"
            }
            self.logger.debug("State change: \(stateDescription)")
            switch state {
            case .ready:
                DispatchQueue.main.async {
                    self.isConnected = true
                    self.connectionError = nil
                    self.logger.info("Connected to daemon")
                }
                self.startReceiving()
            case .failed(let error):
                DispatchQueue.main.async {
                    self.isConnected = false
                    self.connectionError = error.localizedDescription
                    self.logger.error("Connection failed: \(error.localizedDescription)")
                }
                self.connection?.cancel()
                self.connection = nil
                DispatchQueue.main.async {
                    self.isConnected = false
                }
                self.scheduleReconnect()
            case .preparing:
                self.logger.debug("Preparing connection")
            case .cancelled:
                DispatchQueue.main.async {
                    self.isConnected = false
                    self.logger.warning("Connection cancelled")
                }
            @unknown default:
                self.logger.warning("Unknown state")
            }
        }
        
        connection?.start(queue: queue)
        self.logger.debug("Connection started on queue")
    }
    
    func disconnect() {
        connection?.cancel()
        connection = nil

        DispatchQueue.main.async {
            self.isConnected = false
        }
    }

    private func scheduleReconnect() {
        // NWConnection state updates fire on `self.queue` (background), where
        // Timer.scheduledTimer wouldn't fire because there's no active
        // runloop. Use the main queue's asyncAfter instead.
        DispatchQueue.main.async { [weak self] in
            guard let self = self else { return }
            if self.reconnectScheduled { return }
            self.reconnectScheduled = true
            DispatchQueue.main.asyncAfter(deadline: .now() + 1.0) { [weak self] in
                guard let self = self else { return }
                self.reconnectScheduled = false
                self.connect()
            }
        }
    }
    
    private func startReceiving() {
        logger.debug("startReceiving() called")
        connection?.receive(minimumIncompleteLength: 1, maximumLength: 65536) { [weak self] data, _, isComplete, error in
            guard let self = self else { return }
            
            if let data = data, !data.isEmpty {
                if let jsonString = String(data: data, encoding: .utf8) {
                    self.logger.debug("Received data: \(jsonString, privacy: .public)")
                    self.processMessage(jsonString)
                }
            }
            
            if let error = error {
                self.logger.error("Receive error: \(error.localizedDescription, privacy: .public)")
                self.scheduleReconnect()
                return
            }
            
            if isComplete {
                self.logger.info("Connection closed by server")
                self.scheduleReconnect()
                return
            }
            
            // Continue receiving
            self.startReceiving()
        }
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
        logger.debug("handleEvent - type: \(event.type.rawValue, privacy: .public)")
        DispatchQueue.main.async {
            NotificationCenter.default.post(name: .daemonEvent, object: event)
        }
    }

    /// Send a JSON command line to the daemon over the UDS socket.
    /// The daemon executes it in-process and broadcasts the resulting
    /// DaemonEvent back to all connected subscribers.
    func sendCommand(_ commandType: String) {
        guard let connection = connection, isConnected else {
            self.logger.warning("sendCommand(\(commandType)) skipped — not connected")
            return
        }

        let json = "{\"type\":\"\(commandType)\"}\n"
        guard let data = json.data(using: .utf8) else { return }

        connection.send(content: data, completion: .contentProcessed { [weak self] error in
            if let error = error {
                self?.logger.error("sendCommand(\(commandType)) failed: \(error.localizedDescription)")
            } else {
                self?.logger.debug("Sent command: \(commandType)")
            }
        })
    }
}

// Notification name for daemon events
extension Notification.Name {
    static let daemonEvent = Notification.Name("daemonEvent")
}
