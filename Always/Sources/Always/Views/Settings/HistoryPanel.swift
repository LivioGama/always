import SwiftUI
import AppKit

private struct TranscriptEntry: Identifiable {
    let id = UUID()
    let timestamp: Date
    let rawText: String
    let processedText: String

    var displayText: String { processedText.isEmpty ? rawText : processedText }
}

struct HistoryPanel: View {
    @State private var entries: [TranscriptEntry] = []
    @State private var isLoading = false
    @State private var showDeleteConfirmation = false

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    Text("Transcript History")
                        .font(.headline)
                    Text("Last 20 transcriptions from today's session.")
                        .font(.caption)
                        .foregroundColor(.secondary)
                }
                Spacer()
                Button("Refresh") { Task { await loadHistory() } }
                    .controlSize(.small)
                if !entries.isEmpty {
                    Button {
                        exportAll()
                    } label: {
                        Label("Export All", systemImage: "square.and.arrow.up")
                    }
                    .controlSize(.small)
                    Button(role: .destructive) {
                        showDeleteConfirmation = true
                    } label: {
                        Label("Delete All", systemImage: "trash")
                    }
                    .controlSize(.small)
                }
            }

            if isLoading {
                HStack(spacing: 8) {
                    ProgressView().controlSize(.small)
                    Text("Loading…").font(.caption).foregroundColor(.secondary)
                }
                .padding(.top, 8)
            } else if entries.isEmpty {
                Text("No transcriptions yet today.")
                    .font(.caption)
                    .foregroundColor(.secondary)
                    .padding(.top, 8)
            } else {
                ScrollView {
                    LazyVStack(spacing: 6) {
                        ForEach(entries.reversed()) { entry in
                            HistoryEntryRow(entry: entry)
                        }
                    }
                }
            }
        }
        .padding(20)
        .task { await loadHistory() }
        .confirmationDialog(
            "Delete all transcriptions from today's session?",
            isPresented: $showDeleteConfirmation,
            titleVisibility: .visible
        ) {
            Button("Delete All", role: .destructive) {
                Task { await deleteAllHistory() }
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("This permanently removes today's transcript history. This cannot be undone.")
        }
    }

    private func loadHistory() async {
        isLoading = true
        defer { isLoading = false }
        entries = await Task.detached(priority: .utility) {
            readTodayEntries()
        }.value
    }

    private func deleteAllHistory() async {
        await Task.detached(priority: .utility) {
            clearTodayLog()
        }.value
        await loadHistory()
    }

    private func exportAll() {
        let panel = NSSavePanel()
        panel.title = "Export Transcript History"
        panel.nameFieldStringValue = "always-transcripts-\(Self.exportDateFormatter.string(from: Date()))"
        panel.allowedContentTypes = [.plainText]
        guard panel.runModal() == .OK, let url = panel.url else { return }

        // Build plain-text export: one block per entry, separated by rules.
        let iso = ISO8601DateFormatter()
        let blocks = entries.map { entry in
            """
            [\(iso.string(from: entry.timestamp))]
            \(entry.displayText)
            """
        }
        let separator = String(repeating: "—", count: 40)
        let text = blocks.joined(separator: "\n\n\(separator)\n\n")
        try? text.write(to: url, atomically: true, encoding: .utf8)
    }

    private static let exportDateFormatter: DateFormatter = {
        let f = DateFormatter()
        f.dateFormat = "yyyy-MM-dd"
        return f
    }()
}

private struct HistoryEntryRow: View {
    let entry: TranscriptEntry
    @State private var copied = false

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            VStack(alignment: .leading, spacing: 3) {
                Text(entry.displayText)
                    .font(.callout)
                    .fixedSize(horizontal: false, vertical: true)
                Text(entry.timestamp, style: .time)
                    .font(.caption2)
                    .foregroundColor(.secondary)
            }
            Spacer()
            Button {
                NSPasteboard.general.clearContents()
                NSPasteboard.general.setString(entry.displayText, forType: .string)
                copied = true
                DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) { copied = false }
            } label: {
                Image(systemName: copied ? "checkmark" : "doc.on.doc")
                    .font(.caption)
            }
            .buttonStyle(.plain)
            .foregroundColor(copied ? .green : .secondary)
            .help("Copy to clipboard")
        }
        .padding(10)
        .background(Color.secondary.opacity(0.06))
        .cornerRadius(6)
    }
}

private func readTodayEntries() -> [TranscriptEntry] {
    let dateStr = {
        let f = DateFormatter()
        f.dateFormat = "yyyy-MM-dd"
        return f.string(from: Date())
    }()
    let logPath = FileManager.default.homeDirectoryForCurrentUser
        .appendingPathComponent("Library/Logs/Always/always.\(dateStr)")
        .path
    guard let contents = try? String(contentsOfFile: logPath, encoding: .utf8) else { return [] }

    let iso = ISO8601DateFormatter()
    var result: [TranscriptEntry] = []
    for line in contents.components(separatedBy: "\n") where !line.isEmpty {
        guard
            let data = line.data(using: .utf8),
            let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
            let fields = json["fields"] as? [String: Any],
            let msg = fields["message"] as? String, msg == "transcription_pasted",
            let rawText = fields["raw_text"] as? String
        else { continue }
        let processedText = fields["processed_text"] as? String ?? ""
        let ts = (json["timestamp"] as? String).flatMap { iso.date(from: $0) } ?? Date()
        result.append(TranscriptEntry(timestamp: ts, rawText: rawText, processedText: processedText))
    }
    return Array(result.suffix(20))
}

private func clearTodayLog() {
    let dateStr = {
        let f = DateFormatter()
        f.dateFormat = "yyyy-MM-dd"
        return f.string(from: Date())
    }()
    let logURL = FileManager.default.homeDirectoryForCurrentUser
        .appendingPathComponent("Library/Logs/Always/always.\(dateStr)")
    // Truncate the log file. The daemon holds an open file handle and will
    // continue appending new entries at the new (zero) offset.
    try? Data().write(to: logURL, options: .atomic)
}
