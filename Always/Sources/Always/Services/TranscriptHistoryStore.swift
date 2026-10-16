import Foundation

/// One pasted utterance shown in Settings → History.
struct TranscriptEntry: Identifiable, Equatable {
    let id = UUID()
    let timestamp: Date
    let rawText: String
    let processedText: String

    var displayText: String { processedText.isEmpty ? rawText : processedText }

    static func == (a: TranscriptEntry, b: TranscriptEntry) -> Bool {
        a.timestamp == b.timestamp && a.rawText == b.rawText && a.processedText == b.processedText
    }
}

/// Reads the daemon's transcript history (`src/always/history.rs`):
/// `<supportDir>/history.jsonl`, one `{ts, raw_text, processed_text}` line
/// per pasted utterance.
///
/// Before the dedicated store existed, History parsed `transcription_pasted`
/// out of the daily tracing log — but release builds never log transcript
/// text, so a real install always showed an empty History. The log is still
/// read as a fallback when no history file exists yet (an older daemon, or a
/// dev build that logs transcripts).
struct TranscriptHistoryStore {
    let supportDir: URL
    let logDir: URL

    static var current: TranscriptHistoryStore {
        TranscriptHistoryStore(
            supportDir: AppInstance.supportDir,
            logDir: FileManager.default.homeDirectoryForCurrentUser
                .appendingPathComponent("Library/Logs/Always\(AppInstance.suffix)", isDirectory: true)
        )
    }

    static let fileName = "history.jsonl"

    var historyURL: URL { supportDir.appendingPathComponent(Self.fileName) }

    func todayLogURL(now: Date = Date()) -> URL {
        logDir.appendingPathComponent("always.\(Self.dayString(now))")
    }

    /// The last `limit` entries pasted on `now`'s calendar day, oldest first.
    func todayEntries(now: Date = Date(), limit: Int = 20, calendar: Calendar = .current) -> [TranscriptEntry] {
        let all: [TranscriptEntry]
        if FileManager.default.fileExists(atPath: historyURL.path) {
            all = Self.parseHistory(at: historyURL)
        } else {
            all = Self.parseLegacyLog(at: todayLogURL(now: now))
        }
        let today = all.filter { calendar.isDate($0.timestamp, inSameDayAs: now) }
        return Array(today.suffix(limit))
    }

    /// Empties the history. Also truncates today's log, as before, so
    /// transcript text a dev build logged does not reappear via the fallback.
    func clear(now: Date = Date()) {
        try? FileManager.default.createDirectory(at: supportDir, withIntermediateDirectories: true)
        try? Data().write(to: historyURL, options: .atomic)
        let log = todayLogURL(now: now)
        if FileManager.default.fileExists(atPath: log.path) {
            try? Data().write(to: log, options: .atomic)
        }
    }

    static func parseHistory(at url: URL) -> [TranscriptEntry] {
        guard let contents = try? String(contentsOf: url, encoding: .utf8) else { return [] }
        var result: [TranscriptEntry] = []
        for line in contents.split(separator: "\n") where !line.isEmpty {
            guard
                let json = try? JSONSerialization.jsonObject(with: Data(line.utf8)) as? [String: Any],
                let ts = (json["ts"] as? NSNumber)?.doubleValue
            else { continue }
            let raw = json["raw_text"] as? String ?? ""
            let processed = json["processed_text"] as? String ?? ""
            if raw.isEmpty && processed.isEmpty { continue }
            result.append(TranscriptEntry(
                timestamp: Date(timeIntervalSince1970: ts / 1000),
                rawText: raw, processedText: processed))
        }
        return result
    }

    static func parseLegacyLog(at url: URL) -> [TranscriptEntry] {
        guard let contents = try? String(contentsOf: url, encoding: .utf8) else { return [] }
        let iso = ISO8601DateFormatter()
        iso.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        let isoPlain = ISO8601DateFormatter()
        var result: [TranscriptEntry] = []
        for line in contents.split(separator: "\n") where !line.isEmpty {
            guard
                let json = try? JSONSerialization.jsonObject(with: Data(line.utf8)) as? [String: Any],
                let fields = json["fields"] as? [String: Any],
                let msg = fields["message"] as? String, msg == "transcription_pasted",
                let rawText = fields["raw_text"] as? String
            else { continue }
            let processedText = fields["processed_text"] as? String ?? ""
            let stamp = json["timestamp"] as? String
            let ts = stamp.flatMap { iso.date(from: $0) ?? isoPlain.date(from: $0) } ?? Date()
            result.append(TranscriptEntry(timestamp: ts, rawText: rawText, processedText: processedText))
        }
        return result
    }

    static func dayString(_ date: Date) -> String {
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.dateFormat = "yyyy-MM-dd"
        return f.string(from: date)
    }
}
