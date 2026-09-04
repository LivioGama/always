import SwiftUI
import AppKit

/// One glossary entry. Mirrors the daemon's `glossary.rs` `Entry` shape —
/// only `term`, `mistranscriptions`, `frequency`, and `weight` are
/// serialized. `frequency` defaults to 0 and `weight` to 1 when absent.
struct GlossaryEntry: Identifiable, Codable, Equatable {
    var id = UUID()
    var term: String
    var mistranscriptions: [String] = []
    var frequency: Int = 0
    var weight: Int = 1

    enum CodingKeys: String, CodingKey {
        case term, mistranscriptions, frequency, weight
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        term = try c.decode(String.self, forKey: .term)
        mistranscriptions = try c.decodeIfPresent([String].self, forKey: .mistranscriptions) ?? []
        frequency = try c.decodeIfPresent(Int.self, forKey: .frequency) ?? 0
        weight = try c.decodeIfPresent(Int.self, forKey: .weight) ?? 1
    }

    /// Encode back to the daemon's shape (no `id` on disk).
    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(term, forKey: .term)
        try c.encode(mistranscriptions, forKey: .mistranscriptions)
        try c.encode(frequency, forKey: .frequency)
        try c.encode(weight, forKey: .weight)
    }
}

struct VocabularyPanel: View {
    @State private var entries: [GlossaryEntry] = []
    @State private var loadError: String?
    @State private var pendingDelete: GlossaryEntry?

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                header
                toolbar
                if let error = loadError {
                    errorBanner(error)
                }
                if entries.isEmpty && loadError == nil {
                    emptyState
                }
                ForEach(entries) { entry in
                    entryCard(entry)
                }
            }
            .padding(20)
        }
        .onAppear { refresh() }
        .confirmationDialog(
            "Delete \"\(pendingDelete?.term ?? "")\"?",
            isPresented: Binding(
                get: { pendingDelete != nil },
                set: { if !$0 { pendingDelete = nil } }
            )
        ) {
            Button("Delete", role: .destructive) {
                if let entry = pendingDelete {
                    entries.removeAll { $0.id == entry.id }
                    save()
                }
                pendingDelete = nil
            }
            Button("Cancel", role: .cancel) { pendingDelete = nil }
        }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text("Vocabulary").font(.headline)
                Spacer()
                if !entries.isEmpty {
                    Text("\(entries.count) term\(entries.count == 1 ? "" : "s")")
                        .font(.caption)
                        .foregroundColor(.secondary)
                }
            }
            Text("Glossary terms bias transcription toward your preferred spellings and substitutions. Delete entries here or edit `glossary.json` directly to add them.")
                .font(.caption)
                .foregroundColor(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    // MARK: - Toolbar (buttons at top)

    private var toolbar: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                Button(action: openVocabularyFile) {
                    Label("Open glossary.json", systemImage: "doc.text")
                }
                .buttonStyle(.bordered)
                .controlSize(.small)

                Button(action: revealVocabularyInFinder) {
                    Label("Show in Finder", systemImage: "folder")
                }
                .buttonStyle(.bordered)
                .controlSize(.small)

                Button(action: refresh) {
                    Image(systemName: "arrow.clockwise")
                }
                .buttonStyle(.borderless)
                .help("Reload from disk (picks up hand-edits)")
                Spacer()
            }
            Text(userGlossaryURL().path)
                .font(.system(.caption2, design: .monospaced))
                .foregroundColor(.secondary)
                .lineLimit(1)
                .truncationMode(.middle)
        }
    }

    private var emptyState: some View {
        HStack(spacing: 8) {
            Image(systemName: "character.book.closed")
                .foregroundColor(.secondary)
            Text("No glossary entries yet. Open glossary.json to add terms.")
                .font(.caption)
                .foregroundColor(.secondary)
            Spacer()
        }
        .padding(12)
        .background(Color.secondary.opacity(0.06))
        .cornerRadius(8)
    }

    private func errorBanner(_ message: String) -> some View {
        HStack(spacing: 8) {
            Image(systemName: "exclamationmark.triangle.fill")
                .foregroundColor(.orange)
            Text(message)
                .font(.caption)
                .fixedSize(horizontal: false, vertical: true)
            Spacer()
            Button("Reload") { refresh() }
                .controlSize(.small)
        }
        .padding(10)
        .background(Color.orange.opacity(0.1))
        .cornerRadius(6)
    }

    private func entryCard(_ entry: GlossaryEntry) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                Image(systemName: "text.quote")
                    .foregroundColor(.accentColor)
                Text(entry.term)
                    .font(.body.weight(.medium))
                Spacer()
                if entry.weight > 1 {
                    badge("\(entry.weight)×", color: .accentColor)
                }
                if entry.frequency > 0 {
                    badge("\(entry.frequency) freq", color: .secondary)
                }
                Button {
                    pendingDelete = entry
                } label: {
                    Image(systemName: "trash")
                }
                .buttonStyle(.borderless)
                .help("Delete this entry")
            }
            if !entry.mistranscriptions.isEmpty {
                HStack(spacing: 6) {
                    Image(systemName: "arrow.triangle.branch")
                        .font(.caption2)
                        .foregroundColor(.secondary)
                    Text(entry.mistranscriptions.joined(separator: ", "))
                        .font(.caption)
                        .foregroundColor(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        }
        .padding(12)
        .background(Color.secondary.opacity(0.06))
        .cornerRadius(8)
    }

    private func badge(_ text: String, color: Color) -> some View {
        Text(text)
            .font(.caption2.weight(.medium))
            .foregroundColor(color)
            .padding(.horizontal, 6)
            .padding(.vertical, 2)
            .background(color.opacity(0.12))
            .cornerRadius(4)
    }

    // MARK: - Load / save / file helpers

    private func userGlossaryURL() -> URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent(".always")
            .appendingPathComponent("glossary.json")
    }

    private func ensureGlossaryExists(at url: URL) {
        let dir = url.deletingLastPathComponent()
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        if !FileManager.default.fileExists(atPath: url.path) {
            try? "[]".write(to: url, atomically: true, encoding: .utf8)
        }
    }

    private func openVocabularyFile() {
        let url = userGlossaryURL()
        ensureGlossaryExists(at: url)
        NSWorkspace.shared.open(url)
    }

    private func revealVocabularyInFinder() {
        let url = userGlossaryURL()
        ensureGlossaryExists(at: url)
        NSWorkspace.shared.activateFileViewerSelecting([url])
    }

    private func refresh() {
        let url = userGlossaryURL()
        ensureGlossaryExists(at: url)
        loadError = nil
        do {
            let data = try Data(contentsOf: url)
            entries = try JSONDecoder().decode([GlossaryEntry].self, from: data)
        } catch {
            loadError = "glossary.json could not be read: \(error.localizedDescription)"
            entries = []
        }
    }

    /// Write entries back to glossary.json. The daemon re-reads the file
    /// on every transcription, so a save here is live for the next
    /// utterance — no restart needed.
    private func save() {
        guard loadError == nil else { return }
        let url = userGlossaryURL()
        do {
            let encoder = JSONEncoder()
            encoder.outputFormatting = [.prettyPrinted, .withoutEscapingSlashes]
            let data = try encoder.encode(entries)
            try data.write(to: url, options: .atomic)
        } catch {
            loadError = "Failed to save glossary.json: \(error.localizedDescription)"
        }
    }
}
