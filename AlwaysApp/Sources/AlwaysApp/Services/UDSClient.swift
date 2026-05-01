import Foundation
import Network
import Combine

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
    
    private func log(_ message: String) {
        let timestamp = Date().description
        let line = "[\(timestamp)] UDSClient: \(message)\n"
        let path = "/tmp/udsclient.log"
        if let data = line.data(using: .utf8) {
            if FileManager.default.fileExists(atPath: path) {
                if let fileHandle = FileHandle(forWritingAtPath: path) {
                    fileHandle.seekToEndOfFile()
                    fileHandle.write(data)
                    fileHandle.closeFile()
                }
            } else {
                try? data.write(to: URL(fileURLWithPath: path))
            }
        }
        NSLog("UDSClient: \(message)")
    }
    
    init(socketPath: String = "/tmp/always.sock") {
        self.socketPath = socketPath
        self.queue = DispatchQueue(label: "com.always.udsclient")
        log("Initializing with socket path: \(socketPath)")
        connect()
    }
    
    deinit {
        disconnect()
    }
    
    func connect() {
        guard !isConnected else { return }
        
        log("Attempting to connect to \(socketPath)")
        let endpoint = NWEndpoint.unix(path: socketPath)
        
        // CRITICAL: Use .tcp for Unix sockets, not default parameters
        let parameters = NWParameters.tcp
        parameters.allowLocalEndpointReuse = true
        connection = NWConnection(to: endpoint, using: parameters)
        
        connection?.stateUpdateHandler = { [weak self] state in
            guard let self = self else { return }
            self.log("State change: \(state)")
            switch state {
            case .ready:
                DispatchQueue.main.async {
                    self.isConnected = true
                    self.connectionError = nil
                    self.log("✅ Connected to daemon")
                }
                self.startReceiving()
            case .failed(let error):
                DispatchQueue.main.async {
                    self.isConnected = false
                    self.connectionError = error.localizedDescription
                    self.log("❌ Connection failed - \(error)")
                }
                self.scheduleReconnect()
            case .waiting(let error):
                // NWConnection over Unix sockets sticks in .waiting when the
                // server isn't listening yet (e.g. daemon still starting).
                // Don't sit there indefinitely — cancel and reconnect after
                // a short delay so we actually pick up the socket.
                DispatchQueue.main.async {
                    self.connectionError = error.localizedDescription
                    self.log("⏳ Waiting - \(error) — cancelling and scheduling reconnect")
                }
                self.connection?.cancel()
                self.connection = nil
                DispatchQueue.main.async {
                    self.isConnected = false
                }
                self.scheduleReconnect()
            case .preparing:
                self.log("🔄 Preparing connection")
            case .setup:
                self.log("⚙️ Setting up connection")
            case .cancelled:
                self.log("🚫 Connection cancelled")
            @unknown default:
                self.log("❓ Unknown state: \(state)")
            }
        }
        
        connection?.start(queue: queue)
        log("Connection started on queue")
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
        log("startReceiving() called")
        connection?.receive(minimumIncompleteLength: 1, maximumLength: 65536) { [weak self] data, _, isComplete, error in
            guard let self = self else { return }
            
            if let data = data, !data.isEmpty {
                if let jsonString = String(data: data, encoding: .utf8) {
                    self.log("Received data: \(jsonString)")
                    self.processMessage(jsonString)
                }
            }
            
            if let error = error {
                self.log("Receive error - \(error)")
                self.scheduleReconnect()
                return
            }
            
            if isComplete {
                self.log("Connection closed by server")
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
                    print("UDSClient: Failed to decode event - \(error)")
                }
            }
        }
    }
    
    private func handleEvent(_ event: DaemonEvent) {
        NSLog("UDSClient: handleEvent - type: \(event.type.rawValue)")
        DispatchQueue.main.async {
            NotificationCenter.default.post(name: .daemonEvent, object: event)
        }
    }

    /// Send a JSON command line to the daemon over the UDS socket.
    /// The daemon executes it in-process and broadcasts the resulting
    /// DaemonEvent back to all connected subscribers.
    func sendCommand(_ commandType: String) {
        guard let connection = connection, isConnected else {
            log("⚠️ sendCommand(\(commandType)) skipped — not connected")
            return
        }

        let json = "{\"type\":\"\(commandType)\"}\n"
        guard let data = json.data(using: .utf8) else { return }

        connection.send(content: data, completion: .contentProcessed { [weak self] error in
            if let error = error {
                self?.log("❌ sendCommand(\(commandType)) failed - \(error)")
            } else {
                self?.log("→ sent command \(commandType)")
            }
        })
    }
}

// Notification name for daemon events
extension Notification.Name {
    static let daemonEvent = Notification.Name("daemonEvent")
}
