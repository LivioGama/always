import XCTest
import AppKit
import Combine
@testable import AlwaysApp

final class AlwaysAppTests: XCTestCase {

    // MARK: - DaemonEvent codable round-trip

    func testDaemonEventDecoding() throws {
        let json = """
        {"type": "ListeningStarted", "data": null}
        """
        let data = json.data(using: .utf8)!
        let event = try JSONDecoder().decode(DaemonEvent.self, from: data)
        XCTAssertEqual(event.type, .listeningStarted)
    }

    func testDaemonEventEncoding() throws {
        let json = """
        {"type": "ListeningStarted", "data": null}
        """
        let data = json.data(using: .utf8)!
        let event = try JSONDecoder().decode(DaemonEvent.self, from: data)
        let encoded = try JSONEncoder().encode(event)
        let encodedJson = String(data: encoded, encoding: .utf8)!
        XCTAssertTrue(encodedJson.contains("ListeningStarted"))
    }

    // MARK: - UDSClient socket path resolution

    func testSocketPathResolution() throws {
        let path = UDSClient.defaultSocketPath()
        #if os(macOS)
        XCTAssertTrue(path.contains("Library/Caches/Always"),
                      "macOS socket path should live under Library/Caches/Always, got \(path)")
        XCTAssertTrue(path.hasSuffix("always.sock"))
        #else
        XCTAssertTrue(path.hasSuffix("always.sock"))
        #endif
    }

    func testUDSClientDefaultsAreSane() throws {
        // Constructing the client kicks off an async connect attempt against the
        // default socket path. We're not asserting connection success — we're
        // verifying the published @Published defaults expose the expected
        // pre-connection state and that construction itself doesn't crash.
        let client = UDSClient(socketPath: "/tmp/always-test-\(UUID().uuidString).sock")
        XCTAssertFalse(client.isConnected, "Fresh client should not report itself connected")
        XCTAssertNil(client.connectionError, "Fresh client should not have a stored error")
        client.disconnect()
    }

    // MARK: - Config / DaemonStatus model decoding

    func testConfigModel() throws {
        let json = """
        {
            "sttEnergyThreshold": 0.5,
            "hearEnergyThreshold": 0.3,
            "sttCooldownMs": 150,
            "sttSilence": 0.4,
            "sttAutoEnter": true,
            "groqApiKey": null,
            "sileroThreshold": 0.5,
            "shortcutPause": "ctrl+alt+p",
            "shortcutAutoEnter": "ctrl+alt+a",
            "shortcutForcePaste": "ctrl+alt+v"
        }
        """
        let data = json.data(using: .utf8)!
        let config = try JSONDecoder().decode(Config.self, from: data)
        XCTAssertEqual(config.sttEnergyThreshold, 0.5)
        XCTAssertEqual(config.hearEnergyThreshold, 0.3)
        XCTAssertEqual(config.sttCooldownMs, 150)
        XCTAssertEqual(config.sttSilence, 0.4)
        XCTAssertTrue(config.sttAutoEnter)
        XCTAssertEqual(config.sileroThreshold, 0.5)
        XCTAssertEqual(config.shortcutPause, "ctrl+alt+p")
    }

    func testDaemonStatusModel() throws {
        let json = """
        {"isRunning": true, "pid": 12345, "logPath": "/var/log/always.log"}
        """
        let data = json.data(using: .utf8)!
        let status = try JSONDecoder().decode(DaemonStatus.self, from: data)
        XCTAssertTrue(status.isRunning)
        XCTAssertEqual(status.pid, 12345)
        XCTAssertEqual(status.logPath, "/var/log/always.log")
    }

    // MARK: - Pure settings/onboarding helpers

    func testFormatShortcutUsesMacModifierSymbols() throws {
        XCTAssertEqual(formatShortcut("ctrl+alt+p"), "⌃⌥P")
        XCTAssertEqual(formatShortcut("shift+meta+space"), "⇧⌘SPACE")
        XCTAssertEqual(formatShortcut("control+option+a"), "⌃⌥A")
    }

    func testMaskedApiKeyIsNotPersisted() throws {
        XCTAssertFalse(shouldPersistApiKey(""))
        XCTAssertFalse(shouldPersistApiKey("••••••••"))
        XCTAssertTrue(shouldPersistApiKey("gsk_live_test"))
    }

    func testGroqValidationStatusMapping() throws {
        XCTAssertEqual(groqKeyValidationResult(statusCode: 200), .valid)
        XCTAssertEqual(
            groqKeyValidationResult(statusCode: 401),
            .invalid("Invalid API key - Groq rejected the credentials")
        )
        XCTAssertEqual(
            groqKeyValidationResult(statusCode: nil),
            .invalid("Invalid API key - Groq rejected the credentials")
        )
    }

    // MARK: - StatusOverlayController flash protection
    //
    // Regression: calling show(state:) while a flash is animating used to
    // immediately replace the flash icon with the "Listening" state, so users
    // never saw the pause/resume confirmation. Fixed by deferring show() until
    // isFlashActive() returns false. These tests exercise that contract.

    /// `flash()` and `show()` touch AppKit (NSWindow). Skip if we can't bring
    /// up NSApplication in the test environment.
    private func ensureAppKit() throws {
        _ = NSApplication.shared
        // No assertion needed — just touching .shared is enough on macOS.
    }

    func testFlashIsNotClobberedByShow() throws {
        try ensureAppKit()
        let controller = StatusOverlayController.shared

        controller.flash(state: .autoEnterOn, duration: 1.0)
        // Simulate voice-activity arriving 100 ms into the flash.
        Thread.sleep(forTimeInterval: 0.1)
        controller.show(state: .voiceActivity)

        // Flash window is still ~900 ms from finishing — the show() call
        // must not have cancelled it.
        XCTAssertTrue(controller.isFlashActive(),
                      "show(state:) called during a flash must not clobber the flash")

        // Wait out the flash so subsequent tests start with a clean slate.
        Thread.sleep(forTimeInterval: 1.1)
        // Drain any deferred work the flash completion enqueued onto the main queue.
        RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.05))
        XCTAssertFalse(controller.isFlashActive(),
                       "Flash must clear itself after duration elapses")
    }

    func testFlashClearedAfterDuration() throws {
        try ensureAppKit()
        let controller = StatusOverlayController.shared
        controller.flash(state: .paused, duration: 0.3)
        XCTAssertTrue(controller.isFlashActive())
        Thread.sleep(forTimeInterval: 0.4)
        RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.05))
        XCTAssertFalse(controller.isFlashActive())
    }

    // MARK: - StateMonitor event handling

    /// Post a synthetic .daemonEvent and assert StateMonitor mutates the
    /// matching @Published flag. Mirrors what UDSClient does when a real
    /// daemon event arrives off the socket.
    func testStateMonitorTogglesVoiceActivityFromEvents() throws {
        try ensureAppKit()
        let monitor = StateMonitor.shared

        // Decode events from JSON so we exercise the same Codable path
        // production uses.
        let detected = try JSONDecoder().decode(
            DaemonEvent.self,
            from: #"{"type":"VoiceActivityDetected","data":null}"#.data(using: .utf8)!
        )
        let ended = try JSONDecoder().decode(
            DaemonEvent.self,
            from: #"{"type":"VoiceActivityEnded","data":null}"#.data(using: .utf8)!
        )

        let voiceOn = expectation(description: "isVoiceActivity becomes true")
        var observed: [Bool] = []
        var bag = Set<AnyCancellable>()
        monitor.$isVoiceActivity
            .dropFirst() // Skip the current value at subscription time.
            .sink { value in
                observed.append(value)
                if value { voiceOn.fulfill() }
            }
            .store(in: &bag)

        NotificationCenter.default.post(name: .daemonEvent, object: detected)
        wait(for: [voiceOn], timeout: 2.0)

        let voiceOff = expectation(description: "isVoiceActivity becomes false")
        var bag2 = Set<AnyCancellable>()
        monitor.$isVoiceActivity
            .dropFirst()
            .sink { value in
                if !value { voiceOff.fulfill() }
            }
            .store(in: &bag2)

        NotificationCenter.default.post(name: .daemonEvent, object: ended)
        wait(for: [voiceOff], timeout: 2.0)

        XCTAssertFalse(monitor.isVoiceActivity)
    }

    func testStateMonitorTogglesPauseFromEvents() throws {
        try ensureAppKit()
        let monitor = StateMonitor.shared

        // Make sure starting state is unpaused. If a previous test paused us,
        // post Resumed first and wait briefly.
        if monitor.isPaused {
            let resumed = try JSONDecoder().decode(
                DaemonEvent.self,
                from: #"{"type":"Resumed","data":null}"#.data(using: .utf8)!
            )
            NotificationCenter.default.post(name: .daemonEvent, object: resumed)
            RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.1))
        }

        let paused = try JSONDecoder().decode(
            DaemonEvent.self,
            from: #"{"type":"Paused","data":null}"#.data(using: .utf8)!
        )
        let pausedExp = expectation(description: "isPaused becomes true")
        var bag = Set<AnyCancellable>()
        monitor.$isPaused
            .dropFirst()
            .sink { if $0 { pausedExp.fulfill() } }
            .store(in: &bag)

        NotificationCenter.default.post(name: .daemonEvent, object: paused)
        wait(for: [pausedExp], timeout: 2.0)
        XCTAssertTrue(monitor.isPaused)

        // Reset for any later tests.
        let resumed = try JSONDecoder().decode(
            DaemonEvent.self,
            from: #"{"type":"Resumed","data":null}"#.data(using: .utf8)!
        )
        NotificationCenter.default.post(name: .daemonEvent, object: resumed)
        RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.1))
    }

    // MARK: - UDS protocol versioning

    func testHelloEventDecodesVersion() throws {
        let json = #"{"type":"Hello","data":{"version":1}}"#
        let event = try JSONDecoder().decode(
            DaemonEvent.self,
            from: json.data(using: .utf8)!
        )
        XCTAssertEqual(event.type, .hello)
        XCTAssertEqual(event.helloVersion, 1)
        XCTAssertNil(event.data, "Hello payload should not collapse into the string-data dict")
    }

    func testHelloEventRoundTripsThroughCodable() throws {
        let original = DaemonEvent(type: .hello, helloVersion: 1)
        let encoded = try JSONEncoder().encode(original)
        let decoded = try JSONDecoder().decode(DaemonEvent.self, from: encoded)
        XCTAssertEqual(decoded.type, .hello)
        XCTAssertEqual(decoded.helloVersion, 1)
    }

    func testProtocolVersionMatchesDaemon() throws {
        // The Rust daemon's PROTOCOL_VERSION is pinned to 1 in
        // tests/uds_protocol_test.rs. Bumping either side without the
        // other will be caught by both tests.
        XCTAssertEqual(UDS_PROTOCOL_VERSION, 1)
    }

    func testHelloWithMismatchedVersionIsObservable() throws {
        // The client refuses connections on mismatch; here we just
        // verify the decoder surfaces a non-1 version so handleEvent
        // has the data it needs to refuse.
        let json = #"{"type":"Hello","data":{"version":99}}"#
        let event = try JSONDecoder().decode(
            DaemonEvent.self,
            from: json.data(using: .utf8)!
        )
        XCTAssertEqual(event.helloVersion, 99)
    }

    // MARK: - Sensitivity preset round-trip
    //
    // The threshold pairs MUST stay in lockstep with `SensitivityPreset`
    // in `src/always/config.rs`. Drift between the two would silently
    // make the GUI picker write different values than the CLI.

    func testNormalPresetMatchesDefaultConfig() {
        let (stt, hear) = SensitivityPreset.normal.thresholds
        XCTAssertEqual(stt, Config.defaultConfig.sttEnergyThreshold)
        XCTAssertEqual(hear, Config.defaultConfig.hearEnergyThreshold)
    }

    func testPresetRoundTripsThroughThresholds() {
        for preset in SensitivityPreset.allCases {
            let (s, h) = preset.thresholds
            XCTAssertEqual(SensitivityPreset.from(stt: s, hear: h), preset)
        }
    }

    func testCustomThresholdsResolveToNil() {
        XCTAssertNil(SensitivityPreset.from(stt: 0.123, hear: 0.456))
    }

    func testPresetThresholdsAreOrderedByStrictness() {
        // Higher sensitivity = lower energy thresholds.
        let high = SensitivityPreset.high.thresholds.stt
        let normal = SensitivityPreset.normal.thresholds.stt
        let low = SensitivityPreset.low.thresholds.stt
        XCTAssertLessThan(high, normal)
        XCTAssertLessThan(normal, low)
    }
}
