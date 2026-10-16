import AppKit
import XCTest
@testable import Always

/// Settings → Status Overlay (Normal / Compact / Hidden) applies live — the
/// panel promises "no restart needed" (Slack #showcase, 16 Oct: the change
/// only took effect after a restart).
final class OverlayDisplayModeLiveTests: XCTestCase {
    private let key = OverlayDisplayMode.defaultsKey
    private var saved: String?

    override func setUpWithError() throws {
        _ = NSApplication.shared
        saved = UserDefaults.standard.string(forKey: key)
        setMode(.normal)
        StatusOverlayController.shared.hide()
        drain(0.3)
    }

    override func tearDownWithError() throws {
        StatusOverlayController.shared.hide()
        drain(0.3)
        if let saved { UserDefaults.standard.set(saved, forKey: key) } else {
            UserDefaults.standard.removeObject(forKey: key)
        }
        drain(0.05)
    }

    private func setMode(_ mode: OverlayDisplayMode) {
        UserDefaults.standard.set(mode.rawValue, forKey: key)
    }

    private func drain(_ seconds: TimeInterval) {
        RunLoop.main.run(until: Date(timeIntervalSinceNow: seconds))
    }

    private var hud: StatusOverlayWindow? {
        NSApplication.shared.windows.compactMap { $0 as? StatusOverlayWindow }.first
    }

    private var hudVisible: Bool {
        guard let hud else { return false }
        return hud.isVisible && hud.alphaValue > 0
    }

    func testNextShowUsesNewModeWithoutRestart() throws {
        StatusOverlayController.shared.show(state: .voiceActivity)
        drain(0.05)
        XCTAssertEqual(hud?.frame.width, StatusOverlayWindow.overlayWidth)
        StatusOverlayController.shared.hide()
        drain(0.3)

        setMode(.compact)
        StatusOverlayController.shared.show(state: .voiceActivity)
        drain(0.05)
        XCTAssertEqual(hud?.frame.width, StatusOverlayWindow.compactWidth)
    }

    func testCompactAppliesToTheIndicatorAlreadyOnScreen() throws {
        StatusOverlayController.shared.show(state: .voiceActivity)
        drain(0.05)
        XCTAssertEqual(hud?.frame.width, StatusOverlayWindow.overlayWidth)

        setMode(.compact)
        drain(0.1)

        XCTAssertTrue(hudVisible, "switching mode must not drop a live indicator")
        XCTAssertEqual(hud?.frame.width, StatusOverlayWindow.compactWidth,
                       "a visible indicator must switch to Compact at once")
        XCTAssertEqual(hud?.frame.height, StatusOverlayWindow.compactHeight)
    }

    func testHiddenRemovesTheIndicatorAlreadyOnScreen() throws {
        StatusOverlayController.shared.show(state: .voiceActivity)
        drain(0.05)
        XCTAssertTrue(hudVisible)

        setMode(.hidden)
        drain(0.4)

        XCTAssertFalse(hudVisible, "Hidden must take the indicator away at once")
    }

    func testBackToNormalRestoresFullSize() throws {
        setMode(.compact)
        StatusOverlayController.shared.show(state: .voiceActivity)
        drain(0.05)
        XCTAssertEqual(hud?.frame.width, StatusOverlayWindow.compactWidth)

        setMode(.normal)
        drain(0.1)

        XCTAssertEqual(hud?.frame.width, StatusOverlayWindow.overlayWidth)
    }

    func testUnrelatedDefaultsWriteDoesNotTouchTheIndicator() throws {
        StatusOverlayController.shared.show(state: .voiceActivity)
        drain(0.05)
        let before = hud?.frame
        UserDefaults.standard.set(UUID().uuidString, forKey: "always.test.unrelated")
        drain(0.1)
        XCTAssertTrue(hudVisible)
        XCTAssertEqual(hud?.frame, before)
        UserDefaults.standard.removeObject(forKey: "always.test.unrelated")
    }

    // MARK: First-install default (#128)

    private func scratchDefaults() -> (UserDefaults, String) {
        let suite = "always.test.overlay.\(UUID().uuidString)"
        return (UserDefaults(suiteName: suite)!, suite)
    }

    func testFreshInstallDefaultsToNormal() {
        let (d, suite) = scratchDefaults()
        defer { d.removePersistentDomain(forName: suite) }
        XCTAssertEqual(OverlayDisplayMode.current(in: d), .normal)
        OverlayDisplayMode.registerDefault(in: d)
        XCTAssertEqual(d.string(forKey: OverlayDisplayMode.defaultsKey), "normal")
    }

    func testRegisteredDefaultNeverOverridesTheUsersChoice() {
        let (d, suite) = scratchDefaults()
        defer { d.removePersistentDomain(forName: suite) }
        d.set("compact", forKey: OverlayDisplayMode.defaultsKey)
        OverlayDisplayMode.registerDefault(in: d)
        XCTAssertEqual(OverlayDisplayMode.current(in: d), .compact)
    }

    func testSavedHiddenStaysHiddenAndMissingBecomesNormal() {
        let (d, suite) = scratchDefaults()
        defer { d.removePersistentDomain(forName: suite) }
        OverlayDisplayMode.registerDefault(in: d)
        XCTAssertEqual(OverlayDisplayMode.current(in: d), .normal, "missing → Normal")
        d.set("hidden", forKey: OverlayDisplayMode.defaultsKey)
        OverlayDisplayMode.registerDefault(in: d)  // every launch registers again
        XCTAssertEqual(OverlayDisplayMode.current(in: d), .hidden, "saved Hidden must survive launch")
        XCTAssertEqual(d.string(forKey: OverlayDisplayMode.defaultsKey), "hidden")
    }
}
