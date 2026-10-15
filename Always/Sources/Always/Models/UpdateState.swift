import Foundation

// Update state as the GUI sees it. The updater lives entirely in the Rust
// daemon (`always update`, SPEC §12a): it checks GitHub Releases, verifies the
// minisign signatures and runs the installer. This file only models what the
// daemon reports over the UDS socket and reduces those events to one state the
// views render. It is pure (no I/O, no AppKit) so it is unit-testable.

/// A release newer than the running build, as announced by `UpdateAvailable`.
struct UpdateOffer: Equatable {
    /// Running version, no leading "v".
    let current: String
    /// Newest published version, no leading "v".
    let latest: String
    /// GitHub release page (https only), shown as "Release notes".
    let releaseURL: URL?
    /// Whether `InstallUpdate` can work for THIS install. False for the dev
    /// instance, an unsupported install location, or a deb/rpm package.
    let canApply: Bool
    /// Ready-to-show sentence saying what to do instead when `canApply` is false.
    let applyHint: String
}

/// Where an `InstallUpdate` run is.
enum UpdateInstallPhase: Equatable {
    /// `percent` is nil until the daemon reports the first byte count.
    case downloading(percent: Int?)
    case verifying
    /// The verified installer was started detached. It quits the app and the
    /// daemon, swaps `Always.app` and relaunches, so a UDS disconnect from
    /// here on is EXPECTED, not an error.
    case handoff
}

enum UpdateState: Equatable {
    /// Nothing known yet this session (or the daemon is not connected).
    case unknown
    case checking
    case upToDate(version: String)
    /// The daemon asked GitHub and nothing is published yet.
    case noRelease
    /// Offline, rate limited or an unexpected error; `reason` is short text.
    case checkFailed(reason: String)
    case available(UpdateOffer)
    case installing(UpdateInstallPhase)
    /// An install stopped before the installer started; nothing was installed.
    case failed(stage: String, message: String)
}

/// The four daemon events, decoded. Every `data` value on the wire is a JSON
/// string (booleans are "true"/"false", versions have no leading "v").
enum UpdateEvent: Equatable {
    case available(UpdateOffer)
    case checkDone(result: String, current: String, latest: String)
    case progress(phase: String, percent: Int?)
    case failed(stage: String, message: String)

    /// Returns nil for every event that is not an update event (and for an
    /// `UpdateAvailable` with no version, which cannot be shown).
    init?(_ event: DaemonEvent) {
        let data = event.data ?? [:]
        switch event.type {
        case .updateAvailable:
            guard let latest = data["latest"], !latest.isEmpty else { return nil }
            self = .available(
                UpdateOffer(
                    current: data["current"] ?? "",
                    latest: latest,
                    releaseURL: Self.httpsURL(data["url"]),
                    canApply: data["can_apply"] == "true",
                    applyHint: data["apply_hint"] ?? ""
                ))
        case .updateCheckDone:
            self = .checkDone(
                result: data["result"] ?? "error",
                current: data["current"] ?? "",
                latest: data["latest"] ?? ""
            )
        case .updateProgress:
            self = .progress(
                phase: data["phase"] ?? "",
                percent: data["percent"].flatMap { Int($0) }.map { min(100, max(0, $0)) }
            )
        case .updateFailed:
            self = .failed(stage: data["stage"] ?? "", message: data["message"] ?? "")
        default:
            return nil
        }
    }

    /// The daemon's URL is opened in the user's browser, so only https is accepted.
    private static func httpsURL(_ raw: String?) -> URL? {
        guard let raw, let url = URL(string: raw), url.scheme?.lowercased() == "https" else {
            return nil
        }
        return url
    }
}

/// State + the newest known offer, and the transitions between them.
struct UpdateModel: Equatable {
    private(set) var state: UpdateState = .unknown
    /// Newest release known to be newer than this build. Survives a failed
    /// install (so "Try again" works) and a failed re-check; cleared when the
    /// daemon says we are up to date or nothing is published.
    private(set) var offer: UpdateOffer?

    // MARK: Daemon → GUI

    mutating func apply(_ event: UpdateEvent) {
        switch event {
        case .available(let newOffer):
            offer = newOffer
            // A live download/verify must not flip back to the offer card
            // because of a re-announcement; the handoff may (see below).
            if case .installing(let phase) = state, phase != .handoff { return }
            state = .available(newOffer)

        case .checkDone(let result, let current, let latest):
            if case .installing = state { return }
            switch result {
            case "up_to_date":
                offer = nil
                state = .upToDate(version: current)
            case "no_release":
                offer = nil
                state = .noRelease
            case "update_available":
                // An `UpdateAvailable` follows; until then keep spinning,
                // unless we already know that exact release.
                if let offer, latest.isEmpty || offer.latest == latest {
                    state = .available(offer)
                }
            default:
                // A failed re-check must not hide a known update.
                if let offer {
                    state = .available(offer)
                } else {
                    state = .checkFailed(reason: Self.reason(for: result))
                }
            }

        case .progress(let phase, let percent):
            switch phase {
            case "downloading": state = .installing(.downloading(percent: percent))
            case "verifying": state = .installing(.verifying)
            case "installing": state = .installing(.handoff)
            default: break
            }

        case .failed(let stage, let message):
            state = .failed(
                stage: stage,
                message: message.isEmpty ? "The update could not be installed." : message
            )
        }
    }

    /// Daemon connection dropped.
    mutating func daemonDisconnected() {
        switch state {
        case .checking:
            state = .unknown
        case .installing(.handoff):
            break  // expected: the installer is restarting the app
        case .installing:
            state = .failed(
                stage: "install",
                message: "Lost the connection to the Always daemon during the update."
            )
        default:
            break
        }
    }

    // MARK: GUI → daemon

    /// The user asked for a check. A known offer stays on screen (the answer
    /// either confirms it or replaces it); anything else shows the spinner.
    mutating func beginCheck() {
        switch state {
        case .available, .installing: return
        default: state = .checking
        }
    }

    /// The user asked to install. False when there is nothing installable.
    mutating func beginInstall() -> Bool {
        guard let offer, offer.canApply else { return false }
        if case .installing = state { return false }
        state = .installing(.downloading(percent: nil))
        return true
    }

    // MARK: Timeouts (the daemon never answered)

    mutating func checkTimedOut() {
        guard case .checking = state else { return }
        state = .checkFailed(reason: "no answer from the Always daemon")
    }

    mutating func handoffTimedOut() {
        guard case .installing(.handoff) = state else { return }
        state = .failed(
            stage: "install",
            message:
                "The installer did not restart Always. See ~/Library/Logs/Always/update-install.log."
        )
    }

    private static func reason(for result: String) -> String {
        switch result {
        case "offline": return "you appear to be offline"
        case "rate_limited": return "GitHub rate limit reached, try again later"
        default: return "unexpected error"
        }
    }
}
