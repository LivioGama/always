import XCTest
@testable import Always

/// Onboarding "You're All Set" app list (Slack #showcase, 16 Oct: "zut il y a
/// pas slack"). The reporter's running apps, reproduced from his screenshot.
final class OnboardingAppCandidatesTests: XCTestCase {
    private typealias App = OnboardingAppCandidates.App

    private let reportedRunning: [App] = [
        App(bundleId: "com.apple.Preview", name: "Aperçu"),
        App(bundleId: "com.openai.chat", name: "ChatGPT"),
        App(bundleId: "com.todesktop.230313mzl4w4u92", name: "Cursor"),
        App(bundleId: "company.thebrowser.dia", name: "Dia"),
        App(bundleId: "com.google.Chrome", name: "Google Chrome"),
        App(bundleId: "com.granola.app", name: "Granola"),
        App(bundleId: "com.apple.MobileSMS", name: "Messages"),
        App(bundleId: "com.apple.Passwords", name: "Mots de passe"),
        App(bundleId: "com.apple.Music", name: "Musique"),
        App(bundleId: "com.apple.Notes", name: "Notes"),
        App(bundleId: "com.spotify.client", name: "Spotify"),
        App(bundleId: "com.tinyspeck.slackmacgap", name: "Slack"),
        App(bundleId: "com.apple.Terminal", name: "Terminal"),
    ]

    /// The old rule: A→Z, first 8 — Slack is cut, Passwords is offered.
    func testOldAlphabeticalCutDroppedSlack() {
        let old = reportedRunning
            .sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
            .prefix(8).map(\.bundleId)
        XCTAssertFalse(old.contains("com.tinyspeck.slackmacgap"))
    }

    func testRunningSlackIsOffered() {
        let ids = OnboardingAppCandidates.rank(running: reportedRunning, installed: []).map(\.bundleId)
        XCTAssertTrue(ids.contains("com.tinyspeck.slackmacgap"))
        XCTAssertLessThanOrEqual(ids.count, OnboardingAppCandidates.limit)
    }

    func testInstalledButClosedSlackIsOffered() {
        let running = reportedRunning.filter { $0.bundleId != "com.tinyspeck.slackmacgap" }
        let ids = OnboardingAppCandidates.rank(
            running: running,
            installed: [App(bundleId: "com.tinyspeck.slackmacgap", name: "Slack")]
        ).map(\.bundleId)
        XCTAssertTrue(ids.contains("com.tinyspeck.slackmacgap"))
    }

    func testPasswordManagersAndOwnAppAreNeverOffered() {
        let ids = OnboardingAppCandidates.rank(
            running: reportedRunning + [App(bundleId: "com.always.v3", name: "Always")],
            installed: [], exclude: ["com.always.v3"]
        ).map(\.bundleId)
        XCTAssertFalse(ids.contains("com.apple.Passwords"))
        XCTAssertFalse(ids.contains("com.always.v3"))
    }

    func testLikelyTargetsBeatOtherRunningAppsAndDisplayIsAlphabetical() {
        let result = OnboardingAppCandidates.rank(running: reportedRunning, installed: [])
        let ids = result.map(\.bundleId)
        for target in ["com.openai.chat", "com.todesktop.230313mzl4w4u92", "company.thebrowser.dia",
                       "com.google.Chrome", "com.apple.MobileSMS", "com.apple.Notes"] {
            XCTAssertTrue(ids.contains(target), target)
        }
        let names = result.map(\.name)
        XCTAssertEqual(names, names.sorted { $0.localizedCaseInsensitiveCompare($1) == .orderedAscending })
    }

    func testDuplicatesCollapse() {
        let slack = App(bundleId: "com.tinyspeck.slackmacgap", name: "Slack")
        let result = OnboardingAppCandidates.rank(running: [slack, slack], installed: [slack])
        XCTAssertEqual(result, [slack])
    }

    func testFewRunningAppsStillListsInstalledTargets() {
        let result = OnboardingAppCandidates.rank(
            running: [App(bundleId: "com.apple.Terminal", name: "Terminal")],
            installed: [App(bundleId: "com.tinyspeck.slackmacgap", name: "Slack"),
                        App(bundleId: "com.apple.Notes", name: "Notes")])
        XCTAssertEqual(result.map(\.name), ["Notes", "Slack", "Terminal"])
    }

    /// The real path on this Mac: running + installed apps from NSWorkspace.
    func testLiveListOnThisMac() {
        let apps = OnboardingAppAllowlist.runningApps()
        print("onboarding apps:", apps.map { "\($0.name) [\($0.bundleId)] icon=\($0.icon != nil)" })
        XCTAssertLessThanOrEqual(apps.count, OnboardingAppCandidates.limit)
        XCTAssertFalse(apps.contains { OnboardingAppCandidates.neverOffer.contains($0.bundleId) })
        XCTAssertFalse(apps.contains { $0.bundleId.hasPrefix("com.always") })
        XCTAssertEqual(Set(apps.map(\.bundleId)).count, apps.count)
    }
}
