import SwiftUI
import AppKit

/// One voice snippet: saying `trigger` as a whole utterance pastes
/// `expansion` verbatim. Mirrors the daemon's `snippets.rs` entry
/// shape — only `trigger` and `expansion` are serialized.
struct VoiceSnippet: Identifiable, Codable, Equatable {
    var id = UUID()
    var trigger: String
    var expansion: String

    enum CodingKeys: String, CodingKey {
        case trigger, expansion
    }
}

/// Loads and saves `~/.always/snippets.json`. The daemon re-reads the
/// file on every paste, so a save here is live for the very next
/// utterance — no restart, no UDS round-trip.
final class SnippetsStore: ObservableObject {
    @Published var snippets: [VoiceSnippet] = []
    @Published var loadError: String?

    static var fileURL: URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent(".always")
            .appendingPathComponent("snippets.json")
    }

    func load() {
        loadError = nil
        let url = Self.fileURL
        guard FileManager.default.fileExists(atPath: url.path) else {
            snippets = []
            return
        }
        do {
            let data = try Data(contentsOf: url)
            snippets = try JSONDecoder().decode([VoiceSnippet].self, from: data)
        } catch {
            // Never clobber a hand-edited file we failed to parse —
            // surface the error and leave saving disabled until the
            // user fixes or reloads.
            loadError = "snippets.json could not be read: \(error.localizedDescription)"
            snippets = []
        }
    }

    func save() {
        guard loadError == nil else { return }
        let url = Self.fileURL
        do {
            try FileManager.default.createDirectory(
                at: url.deletingLastPathComponent(),
                withIntermediateDirectories: true
            )
            let encoder = JSONEncoder()
            encoder.outputFormatting = [.prettyPrinted, .withoutEscapingSlashes]
            let data = try encoder.encode(snippets)
            try data.write(to: url, options: .atomic)
        } catch {
            loadError = "Failed to save snippets.json: \(error.localizedDescription)"
        }
    }
}

struct SnippetsPanel: View {
    @StateObject private var store = SnippetsStore()
    /// Snippet whose delete button was pressed; drives the confirm dialog.
    @State private var pendingDelete: VoiceSnippet?

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                header
                if let error = store.loadError {
                    errorBanner(error)
                }
                ForEach($store.snippets) { $snippet in
                    snippetCard($snippet)
                }
                addButton
                Divider()
                fileFooter
            }
            .padding(20)
        }
        .onAppear { store.load() }
        .confirmationDialog(
            "Delete snippet \"\(pendingDelete?.trigger ?? "")\"?",
            isPresented: Binding(
                get: { pendingDelete != nil },
                set: { if !$0 { pendingDelete = nil } }
            )
        ) {
            Button("Delete", role: .destructive) {
                if let snippet = pendingDelete {
                    store.snippets.removeAll { $0.id == snippet.id }
                    store.save()
                }
                pendingDelete = nil
            }
            Button("Cancel", role: .cancel) { pendingDelete = nil }
        }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text("Voice Snippets").font(.headline)
                Spacer()
                if !store.snippets.isEmpty {
                    Text("\(store.snippets.count) snippet\(store.snippets.count == 1 ? "" : "s")")
                        .font(.caption)
                        .foregroundColor(.secondary)
                }
            }
            Text("Say a trigger phrase — alone or anywhere in a sentence — and Always replaces it with the expansion: exact text, no AI rewriting. Matching ignores case and punctuation. Changes apply to the very next thing you say.")
                .font(.caption)
                .foregroundColor(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    private func errorBanner(_ message: String) -> some View {
        HStack(spacing: 8) {
            Image(systemName: "exclamationmark.triangle.fill")
                .foregroundColor(.orange)
            Text(message)
                .font(.caption)
                .fixedSize(horizontal: false, vertical: true)
            Spacer()
            Button("Reload") { store.load() }
                .controlSize(.small)
        }
        .padding(10)
        .background(Color.orange.opacity(0.1))
        .cornerRadius(6)
    }

    private func snippetCard(_ snippet: Binding<VoiceSnippet>) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                Image(systemName: "quote.bubble")
                    .foregroundColor(.accentColor)
                TextField("Trigger phrase — e.g. grill me", text: snippet.trigger)
                    .textFieldStyle(.roundedBorder)
                    .font(.body)
                Button {
                    pendingDelete = snippet.wrappedValue
                } label: {
                    Image(systemName: "trash")
                }
                .buttonStyle(.borderless)
                .help("Delete this snippet")
            }
            TextEditor(text: snippet.expansion)
                .font(.system(.body, design: .monospaced))
                .frame(minHeight: 72, maxHeight: 180)
                .scrollContentBackground(.hidden)
                .padding(6)
                .background(Color(NSColor.textBackgroundColor))
                .overlay(
                    RoundedRectangle(cornerRadius: 6)
                        .stroke(Color.secondary.opacity(0.25), lineWidth: 1)
                )
            if snippet.wrappedValue.expansion.isEmpty {
                Text("Expansion is empty — saying the trigger will paste nothing.")
                    .font(.caption2)
                    .foregroundColor(.orange)
            }
        }
        .padding(12)
        .background(Color.secondary.opacity(0.06))
        .cornerRadius(8)
        .onChange(of: snippet.wrappedValue) { _, _ in
            store.save()
        }
    }

    private var addButton: some View {
        Button {
            store.snippets.append(VoiceSnippet(trigger: "", expansion: ""))
            store.save()
        } label: {
            Label("Add Snippet", systemImage: "plus.circle.fill")
        }
        .buttonStyle(.bordered)
        .disabled(store.loadError != nil)
    }

    private var fileFooter: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                Button {
                    NSWorkspace.shared.activateFileViewerSelecting([SnippetsStore.fileURL])
                } label: {
                    Label("Show in Finder", systemImage: "folder")
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                Button {
                    store.load()
                } label: {
                    Image(systemName: "arrow.clockwise")
                }
                .buttonStyle(.borderless)
                .help("Reload from disk (picks up hand-edits)")
                Spacer()
            }
            Text(SnippetsStore.fileURL.path)
                .font(.system(.caption2, design: .monospaced))
                .foregroundColor(.secondary)
                .lineLimit(1)
                .truncationMode(.middle)
        }
    }
}
