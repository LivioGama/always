import Combine
import Foundation
import os.log

/// Owns the Models-tab state. Observes daemon UDS events and exposes
/// published collections the SwiftUI view binds to.
///
/// Pattern matches `StateMonitor` — global singleton, subscribes to
/// `Notification.Name.daemonEvent`, fans out into `@Published`
/// properties. Commands flow back through the shared `UDSClient`
/// instance owned by `CLIService`.
@MainActor
final class ModelManagerClient: ObservableObject {
    static let shared = ModelManagerClient()

    private let logger = Logger(subsystem: "com.always.app", category: "models")

    /// Latest catalog snapshot from the daemon. Empty until the first
    /// `ModelsList` event arrives (the client sends a `ListModels`
    /// command on every UDS reconnect to trigger one).
    @Published var models: [ModelInfo] = []
    /// Per-id download progress (0.0–100.0). Cleared when the matching
    /// model finishes / cancels / fails.
    @Published var downloadProgress: [String: Double] = [:]
    /// Per-id absolute byte counts — used by the row's secondary
    /// caption ("120 MB / 456 MB").
    @Published var downloadBytes: [String: (downloaded: UInt64, total: UInt64)] = [:]
    /// Ids currently being verified (SHA256) or extracted (tar.gz).
    @Published var verifying: Set<String> = []
    @Published var extracting: Set<String> = []
    /// Active backend in canonical wire form — `groq` or `local:<id>`.
    @Published var activeBackend: String = "groq"
    /// Per-id terminal error message. Surfaced in the row until the
    /// user retries or dismisses.
    @Published var errors: [String: String] = [:]

    /// Commands flow through `StateMonitor.shared` (the singleton that
    /// owns the daemon's `UDSClient` instance). Inbound events come
    /// over `Notification.Name.daemonEvent`, broadcast by the same
    /// client.
    private var observer: NSObjectProtocol?
    private var cancellables = Set<AnyCancellable>()

    private init() {
        observer = NotificationCenter.default.addObserver(
            forName: .daemonEvent,
            object: nil,
            queue: .main
        ) { [weak self] note in
            guard let event = note.object as? DaemonEvent else { return }
            Task { @MainActor in
                self?.handle(event)
            }
        }

        // Auto-request the catalog whenever the GUI (re)connects to the
        // daemon. Without this, a Settings → Models open that races the
        // bootstrap reconnect drops its one-shot request and the panel
        // stays stuck on "Waiting for model catalog from daemon…".
        StateMonitor.shared.$isDaemonConnected
            .removeDuplicates()
            .filter { $0 }
            .sink { [weak self] _ in
                self?.requestModelsList()
            }
            .store(in: &cancellables)
    }

    deinit {
        if let observer {
            NotificationCenter.default.removeObserver(observer)
        }
    }

    /// Active model id when the backend is local — `nil` for the
    /// remote Groq path.
    var activeModelId: String? {
        if activeBackend == "groq" { return nil }
        if activeBackend.hasPrefix("local:") {
            return String(activeBackend.dropFirst("local:".count))
        }
        return nil
    }

    /// Convenience for the View: split into the two sections Handy
    /// renders ("Downloaded Models" + "Available to Download").
    var downloadedModels: [ModelInfo] {
        models.filter { $0.is_downloaded }
            .sorted { lhs, rhs in
                if lhs.is_recommended != rhs.is_recommended {
                    return lhs.is_recommended
                }
                return lhs.name < rhs.name
            }
    }

    var availableModels: [ModelInfo] {
        models.filter { !$0.is_downloaded }
            .sorted { lhs, rhs in
                if lhs.is_recommended != rhs.is_recommended {
                    return lhs.is_recommended
                }
                return lhs.size_mb < rhs.size_mb
            }
    }

    // MARK: Commands (UI → daemon)

    /// Ask the daemon for a fresh catalog snapshot. Sent on Models-tab
    /// open + on every UDS reconnect.
    func requestModelsList() {
        StateMonitor.shared.sendCommand("ListModels")
    }

    func download(_ modelId: String) {
        errors.removeValue(forKey: modelId)
        downloadProgress[modelId] = 0
        StateMonitor.shared.sendCommandWithData("DownloadModel", ["model_id": modelId])
    }

    func cancelDownload(_ modelId: String) {
        StateMonitor.shared.sendCommandWithData("CancelModelDownload", ["model_id": modelId])
    }

    func delete(_ modelId: String) {
        StateMonitor.shared.sendCommandWithData("DeleteModel", ["model_id": modelId])
    }

    /// Switch the active backend. Pass `"groq"` for the remote API,
    /// `"local:<model_id>"` for a downloaded engine.
    func setActive(backend: String) {
        StateMonitor.shared.sendCommandWithData("SetActiveTranscriber", ["backend": backend])
    }

    // MARK: Event handler (daemon → UI)

    private func handle(_ event: DaemonEvent) {
        switch event.type {
        case .modelsList:
            if let list = event.modelsList {
                models = list.models
            }
        case .modelDownloadProgress:
            if let p = event.modelDownloadProgress {
                downloadProgress[p.model_id] = p.percentage
                downloadBytes[p.model_id] = (p.downloaded, p.total)
            }
        case .modelDownloadComplete:
            if let id = event.modelId?.model_id {
                downloadProgress.removeValue(forKey: id)
                downloadBytes.removeValue(forKey: id)
                errors.removeValue(forKey: id)
                // Refresh the snapshot to pick up `is_downloaded = true`
                // and the cleared `partial_size`. The daemon also sends
                // a fresh `ModelsList`, but requesting one here makes
                // the UI snappy when reconnecting mid-download.
                requestModelsList()
            }
        case .modelDownloadCancelled:
            if let id = event.modelId?.model_id {
                downloadProgress.removeValue(forKey: id)
                downloadBytes.removeValue(forKey: id)
            }
        case .modelDownloadFailed:
            if let err = event.modelError {
                downloadProgress.removeValue(forKey: err.model_id)
                downloadBytes.removeValue(forKey: err.model_id)
                errors[err.model_id] = err.error
                logger.error("download failed for \(err.model_id, privacy: .public): \(err.error, privacy: .public)")
            }
        case .modelVerificationStarted:
            if let id = event.modelId?.model_id {
                verifying.insert(id)
            }
        case .modelVerificationCompleted:
            if let id = event.modelId?.model_id {
                verifying.remove(id)
            }
        case .modelExtractionStarted:
            if let id = event.modelId?.model_id {
                extracting.insert(id)
            }
        case .modelExtractionCompleted:
            if let id = event.modelId?.model_id {
                extracting.remove(id)
            }
        case .modelExtractionFailed:
            if let err = event.modelError {
                extracting.remove(err.model_id)
                errors[err.model_id] = err.error
            }
        case .activeTranscriberChanged:
            if let payload = event.activeTranscriber {
                activeBackend = payload.backend
            }
        default:
            break
        }
    }
}
