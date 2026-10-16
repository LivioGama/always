import Foundation

/// Which apps the last onboarding page offers to turn voice typing on in.
///
/// It used to be the running Dock apps, sorted A→Z and cut at 8 — so on a
/// busy Mac Slack (an "S") fell off the end while Preview and Passwords made
/// the list (Slack #showcase, 16 Oct: "zut il y a pas slack"). Selection is
/// now by how likely you are to dictate into the app, and display stays A→Z.
enum OnboardingAppCandidates {
    struct App: Equatable {
        let bundleId: String
        let name: String
    }

    static let limit = 10

    /// Finder display name without a trailing ".app". Only the suffix goes:
    /// an app whose name merely contains ".app" (e.g. "Foo.app Tools.app")
    /// keeps it.
    static func strippingAppSuffix(_ displayName: String) -> String {
        guard displayName.hasSuffix(".app"), displayName.count > ".app".count else { return displayName }
        return String(displayName.dropLast(".app".count))
    }

    /// Apps people dictate into, most likely first. Offered when running OR
    /// installed. Slack in a browser (app.slack.com) is covered by turning
    /// on that browser — the allowlist is per app.
    static let likelyTargets: [String] = [
        "com.tinyspeck.slackmacgap",       // Slack
        "com.apple.MobileSMS",             // Messages
        "com.openai.chat",                 // ChatGPT
        "com.anthropic.claudefordesktop",  // Claude
        "com.todesktop.230313mzl4w4u92",   // Cursor
        "com.microsoft.VSCode",
        "dev.zed.Zed",
        "com.google.Chrome",
        "com.apple.Safari",
        "company.thebrowser.Browser",      // Arc
        "company.thebrowser.dia",          // Dia
        "org.mozilla.firefox",
        "com.apple.mail",
        "com.apple.Notes",
        "notion.id",
        "net.whatsapp.WhatsApp",
        "com.microsoft.teams2",
        "com.hnc.Discord",
        "ru.keepcoder.Telegram",
    ]

    /// Never offered: nothing to dictate into, or somewhere words must not
    /// land unasked (password managers).
    static let neverOffer: Set<String> = [
        "com.apple.finder",
        "com.apple.systempreferences",
        "com.apple.ActivityMonitor",
        "com.apple.Passwords",
        "com.apple.keychainaccess",
        "com.1password.1password",
        "com.bitwarden.desktop",
    ]

    /// Likely targets (running before merely installed, in `likelyTargets`
    /// order), then other running apps A→Z; deduped, capped at `limit`,
    /// returned sorted by name for display.
    static func rank(running: [App], installed: [App], exclude: Set<String> = []) -> [App] {
        let blocked = neverOffer.union(exclude)
        let priority = Dictionary(
            likelyTargets.enumerated().map { ($1, $0) }, uniquingKeysWith: { a, _ in a })
        let runningIds = Set(running.map(\.bundleId))

        var seen = Set<String>()
        var pool: [App] = []
        for app in running + installed where !blocked.contains(app.bundleId) {
            if seen.insert(app.bundleId).inserted { pool.append(app) }
        }

        func key(_ app: App) -> (Int, Int, String) {
            if let p = priority[app.bundleId] {
                return (runningIds.contains(app.bundleId) ? 0 : 1, p, "")
            }
            return (2, 0, app.name.lowercased())
        }
        let chosen = pool.sorted { key($0) < key($1) }.prefix(limit)
        return chosen.sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
    }
}
