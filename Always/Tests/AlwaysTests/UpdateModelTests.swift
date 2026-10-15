import XCTest
@testable import Always

/// The daemon-driven update UI state (`UpdateModel`) and the wire decoding of
/// the four update events (SPEC §12a). The JSON below is the shape pinned by
/// the daemon's `tests/uds_protocol_test.rs`.
final class UpdateModelTests: XCTestCase {

    private func decode(_ json: String) throws -> DaemonEvent {
        try JSONDecoder().decode(DaemonEvent.self, from: Data(json.utf8))
    }

    private func event(_ json: String) throws -> UpdateEvent {
        let decoded = try decode(json)
        return try XCTUnwrap(UpdateEvent(decoded), "not an update event: \(json)")
    }

    private let availableJSON = """
        {"type":"UpdateAvailable","data":{"current":"0.1.0","latest":"0.2.0",\
        "url":"https://github.com/LivioGama/always/releases/tag/v0.2.0",\
        "can_apply":"true","apply_hint":""}}
        """

    private func offered(canApply: Bool = true) -> UpdateModel {
        var model = UpdateModel()
        model.apply(
            .available(
                UpdateOffer(
                    current: "0.1.0", latest: "0.2.0",
                    releaseURL: URL(string: "https://github.com/LivioGama/always/releases/tag/v0.2.0"),
                    canApply: canApply,
                    applyHint: canApply ? "" : "Update with your package manager."
                )))
        return model
    }

    // MARK: Wire decoding

    func testUpdateAvailableDecodes() throws {
        let decoded = try decode(availableJSON)
        XCTAssertEqual(decoded.type, .updateAvailable)
        guard case .available(let offer) = try event(availableJSON) else {
            return XCTFail("expected .available")
        }
        XCTAssertEqual(offer.current, "0.1.0")
        XCTAssertEqual(offer.latest, "0.2.0")
        XCTAssertEqual(offer.releaseURL?.absoluteString, "https://github.com/LivioGama/always/releases/tag/v0.2.0")
        XCTAssertTrue(offer.canApply)
        XCTAssertEqual(offer.applyHint, "")
    }

    func testUpdateAvailableWithoutApplyKeepsHint() throws {
        let json = """
            {"type":"UpdateAvailable","data":{"current":"0.1.0","latest":"0.2.0","url":"",\
            "can_apply":"false","apply_hint":"Run 'always update' in a terminal."}}
            """
        guard case .available(let offer) = try event(json) else { return XCTFail("expected .available") }
        XCTAssertFalse(offer.canApply)
        XCTAssertEqual(offer.applyHint, "Run 'always update' in a terminal.")
        XCTAssertNil(offer.releaseURL)
    }

    func testNonHttpsReleaseURLIsDropped() throws {
        let json = """
            {"type":"UpdateAvailable","data":{"current":"0.1.0","latest":"0.2.0",\
            "url":"file:///etc/passwd","can_apply":"true","apply_hint":""}}
            """
        guard case .available(let offer) = try event(json) else { return XCTFail("expected .available") }
        XCTAssertNil(offer.releaseURL)
    }

    func testCheckDoneProgressAndFailedDecode() throws {
        XCTAssertEqual(
            try event(#"{"type":"UpdateCheckDone","data":{"result":"no_release","current":"0.1.0","latest":""}}"#),
            .checkDone(result: "no_release", current: "0.1.0", latest: ""))
        XCTAssertEqual(
            try event(#"{"type":"UpdateProgress","data":{"phase":"downloading","percent":"42","detail":"a.tar.gz"}}"#),
            .progress(phase: "downloading", percent: 42))
        XCTAssertEqual(
            try event(#"{"type":"UpdateProgress","data":{"phase":"installing","percent":"","detail":""}}"#),
            .progress(phase: "installing", percent: nil))
        XCTAssertEqual(
            try event(#"{"type":"UpdateFailed","data":{"stage":"verify","message":"bad signature"}}"#),
            .failed(stage: "verify", message: "bad signature"))
    }

    func testOtherEventsAreNotUpdateEvents() throws {
        XCTAssertNil(UpdateEvent(try decode(#"{"type":"Heartbeat"}"#)))
    }

    // MARK: Check results

    func testUpToDate() throws {
        var model = UpdateModel()
        model.beginCheck()
        XCTAssertEqual(model.state, .checking)
        model.apply(.checkDone(result: "up_to_date", current: "0.1.0", latest: "0.1.0"))
        XCTAssertEqual(model.state, .upToDate(version: "0.1.0"))
    }

    func testNoReleaseIsNotAnError() {
        var model = UpdateModel()
        model.beginCheck()
        model.apply(.checkDone(result: "no_release", current: "0.1.0", latest: ""))
        XCTAssertEqual(model.state, .noRelease)
    }

    func testCheckFailuresAreMutedReasons() {
        for (result, fragment) in [("offline", "offline"), ("rate_limited", "rate limit"), ("error", "unexpected")] {
            var model = UpdateModel()
            model.beginCheck()
            model.apply(.checkDone(result: result, current: "0.1.0", latest: ""))
            guard case .checkFailed(let reason) = model.state else {
                return XCTFail("\(result) should be .checkFailed, got \(model.state)")
            }
            XCTAssertTrue(reason.contains(fragment), "\(result) -> \(reason)")
        }
    }

    func testCheckKeepsSpinningUntilUpdateAvailableFollows() throws {
        var model = UpdateModel()
        model.beginCheck()
        model.apply(.checkDone(result: "update_available", current: "0.1.0", latest: "0.2.0"))
        XCTAssertEqual(model.state, .checking)
        model.apply(try event(availableJSON))
        guard case .available(let offer) = model.state else { return XCTFail("expected .available") }
        XCTAssertEqual(offer.latest, "0.2.0")
    }

    func testRecheckKeepsKnownOfferOnScreen() {
        var model = offered()
        model.beginCheck()
        guard case .available = model.state else { return XCTFail("offer must stay visible") }
        model.apply(.checkDone(result: "offline", current: "0.1.0", latest: ""))
        guard case .available = model.state else { return XCTFail("failed re-check must not hide the update") }
        model.apply(.checkDone(result: "up_to_date", current: "0.2.0", latest: "0.2.0"))
        XCTAssertEqual(model.state, .upToDate(version: "0.2.0"))
        XCTAssertNil(model.offer)
    }

    func testCheckTimeoutDoesNotLeaveSpinner() {
        var model = UpdateModel()
        model.beginCheck()
        model.checkTimedOut()
        guard case .checkFailed = model.state else { return XCTFail("expected .checkFailed") }
    }

    // MARK: Install

    func testInstallFlowEndsInExpectedHandoff() {
        var model = offered()
        XCTAssertTrue(model.beginInstall())
        XCTAssertEqual(model.state, .installing(.downloading(percent: nil)))
        model.apply(.progress(phase: "downloading", percent: 40))
        XCTAssertEqual(model.state, .installing(.downloading(percent: 40)))
        model.apply(.progress(phase: "verifying", percent: nil))
        XCTAssertEqual(model.state, .installing(.verifying))
        model.apply(.progress(phase: "installing", percent: nil))
        XCTAssertEqual(model.state, .installing(.handoff))

        // The daemon quitting after "installing" is expected, not a failure.
        model.daemonDisconnected()
        XCTAssertEqual(model.state, .installing(.handoff))
    }

    func testInstallRefusedWhenDaemonSaidItCannotApply() {
        var model = offered(canApply: false)
        XCTAssertFalse(model.beginInstall())
        guard case .available = model.state else { return XCTFail("state must not change") }
    }

    func testDisconnectMidDownloadFails() {
        var model = offered()
        _ = model.beginInstall()
        model.apply(.progress(phase: "downloading", percent: 10))
        model.daemonDisconnected()
        guard case .failed(let stage, _) = model.state else { return XCTFail("expected .failed") }
        XCTAssertEqual(stage, "install")
    }

    func testFailureKeepsOfferSoUserCanRetry() {
        var model = offered()
        _ = model.beginInstall()
        model.apply(.failed(stage: "verify", message: "signature did not verify"))
        XCTAssertEqual(model.state, .failed(stage: "verify", message: "signature did not verify"))
        XCTAssertEqual(model.offer?.latest, "0.2.0")
        XCTAssertTrue(model.beginInstall())
    }

    func testReannouncementDoesNotInterruptALiveDownload() throws {
        var model = offered()
        _ = model.beginInstall()
        model.apply(.progress(phase: "downloading", percent: 5))
        model.apply(try event(availableJSON))
        XCTAssertEqual(model.state, .installing(.downloading(percent: 5)))
    }

    func testReannouncementAfterHandoffMeansInstallerDidNotTakeOver() throws {
        var model = offered()
        _ = model.beginInstall()
        model.apply(.progress(phase: "installing", percent: nil))
        model.apply(try event(availableJSON))
        guard case .available = model.state else { return XCTFail("expected .available") }
    }

    func testHandoffTimeoutFailsWithLogPointer() {
        var model = offered()
        _ = model.beginInstall()
        model.apply(.progress(phase: "installing", percent: nil))
        model.handoffTimedOut()
        guard case .failed(_, let message) = model.state else { return XCTFail("expected .failed") }
        XCTAssertTrue(message.contains("update-install.log"))
    }
}
