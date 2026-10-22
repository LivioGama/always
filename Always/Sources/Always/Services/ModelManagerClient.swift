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
    /// Ids that finished extraction and are waiting for the ModelsList
    /// snapshot to confirm `is_downloaded = true`. Bridges the gap between
    /// extraction completing and the catalog refresh arriving.
    @Published var installing: Set<String> = []
    /// Active backend in canonical wire form — `groq`, `apple` or
    /// `local:<id>`; empty until the daemon reports it, so nothing shows as
    /// selected before then.
    @Published var activeBackend: String = ""
    /// Backend the user asked to switch to but the daemon hasn't confirmed
    /// yet. Set optimistically on `setActive`; cleared when
    /// `ActiveTranscriberChanged` arrives. Drives the "Loading…" spinner
    /// in the Use button.
    @Published var loadingBackend: String? = nil
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
            MainActor.assumeIsolated {
                self?.handle(event)
            }
        }

        // Auto-request the catalog whenever the GUI (re)connects to the
        // daemon. Without this, a Settings → Models open that races the
        // bootstrap reconnect drops its one-shot request and the panel
        // stays stuck on "Waiting for model catalog from daemon…".
        // Note: The daemon now sends ModelsList on connection, so this
        // is a fallback for edge cases.
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

    // MARK: Commands (UI → daemon)

    /// Active model id when the backend is local — `nil` for the
    /// remote Groq path.
    var activeModelId: String? {
        if activeBackend == "groq" { return nil }
        if activeBackend.hasPrefix("local:") {
            return String(activeBackend.dropFirst("local:".count))
        }
        return nil
    }

    /// Whether the current active model produces live partial text.
    ///
    /// Groq is deliberately NOT treated as streaming. The API returns one
    /// finished transcript per utterance and the daemon emits no
    /// `TranscriptChunk` for it, so claiming otherwise produced exactly
    /// the behaviour the user reported: the overlay enabled its live-text
    /// path, no partial ever arrived so it sat on "Listening" for the
    /// whole sentence, and then `TranscriptFinal` fired a 1.5s flash of
    /// text at the very end — a preview of something already pasted,
    /// arriving after it was any use.
    ///
    /// Only a model whose catalog entry declares `supports_streaming`
    /// gets the live-text treatment.
    var activeModelSupportsStreaming: Bool {
        guard let modelId = activeModelId else { return false }
        if let model = models.first(where: { $0.id == modelId }) {
            return model.supports_streaming
        }
        return false
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

    /// Engine picked in onboarding that can't apply yet: a model still
    /// downloading, or Groq waiting for the restart that loads its key.
    /// Re-applied on every catalog snapshot (one arrives on each reconnect)
    /// until the daemon confirms it, so neither leaving the step nor a
    /// daemon restart (which kills an in-flight download) loses it.
    var pendingBackend: String?

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
    /// Sets `loadingBackend` optimistically so the UI can show a spinner
    /// immediately — cleared when `ActiveTranscriberChanged` confirms.
    func setActive(backend: String) {
        // Any other explicit choice overrides a pending onboarding pick.
        if pendingBackend != backend { pendingBackend = nil }
        loadingBackend = backend
        errors.removeValue(forKey: Self.errorKey(forBackend: backend))
        StateMonitor.shared.sendCommandWithData("SetActiveTranscriber", ["backend": backend])
        // Safety net: the daemon answers every switch (success, no-op or
        // failure), but a daemon that died mid-load never will. Never leave
        // "Loading…" spinning with no way out.
        // Only the LATEST attempt's timer may fire: X, Y, X within the window
        // must not let the first X's timer cut the second X attempt short.
        switchGeneration += 1
        let attempt = backend
        let generation = switchGeneration
        DispatchQueue.main.asyncAfter(deadline: .now() + switchTimeout) { [weak self] in
            MainActor.assumeIsolated {
                guard let self, self.switchGeneration == generation,
                      self.loadingBackend == attempt else { return }
                self.loadingBackend = nil
                self.errors[Self.errorKey(forBackend: attempt)] =
                    "Didn't finish loading. Try again, or pick another model."
            }
        }
    }

    /// Push `pendingBackend` to a (possibly just restarted) daemon.
    private func applyPending(_ backend: String, _ list: [ModelInfo]) {
        guard backend.hasPrefix("local:") else { return setActive(backend: backend) }
        let id = Self.errorKey(forBackend: backend)
        guard let m = list.first(where: { $0.id == id }) else { pendingBackend = nil; return }
        if m.is_downloaded && !m.is_downloading {
            setActive(backend: backend)
        } else if !m.is_downloading {
            // A restarted daemon dropped the download: resume it from the
            // partial file. A duplicate request is a no-op in the daemon.
            StateMonitor.shared.sendCommandWithData("DownloadModel", ["model_id": id])
        }
    }

    /// How long "Loading…" may spin before the row gives up and says so.
    var switchTimeout: TimeInterval = 120

    /// Bumped on every `setActive`; a timeout only acts if it is still current.
    private var switchGeneration = 0

    /// Row key for a backend's error: the model id for `local:<id>`, the
    /// backend itself otherwise (`apple`, `groq`).
    nonisolated static func errorKey(forBackend backend: String) -> String {
        backend.hasPrefix("local:") ? String(backend.dropFirst("local:".count)) : backend
    }

    // MARK: Event handler (daemon → UI)

    private func handle(_ event: DaemonEvent) {
        switch event.type {
        case .modelsList:
            if let list = event.modelsList {
                models = list.models
                // A model that's now confirmed on disk is fully done —
                // clear ALL in-progress state so the row settles into the
                // Downloaded section with a Use button.
                for m in list.models where m.is_downloaded {
                    installing.remove(m.id)
                    verifying.remove(m.id)
                    extracting.remove(m.id)
                    downloadProgress.removeValue(forKey: m.id)
                    downloadBytes.removeValue(forKey: m.id)
                }
                if let pending = pendingBackend { applyPending(pending, list.models) }
            }
        case .modelDownloadProgress:
            if let p = event.modelDownloadProgress {
                if p.percentage >= 100.0 {
                    // Bytes done. Drop the progress bar and enter the persistent
                    // `installing` floor state immediately — the daemon still has
                    // to verify + extract, and those events may lag. `installing`
                    // holds the row "in progress" with no gap until ModelsList
                    // confirms is_downloaded, so it never flickers back to the
                    // Download button between phases.
                    downloadProgress.removeValue(forKey: p.model_id)
                    downloadBytes.removeValue(forKey: p.model_id)
                    installing.insert(p.model_id)
                } else {
                    downloadProgress[p.model_id] = p.percentage
                    downloadBytes[p.model_id] = (p.downloaded, p.total)
                }
            }
        case .modelDownloadComplete:
            if let id = event.modelId?.model_id {
                downloadProgress.removeValue(forKey: id)
                downloadBytes.removeValue(forKey: id)
                errors.removeValue(forKey: id)
                // DownloadComplete fires LAST (after verify + extract). Keep the
                // `installing` floor set until ModelsList confirms is_downloaded.
                installing.insert(id)
                requestModelsList()
            }
        case .modelDownloadCancelled:
            if let id = event.modelId?.model_id {
                if pendingBackend == "local:\(id)" { pendingBackend = nil }
                downloadProgress.removeValue(forKey: id)
                downloadBytes.removeValue(forKey: id)
                installing.remove(id)
                verifying.remove(id)
                extracting.remove(id)
            }
        case .modelDownloadFailed:
            if let err = event.modelError {
                if pendingBackend == "local:\(err.model_id)" { pendingBackend = nil }
                downloadProgress.removeValue(forKey: err.model_id)
                downloadBytes.removeValue(forKey: err.model_id)
                installing.remove(err.model_id)
                verifying.remove(err.model_id)
                extracting.remove(err.model_id)
                errors[err.model_id] = err.error
                logger.error("download failed for \(err.model_id, privacy: .public): \(err.error, privacy: .public)")
            }
        case .modelVerificationStarted:
            if let id = event.modelId?.model_id {
                verifying.insert(id)
                installing.insert(id)   // floor — survives VerificationCompleted
            }
        case .modelVerificationCompleted:
            if let id = event.modelId?.model_id {
                // Only drop the specific "Verifying…" label. `installing` floor
                // remains, so there's no gap before ExtractionStarted (directory
                // models) or DownloadComplete (single-file models).
                verifying.remove(id)
            }
        case .modelExtractionStarted:
            if let id = event.modelId?.model_id {
                extracting.insert(id)
                installing.insert(id)   // floor
            }
        case .modelExtractionCompleted:
            if let id = event.modelId?.model_id {
                // Drop the "Extracting…" label; installing floor remains until
                // ModelsList confirms is_downloaded.
                extracting.remove(id)
                requestModelsList()
            }
        case .modelExtractionFailed:
            if let err = event.modelError {
                extracting.remove(err.model_id)
                installing.remove(err.model_id)
                verifying.remove(err.model_id)
                errors[err.model_id] = err.error
            }
        case .activeTranscriberFailed:
            if let backend = event.data?["backend"] {
                let error = event.data?["error"] ?? "Model failed to load"
                if loadingBackend == backend { loadingBackend = nil }
                if pendingBackend == backend { pendingBackend = nil }
                errors[Self.errorKey(forBackend: backend)] = error
                logger.error("switch to \(backend, privacy: .public) failed: \(error, privacy: .public)")
            }
        case .activeTranscriberChanged:
            if let payload = event.activeTranscriber {
                activeBackend = payload.backend
                if pendingBackend == payload.backend { pendingBackend = nil }
                // A slow load that succeeds after the timeout already put an
                // error on this row; it is running now, so drop the stale one.
                errors.removeValue(forKey: Self.errorKey(forBackend: payload.backend))
                // Daemon confirmed the switch — clear the optimistic loading
                // indicator regardless of which backend became active.
                loadingBackend = nil
            }
        default:
            break
        }
    }
}
