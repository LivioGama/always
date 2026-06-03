import Foundation
import Combine
import os.log

/// Owns the in-app view of the daemon's glossary/correction pipeline:
///
/// - `pending`: candidate corrections the daemon detected (e.g. from
///   user-edited paste output) but is waiting on the user to approve
///   before persisting to the glossary.
/// - `lastLogged`: the most recently auto-applied correction, displayed
///   as a transient "Saved: X → Y" toast in the menu bar.
///
/// The daemon is the source of truth — this class is purely reactive.
/// State mutations happen ONLY in response to `.daemonEvent`
/// notifications posted by `UDSClient`. Approve/reject/capture commands
/// flow back to the daemon via `StateMonitor.shared`, which already
/// owns the singleton UDSClient connection (we deliberately don't open
/// a second socket — only one connection per process).
final class CorrectionsCenter: ObservableObject {
    /// Singleton because the menu-bar UI and the toast view both need
    /// to read the same `pending` array; passing an instance through
    /// SwiftUI environment plumbing would be more ceremony than benefit
    /// for a top-level menu-extra app.
    static let shared = CorrectionsCenter()

    @Published var pending: [Pending] = []

    /// Set when a `CorrectionLogged` event arrives; cleared after the
    /// toast UI calls `dismissLastLogged()` (or after a 2.5s timer in
    /// the toast view itself).
    @Published var lastLogged: Logged? = nil

    struct Pending: Identifiable, Hashable {
        /// Daemon-issued UUID. Echoed back on Approve/Reject so the
        /// daemon can look the candidate up in its own pending map.
        let id: String
        let wrong: String
        let right: String
    }

    struct Logged: Equatable {
        let wrong: String
        let right: String
        let timestamp: Date
    }

    private var cancellables = Set<AnyCancellable>()
    private let logger = Logger(subsystem: "com.always.app", category: "corrections-center")

    private init() {
        // Listen on the same NotificationCenter channel UDSClient uses.
        // We filter by event type rather than subscribing to a dedicated
        // channel so we stay consistent with how StateMonitor consumes
        // events.
        NotificationCenter.default.publisher(for: .daemonEvent)
            .compactMap { $0.object as? DaemonEvent }
            .sink { [weak self] event in
                self?.handle(event)
            }
            .store(in: &cancellables)
    }

    private func handle(_ event: DaemonEvent) {
        switch event.type {
        case .correctionPending:
            guard let payload = event.correctionPending else { return }
            DispatchQueue.main.async { [weak self] in
                self?.appendPending(payload)
            }

        case .correctionLogged:
            guard let payload = event.correctionLogged else { return }
            DispatchQueue.main.async { [weak self] in
                self?.applyLogged(payload)
            }

        default:
            break
        }
    }

    /// Append a candidate, deduping on `id`. Dedup matters because the
    /// daemon may rebroadcast a still-pending correction (e.g. after a
    /// reconnect) and we don't want the menu list to grow phantom rows.
    private func appendPending(_ payload: CorrectionPendingData) {
        if pending.contains(where: { $0.id == payload.id }) { return }
        pending.append(Pending(id: payload.id, wrong: payload.wrong, right: payload.right))
        logger.debug("queued pending correction \(payload.wrong, privacy: .public) -> \(payload.right, privacy: .public)")
    }

    /// `CorrectionLogged` means the daemon has *already* applied the
    /// correction (no user prompt needed). If the same `(wrong, right)`
    /// pair was sitting in `pending` — say, the user approved it via a
    /// different surface, or the daemon auto-approved an exact-string
    /// retry — we drop it so the menu count doesn't lie.
    private func applyLogged(_ payload: CorrectionLoggedData) {
        lastLogged = Logged(
            wrong: payload.wrong,
            right: payload.right,
            timestamp: Date()
        )
        pending.removeAll { $0.wrong == payload.wrong && $0.right == payload.right }
        logger.debug("logged correction \(payload.wrong, privacy: .public) -> \(payload.right, privacy: .public)")
    }

    // MARK: - User actions (flow back to the daemon)

    /// Tell the daemon to persist this correction to the glossary.
    /// We optimistically remove it from `pending` so the UI feels
    /// instant; if the daemon later rebroadcasts it (rare), the dedup
    /// in `appendPending` keeps things consistent.
    func approve(_ id: String) {
        pending.removeAll { $0.id == id }
        StateMonitor.shared.sendCommandWithData("ApproveCorrection", ["id": id])
    }

    /// Discard the candidate without persisting. Same optimistic-remove
    /// logic as `approve`.
    func reject(_ id: String) {
        pending.removeAll { $0.id == id }
        StateMonitor.shared.sendCommandWithData("RejectCorrection", ["id": id])
    }

    /// Ask the daemon to diff the user's current selection against the
    /// most recent paste and queue any difference as a Pending
    /// correction. Parameterless command — the daemon owns the diff.
    func captureNow() {
        StateMonitor.shared.sendCommand("CaptureCorrection")
    }

    /// Dismiss the toast. Called by the toast view after its 2.5s
    /// auto-hide timer fires, or by a manual close button.
    func dismissLastLogged() {
        lastLogged = nil
    }
}
