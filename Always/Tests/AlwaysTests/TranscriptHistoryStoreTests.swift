import XCTest
@testable import Always

/// Settings → History on a fresh install (Slack #showcase, 16 Oct: "l'historique
/// est vide chez moi ! même après un refresh"). Each test uses its own temp
/// HOME layout, nothing pre-created.
final class TranscriptHistoryStoreTests: XCTestCase {
    private var home: URL!
    private var store: TranscriptHistoryStore!

    override func setUpWithError() throws {
        home = FileManager.default.temporaryDirectory
            .appendingPathComponent("always-history-home-\(UUID().uuidString)", isDirectory: true)
        store = TranscriptHistoryStore(
            supportDir: home.appendingPathComponent("Library/Application Support/always", isDirectory: true),
            logDir: home.appendingPathComponent("Library/Logs/Always", isDirectory: true))
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: home)
    }

    private func write(_ text: String, to url: URL) throws {
        try FileManager.default.createDirectory(
            at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try text.write(to: url, atomically: true, encoding: .utf8)
    }

    private func ms(_ date: Date) -> Int { Int(date.timeIntervalSince1970 * 1000) }

    /// The bug: a release daemon logs `transcription_pasted` WITHOUT text
    /// (privacy), so the old log-scraping History found nothing.
    func testReleaseLogWithoutTextYieldsNothingFromLog() throws {
        let now = Date()
        try write(
            #"{"timestamp":"2026-10-16T10:00:00.123456Z","level":"INFO","fields":{"message":"transcription_pasted","chars":21,"energy":0.1,"processed_chars":22}}"#,
            to: store.todayLogURL(now: now))
        XCTAssertEqual(store.todayEntries(now: now), [])
    }

    func testFreshInstallWithNothingWrittenIsEmpty() {
        XCTAssertEqual(store.todayEntries(), [])
    }

    func testReadsDaemonHistoryFile() throws {
        let now = Date()
        let yesterday = now.addingTimeInterval(-86_400 * 2)
        try write(
            """
            {"ts":\(ms(yesterday)),"raw_text":"old","processed_text":"Old."}
            {"ts":\(ms(now)),"raw_text":"bonjour tout le monde","processed_text":"Bonjour tout le monde."}
            not json
            {"ts":\(ms(now)),"raw_text":"sans traitement","processed_text":""}

            """, to: store.historyURL)

        let entries = store.todayEntries(now: now)
        XCTAssertEqual(entries.map(\.displayText), ["Bonjour tout le monde.", "sans traitement"])
    }

    func testKeepsOnlyLastTwentyOfToday() throws {
        let now = Date()
        let lines = (0..<25).map { #"{"ts":\#(ms(now)),"raw_text":"u\#($0)","processed_text":""}"# }
        try write(lines.joined(separator: "\n") + "\n", to: store.historyURL)
        let entries = store.todayEntries(now: now)
        XCTAssertEqual(entries.count, 20)
        XCTAssertEqual(entries.first?.rawText, "u5")
        XCTAssertEqual(entries.last?.rawText, "u24")
    }

    func testFallsBackToLegacyLogWhenNoHistoryFile() throws {
        let now = Date()
        let iso = ISO8601DateFormatter()
        try write(
            #"{"timestamp":"\#(iso.string(from: now))","fields":{"message":"transcription_pasted","raw_text":"dev raw","processed_text":"Dev processed."}}"#,
            to: store.todayLogURL(now: now))
        XCTAssertEqual(store.todayEntries(now: now).map(\.displayText), ["Dev processed."])
    }

    func testClearEmptiesHistoryAndStopsLogFallback() throws {
        let now = Date()
        try write(#"{"ts":\#(ms(now)),"raw_text":"a","processed_text":"A."}"# + "\n", to: store.historyURL)
        try write(
            #"{"timestamp":"\#(ISO8601DateFormatter().string(from: now))","fields":{"message":"transcription_pasted","raw_text":"b"}}"#,
            to: store.todayLogURL(now: now))
        store.clear(now: now)
        XCTAssertEqual(store.todayEntries(now: now), [])
    }
}
