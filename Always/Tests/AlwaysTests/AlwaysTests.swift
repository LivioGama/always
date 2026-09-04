import XCTest
import AppKit
import Combine
@testable import Always

final class AlwaysTests: XCTestCase {

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
            "sttAdaptiveSilence": true,
            "sttAutoEnter": true,
            "autoEnterDelayMs": 4000,
            "groqApiKey": null,
            "groqKeySaved": false,
            "sileroThreshold": 0.5,
            "shortcutPause": "ctrl+alt+p",
            "shortcutAutoEnter": "ctrl+alt+a",
            "shortcutForcePaste": "ctrl+alt+v",
            "shortcutCorrectionDialog": "ctrl+alt+w",
            "shortcutMasterPause": "ctrl+alt+shift+p",
            "postprocessEnabled": true,
            "idlePauseSecs": 600,
            "idlePauseAction": "pause",
            "audibleStatusSound": "off"
        }
        """
        let data = json.data(using: .utf8)!
        let config = try JSONDecoder().decode(Config.self, from: data)
        XCTAssertEqual(config.sttEnergyThreshold, 0.5)
        XCTAssertEqual(config.hearEnergyThreshold, 0.3)
        XCTAssertEqual(config.sttCooldownMs, 150)
        XCTAssertEqual(config.sttSilence, 0.4)
        XCTAssertTrue(config.sttAdaptiveSilence)
        XCTAssertTrue(config.sttAutoEnter)
        XCTAssertEqual(config.autoEnterDelayMs, 4000)
        XCTAssertEqual(config.sileroThreshold, 0.5)
        XCTAssertEqual(config.shortcutPause, "ctrl+alt+p")
        XCTAssertEqual(config.idlePauseSecs, 600)
        XCTAssertEqual(config.audibleStatusSound, "off")
    }

    // Regression: the CLI's `config show` output uses `auto_enter_delay_ms`
    // (in milliseconds). Earlier builds emitted `auto_enter_delay_secs` and
    // Swift parsed it differently — silently breaking the round-trip.
    func testConfigFromCLIParsesAutoEnterDelayMs() throws {
        let cliOutput = """
        stt_energy_threshold: 0.012
        hear_energy_threshold: 0.001
        stt_cooldown_ms: 150
        stt_silence: 2.0
        stt_auto_enter: true
        auto_enter_delay_ms: 4000
        silero_threshold: 0.5
        idle_pause_secs: 600
            audible_status_sound: high
        postprocess_enabled: true
        """
        guard let config = Config.fromCLI(output: cliOutput) else {
            return XCTFail("fromCLI returned nil")
        }
        XCTAssertEqual(config.autoEnterDelayMs, 4000)
        XCTAssertEqual(config.sttSilence, 2.0)
        XCTAssertTrue(config.sttAutoEnter)
        XCTAssertEqual(config.idlePauseSecs, 600)
        XCTAssertEqual(config.idlePauseAction, "pause")
        XCTAssertEqual(config.audibleStatusSound, "high")
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
        // `formatShortcut` was replaced by `parseShortcutParts` + `partSymbol`.
        // Verify the new helpers produce the same keycap symbols.
        XCTAssertEqual(parseShortcutParts("ctrl+alt+p").map(partSymbol).joined(), "⌃⌥P")
        XCTAssertEqual(parseShortcutParts("shift+meta+space").map(partSymbol).joined(), "⇧⌘Space")
        XCTAssertEqual(parseShortcutParts("control+option+a").map(partSymbol).joined(), "⌃⌥A")
        // Fn key — the new modifier-less shortcut the old function didn't handle.
        XCTAssertEqual(parseShortcutParts("fn").map(partSymbol).joined(), "Fn")
    }

    func testMaskedApiKeyIsNotPersisted() throws {
        XCTAssertFalse(shouldPersistApiKey(""))
        XCTAssertFalse(shouldPersistApiKey("••••••••"))
        XCTAssertFalse(shouldPersistApiKey("***"))
        XCTAssertFalse(shouldPersistApiKey("*** (in keychain)"))
        XCTAssertFalse(shouldPersistApiKey("●●●●"))
        XCTAssertTrue(shouldPersistApiKey("gsk_live_test"))
    }

    func testConfigFromCLIIgnoresMaskedGroqKeyPlaceholder() throws {
        let cliOutput = """
        groq_api_key: *** (in keychain)
        """
        guard let config = Config.fromCLI(output: cliOutput) else {
            return XCTFail("fromCLI returned nil")
        }
        XCTAssertNil(config.groqApiKey)
        XCTAssertTrue(config.groqKeySaved)
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

    func testSingleInstanceGuardOnlyMatchesExecutablePath() throws {
        XCTAssertTrue(SingleInstanceGuard.isAlwaysGUIProcess(
            command: "/Applications/Always.app/Contents/MacOS/Always"
        ))
        XCTAssertTrue(SingleInstanceGuard.isAlwaysGUIProcess(
            command: "/Users/livio/Documents/always/Always/.build/release/Always"
        ))
        XCTAssertFalse(SingleInstanceGuard.isAlwaysGUIProcess(
            command: "zsh -lc open /Applications/Always.app && ps ax | grep Always.app/Contents/MacOS/Always"
        ))
    }

    func testApplicationTerminationRequestsDaemonStop() throws {
        let delegate = AppDelegate()
        var didRequestDaemonStop = false

        delegate.stopDaemonForAppTermination {
            didRequestDaemonStop = true
        }

        XCTAssertTrue(
            didRequestDaemonStop,
            "Quit cleanup must stop the daemon so it cannot keep transcribing after the app exits"
        )
    }

    // MARK: - StatusOverlayController flash protection
    //
    // Contract: a low-priority show(state:) during a flash defers until the
    // flash completes (the user must see the toggle confirmation), while
    // time-critical live feedback (`preemptsFlash` — listening/transcribing)
    // cuts the flash short and shows immediately. Deferring "Listening"
    // behind a 1.5–4s flash made the badge appear seconds late.

    /// `flash()` and `show()` touch AppKit (NSWindow). Skip if we can't bring
    /// up NSApplication in the test environment.
    private func ensureAppKit() throws {
        _ = NSApplication.shared
        // No assertion needed — just touching .shared is enough on macOS.
    }

    private func isStatusOverlayVisible() -> Bool {
        NSApplication.shared.windows.contains {
            ($0 is StatusOverlayWindow) && $0.isVisible && $0.alphaValue > 0
        }
    }

    private func waitForStatusOverlayToHide(timeout: TimeInterval = 2.0) {
        let deadline = Date(timeIntervalSinceNow: timeout)
        while isStatusOverlayVisible(), Date() < deadline {
            RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.02))
        }
    }

    func testFlashIsNotClobberedByLowPriorityShow() throws {
        try ensureAppKit()
        let controller = StatusOverlayController.shared

        controller.flash(state: .autoEnterOn, duration: 1.0)
        // Simulate a low-priority state arriving 100 ms into the flash.
        Thread.sleep(forTimeInterval: 0.1)
        controller.show(state: .processing)

        // Flash window is still ~900 ms from finishing — a non-preempting
        // show() call must not have cancelled it.
        XCTAssertTrue(controller.isFlashActive(),
                      "low-priority show(state:) during a flash must not clobber the flash")

        // Wait out the flash so subsequent tests start with a clean slate.
        Thread.sleep(forTimeInterval: 1.1)
        // Drain any deferred work the flash completion enqueued onto the main queue.
        RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.05))
        XCTAssertFalse(controller.isFlashActive(),
                       "Flash must clear itself after duration elapses")
    }

    func testListeningPreemptsActiveFlash() throws {
        try ensureAppKit()
        let controller = StatusOverlayController.shared

        controller.flash(state: .autoEnterOn, duration: 1.0)
        XCTAssertTrue(controller.isFlashActive())
        // Voice activity arriving mid-flash must end the flash immediately —
        // the listening badge is live feedback and cannot wait up to 4s.
        Thread.sleep(forTimeInterval: 0.1)
        controller.show(state: .voiceActivity)

        XCTAssertFalse(controller.isFlashActive(),
                       "voiceActivity must preempt an active flash, not defer behind it")
        XCTAssertTrue(isStatusOverlayVisible(),
                      "overlay must be showing the preempting state immediately")

        // Clean up for subsequent tests.
        controller.hide()
        RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.05))
    }

    func testPreemptsFlashClassification() {
        XCTAssertTrue(OverlayState.voiceActivity.preemptsFlash)
        XCTAssertTrue(OverlayState.transcribing.preemptsFlash)
        XCTAssertTrue(OverlayState.transcribingElapsed(seconds: 3).preemptsFlash)
        XCTAssertTrue(
            OverlayState.transcribingWithText(text: "hi", isInterim: true).preemptsFlash)
        XCTAssertFalse(OverlayState.processing.preemptsFlash)
        XCTAssertFalse(OverlayState.paused.preemptsFlash)
        XCTAssertFalse(OverlayState.autoEnterCountdown(secondsRemaining: 2).preemptsFlash)
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

    func testListeningStartedWithoutVoiceDoesNotShowOverlay() throws {
        try ensureAppKit()
        let monitor = StateMonitor.shared
        let controller = StatusOverlayController.shared

        controller.hide()
        monitor.isDaemonConnected = true
        monitor.isPaused = false
        monitor.isMasterPaused = false
        monitor.isIdleAutoPaused = false
        monitor.isTranscribing = false
        monitor.isVoiceActivity = false
        monitor.isListeningActive = false
        waitForStatusOverlayToHide()
        XCTAssertFalse(isStatusOverlayVisible(), "test must start with the overlay hidden")

        let listeningStarted = try JSONDecoder().decode(
            DaemonEvent.self,
            from: #"{"type":"ListeningStarted","data":null}"#.data(using: .utf8)!
        )
        NotificationCenter.default.post(name: .daemonEvent, object: listeningStarted)
        RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.1))

        XCTAssertTrue(monitor.isListeningActive)
        XCTAssertFalse(monitor.isVoiceActivity)
        XCTAssertFalse(isStatusOverlayVisible())
    }

    func testVoiceActivityDetectedShowsOverlayImmediately() throws {
        try ensureAppKit()
        let monitor = StateMonitor.shared
        let controller = StatusOverlayController.shared

        controller.hide()
        monitor.isDaemonConnected = true
        monitor.isPaused = false
        monitor.isMasterPaused = false
        monitor.isIdleAutoPaused = false
        monitor.isTranscribing = false
        monitor.isVoiceActivity = false
        RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.2))

        let detected = try JSONDecoder().decode(
            DaemonEvent.self,
            from: #"{"type":"VoiceActivityDetected","data":null}"#.data(using: .utf8)!
        )
        NotificationCenter.default.post(name: .daemonEvent, object: detected)
        RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.1))

        XCTAssertTrue(monitor.isVoiceActivity)
        XCTAssertTrue(isStatusOverlayVisible())
    }

    func testTranscriptionInterimPreservesFrenchUTF8Preview() throws {
        try ensureAppKit()
        let monitor = StateMonitor.shared
        let expected = "Salut ça va, même en français."

        let event = try JSONDecoder().decode(
            DaemonEvent.self,
            from: #"{"type":"TranscriptionInterim","data":{"text":"Salut ça va, même en français."}}"#.data(using: .utf8)!
        )

        NotificationCenter.default.post(name: .daemonEvent, object: event)
        RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.1))

        XCTAssertEqual(monitor.partialTranscript, expected)
    }

    // Regression: streaming producers (e.g. Nemotron) re-process the growing
    // audio buffer on every preview call and emit the full cumulative text
    // each time — not just the newly-added chunk. StateMonitor MUST replace
    // partialTranscript wholesale on every event, never append. A single-event
    // test can't catch a handler that concatenates instead of overwriting, so
    // this posts a sequence of events with clearly different cumulative text
    // and asserts each one replaces the previous value exactly.
    func testStreamingPreviewReplacesNotAppendsAcrossMultipleEvents() throws {
        try ensureAppKit()
        let monitor = StateMonitor.shared

        func postInterim(_ text: String) throws {
            let json = "{\"type\":\"TranscriptionInterim\",\"data\":{\"text\":\"\(text)\"}}"
            let event = try JSONDecoder().decode(DaemonEvent.self, from: json.data(using: .utf8)!)
            NotificationCenter.default.post(name: .daemonEvent, object: event)
            RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.1))
        }

        try postInterim("hello")
        XCTAssertEqual(monitor.partialTranscript, "hello")

        // Later preview snapshot: cumulative text changed, not a simple
        // concatenation of the previous chunk.
        try postInterim("hello world")
        XCTAssertEqual(
            monitor.partialTranscript, "hello world",
            "must replace the previous preview, not append to it"
        )

        // A third event confirms replace-not-append holds across the whole
        // sequence, not just a two-event tolerance.
        try postInterim("hello world, how are you")
        XCTAssertEqual(
            monitor.partialTranscript, "hello world, how are you",
            "must replace again, never accumulate prior previews"
        )
    }

    // Live provisional transcript (Groq live preview): a TranscriptChunk
    // arriving WHILE the user is still talking must render its text in the
    // overlay even when the active model does not stream. The daemon is
    // the authority on when previews flow; the GUI must not discard them
    // behind a streaming-capability gate.
    func testLivePreviewTextRendersDuringVoiceActivityOnNonStreamingModel() throws {
        try ensureAppKit()
        let monitor = StateMonitor.shared

        monitor.currentModelSupportsStreaming = false
        monitor.isDaemonConnected = true
        monitor.isPaused = false
        monitor.isMasterPaused = false
        monitor.isIdleAutoPaused = false
        monitor.isTranscribing = false
        monitor.isVoiceActivity = false
        monitor.partialTranscript = ""
        monitor.invalidateAppliedOverlay()
        RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.2))

        let detected = try JSONDecoder().decode(
            DaemonEvent.self,
            from: #"{"type":"VoiceActivityDetected","data":null}"#.data(using: .utf8)!
        )
        NotificationCenter.default.post(name: .daemonEvent, object: detected)
        RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.1))
        XCTAssertTrue(monitor.isVoiceActivity)

        let chunk = try JSONDecoder().decode(
            DaemonEvent.self,
            from: #"{"type":"TranscriptChunk","data":{"text":"draft while talking"}}"#
                .data(using: .utf8)!
        )
        NotificationCenter.default.post(name: .daemonEvent, object: chunk)
        RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.2))

        XCTAssertEqual(monitor.partialTranscript, "draft while talking")
        XCTAssertTrue(isStatusOverlayVisible())
        let shown = NSApplication.shared.windows
            .compactMap { $0 as? StatusOverlayWindow }
            .compactMap { $0.currentOverlayState }
            .first
        XCTAssertEqual(
            shown,
            .transcribingWithText(text: "draft while talking", isInterim: true),
            "partial text must render while the user is still talking, not the bare listening badge"
        )

        // Cleanup for later tests.
        let ended = try JSONDecoder().decode(
            DaemonEvent.self,
            from: #"{"type":"VoiceActivityEnded","data":null}"#.data(using: .utf8)!
        )
        NotificationCenter.default.post(name: .daemonEvent, object: ended)
        monitor.partialTranscript = ""
        RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.1))
    }

    // A fresh utterance must open with a clean badge: stale partial text
    // from the previous utterance is dropped on the first
    // VoiceActivityDetected of a new utterance.
    func testFreshVoiceActivityClearsStalePartialTranscript() throws {
        try ensureAppKit()
        let monitor = StateMonitor.shared

        monitor.isVoiceActivity = false
        monitor.isTranscribing = false
        monitor.partialTranscript = "words from the previous utterance"
        RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.1))

        let detected = try JSONDecoder().decode(
            DaemonEvent.self,
            from: #"{"type":"VoiceActivityDetected","data":null}"#.data(using: .utf8)!
        )
        NotificationCenter.default.post(name: .daemonEvent, object: detected)
        RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.1))

        XCTAssertEqual(monitor.partialTranscript, "")

        // Cleanup for later tests.
        let ended = try JSONDecoder().decode(
            DaemonEvent.self,
            from: #"{"type":"VoiceActivityEnded","data":null}"#.data(using: .utf8)!
        )
        NotificationCenter.default.post(name: .daemonEvent, object: ended)
        RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.1))
    }

    func testLongTranscriptGrowsOverlayHeightWithinCap() throws {
        try ensureAppKit()
        let window = StatusOverlayWindow()

        // Baseline: a short state uses the classic HUD size; remember where
        // the bottom edge sits.
        window.show(state: .voiceActivity, instant: true)
        RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.1))
        XCTAssertEqual(window.frame.size.height, StatusOverlayWindow.overlayHeight)
        let anchoredBottomY = window.frame.origin.y

        let longText = String(repeating: "This transcript must wrap inside the HUD. ", count: 20)
        window.show(state: .transcribingWithText(text: longText, isInterim: true), instant: true)
        RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.1))

        // Width never changes; height grows to fit, bounded by the cap.
        XCTAssertEqual(window.frame.size.width, StatusOverlayWindow.overlayWidth)
        XCTAssertGreaterThan(window.frame.size.height, StatusOverlayWindow.overlayHeight)
        XCTAssertLessThanOrEqual(window.frame.size.height, OverlayHUDSizing.maxHeight)
        XCTAssertEqual(window.contentView?.frame.size.width, StatusOverlayWindow.overlayWidth)
        XCTAssertEqual(window.contentView?.frame.size.height, window.frame.size.height)
        // Bottom edge stays anchored — the panel grows upward, so it can
        // never be pushed off the bottom of the screen.
        XCTAssertEqual(window.frame.origin.y, anchoredBottomY)

        // Shrinks back when the transcript goes away.
        window.show(state: .voiceActivity, instant: true)
        RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.1))
        XCTAssertEqual(window.frame.size.height, StatusOverlayWindow.overlayHeight)
        XCTAssertEqual(window.frame.origin.y, anchoredBottomY)

        window.orderOut(nil)
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
        // assertForOverFulfill=false: StateMonitor is a singleton across the
        // test suite. If another test toggled isPaused → true earlier and
        // we're observing a transition with `.dropFirst()`, the sink may
        // still receive multiple `true` values before XCTest tears the
        // expectation down. We only care that it became true at least once.
        pausedExp.assertForOverFulfill = false
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
        // Pinned in lockstep with `PROTOCOL_VERSION` in
        // `src/always/event.rs` and `tests/uds_protocol_test.rs`. Bumping
        // either side without updating the matching constant on the
        // other side will fail both tests at once.
        XCTAssertEqual(UDS_PROTOCOL_VERSION, 12)
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

    // MARK: - Correction events (decoder)
    //
    // CorrectionLogged / CorrectionPending share the same
    // `#[serde(tag="type", content="data")]` envelope as Hello on the
    // Rust side. These tests pin the wire format so a daemon-side
    // serde refactor can't silently break the menu-bar UI.

    func testCorrectionLoggedEventDecodes() throws {
        let json = #"{"type":"CorrectionLogged","data":{"wrong":"kuburnetes","right":"kubernetes"}}"#
        let event = try JSONDecoder().decode(
            DaemonEvent.self,
            from: json.data(using: .utf8)!
        )
        XCTAssertEqual(event.type, .correctionLogged)
        XCTAssertEqual(event.correctionLogged?.wrong, "kuburnetes")
        XCTAssertEqual(event.correctionLogged?.right, "kubernetes")
        // Typed payloads must NOT collapse into the loose data dict —
        // the menu view depends on the typed accessor.
        XCTAssertNil(event.data)
        XCTAssertNil(event.correctionPending)
    }

    func testCorrectionPendingEventDecodes() throws {
        let json = #"{"type":"CorrectionPending","data":{"id":"7c0c9e1a-aaaa-bbbb-cccc-deadbeef0001","wrong":"kuburnetes","right":"kubernetes"}}"#
        let event = try JSONDecoder().decode(
            DaemonEvent.self,
            from: json.data(using: .utf8)!
        )
        XCTAssertEqual(event.type, .correctionPending)
        XCTAssertEqual(event.correctionPending?.id, "7c0c9e1a-aaaa-bbbb-cccc-deadbeef0001")
        XCTAssertEqual(event.correctionPending?.wrong, "kuburnetes")
        XCTAssertEqual(event.correctionPending?.right, "kubernetes")
        XCTAssertNil(event.data)
        XCTAssertNil(event.correctionLogged)
    }

    // MARK: - Overlay HUD vertical growth for long transcripts

    /// ~127 chars: too tall for the classic 130pt HUD, but within the cap.
    private static let longTranscript =
        "This live transcript keeps growing while the user talks, passing one "
        + "hundred characters so the HUD must grow taller to show it."

    private static let overflowingTranscript = String(
        repeating: "the quick brown fox jumps over the lazy dog ", count: 30
    ) + "and these final words must remain visible"

    func testHUDHeightStaysBaseForShortText() {
        XCTAssertEqual(OverlayHUDSizing.hudHeight(forText: "Listening"),
                       OverlayHUDSizing.baseHeight)
        XCTAssertEqual(OverlayHUDSizing.hudHeight(forText: ""),
                       OverlayHUDSizing.baseHeight)
    }

    func testHUDHeightGrowsForLongTranscriptUpToCap() {
        let height = OverlayHUDSizing.hudHeight(forText: Self.longTranscript)
        XCTAssertGreaterThan(height, OverlayHUDSizing.baseHeight,
                             "a 100+ char transcript must grow the panel")
        XCTAssertLessThanOrEqual(height, OverlayHUDSizing.maxHeight)
        XCTAssertEqual(OverlayHUDSizing.hudHeight(forText: Self.overflowingTranscript),
                       OverlayHUDSizing.maxHeight,
                       "unbounded text clamps at the growth cap")
    }

    func testFittedTailKeepsShortTextIntact() {
        XCTAssertEqual(OverlayHUDSizing.fittedTail(of: "Listening"), "Listening")
        XCTAssertEqual(OverlayHUDSizing.fittedTail(of: Self.longTranscript),
                       Self.longTranscript,
                       "text that fits within the cap must not be truncated")
    }

    func testFittedTailHeadTruncatesKeepingNewestWords() {
        let fitted = OverlayHUDSizing.fittedTail(of: Self.overflowingTranscript)
        XCTAssertTrue(fitted.hasPrefix("…"), "truncation marker belongs at the START")
        XCTAssertTrue(fitted.hasSuffix("and these final words must remain visible"),
                      "the newest words must survive head-truncation")
        XCTAssertLessThan(fitted.count, Self.overflowingTranscript.count)
        XCTAssertLessThanOrEqual(OverlayHUDSizing.textHeight(of: fitted),
                                 OverlayHUDSizing.maxTextHeight,
                                 "fitted text must fit the capped label area")
        // The displayed layout for capped text sizes to the fitted lines —
        // above the base, never above the cap.
        let layoutHeight = OverlayHUDSizing.layout(forText: Self.overflowingTranscript).height
        XCTAssertGreaterThan(layoutHeight, OverlayHUDSizing.baseHeight)
        XCTAssertLessThanOrEqual(layoutHeight, OverlayHUDSizing.maxHeight)
    }

    func testOverlayViewGrowsAndShrinksWithState() throws {
        try ensureAppKit()
        let view = StatusOverlayView(
            frame: NSRect(x: 0, y: 0,
                          width: StatusOverlayWindow.overlayWidth,
                          height: StatusOverlayWindow.overlayHeight)
        )
        XCTAssertEqual(view.desiredHeight, OverlayHUDSizing.baseHeight)

        view.state = .transcribingWithText(text: Self.longTranscript, isInterim: true)
        XCTAssertGreaterThan(view.desiredHeight, OverlayHUDSizing.baseHeight,
                             "panel must grow for a long live transcript")
        XCTAssertLessThanOrEqual(view.desiredHeight, OverlayHUDSizing.maxHeight)

        let grown = view.desiredHeight
        view.state = .transcribingWithText(text: Self.overflowingTranscript, isInterim: true)
        XCTAssertGreaterThanOrEqual(view.desiredHeight, grown,
                                    "overflowing text is at least as tall as long text")
        XCTAssertLessThanOrEqual(view.desiredHeight, OverlayHUDSizing.maxHeight,
                                 "growth clamps at the cap")

        view.state = .voiceActivity
        XCTAssertEqual(view.desiredHeight, OverlayHUDSizing.baseHeight,
                       "panel must shrink back when the transcript goes away")
    }

    func testCompactModeStaysFixedPill() throws {
        try ensureAppKit()
        let view = StatusOverlayView(
            frame: NSRect(x: 0, y: 0,
                          width: StatusOverlayWindow.compactWidth,
                          height: StatusOverlayWindow.compactHeight),
            compact: true
        )
        view.state = .transcribingWithText(text: Self.overflowingTranscript, isInterim: true)
        XCTAssertEqual(view.desiredHeight, StatusOverlayWindow.compactHeight,
                       "compact pill never grows")
    }

}
