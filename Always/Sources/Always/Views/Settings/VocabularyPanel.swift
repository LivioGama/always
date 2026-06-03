import SwiftUI
import AppKit

struct VocabularyPanel: View {
    @State private var vocabularyTermCount: Int = 0
    @State private var vocabularyPath: String = ""

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 10) {
                HStack {
                    Text("Vocabulary").font(.headline)
                    Spacer()
                    if vocabularyTermCount > 0 {
                        Text("\(vocabularyTermCount) terms")
                            .font(.caption)
                            .foregroundColor(.secondary)
                    }
                }
                Text("Glossary terms bias transcription toward your preferred spellings and substitutions. Edit `glossary.json` to add or remove entries.")
                    .font(.caption)
                    .foregroundColor(.secondary)
                    .fixedSize(horizontal: false, vertical: true)

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

                    Button(action: refreshVocabularyInfo) {
                        Image(systemName: "arrow.clockwise")
                    }
                    .buttonStyle(.borderless)
                    .help("Refresh vocabulary info")
                    Spacer()
                }
                if !vocabularyPath.isEmpty {
                    Text(vocabularyPath)
                        .font(.system(.caption2, design: .monospaced))
                        .foregroundColor(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
            }
            .padding(20)
        }
        .onAppear { refreshVocabularyInfo() }
    }

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

    private func refreshVocabularyInfo() {
        let url = userGlossaryURL()
        vocabularyPath = url.path

        if FileManager.default.fileExists(atPath: url.path),
           let data = try? Data(contentsOf: url),
           let json = try? JSONSerialization.jsonObject(with: data) as? [[String: Any]] {
            vocabularyTermCount = json.count
        } else {
            vocabularyTermCount = 0
        }
    }
}
