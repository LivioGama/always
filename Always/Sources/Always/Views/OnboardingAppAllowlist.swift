import AppKit
import SwiftUI

/// Final onboarding step: choose the apps voice typing is ON in.
///
/// Always pastes only into apps on the user's allowlist; every other app
/// is paused by default (see `src/always/per_app.rs`). Nothing in setup
/// said so, and "You're all set. Start speaking" was followed by silence
/// in whatever app the user tried first — the most common first-run
/// "it doesn't work". This list makes the choice explicit, using the apps
/// already open, and points at where to change it later.
struct OnboardingAppAllowlist: View {
    @ObservedObject private var state: StateMonitor = .shared
    @State private var apps: [AppEntry] = []

    struct AppEntry: Identifiable {
        let bundleId: String
        let name: String
        let icon: NSImage?
        var id: String { bundleId }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Turn voice typing on in:")
                .font(.headline)
            if apps.isEmpty {
                Text("Open the app you want to dictate into, then use the menu bar: “Resume Always for …”.")
                    .font(.caption)
                    .foregroundColor(.secondary)
            } else {
                ForEach(apps) { app in
                    Toggle(isOn: binding(for: app.bundleId)) {
                        HStack(spacing: 8) {
                            if let icon = app.icon {
                                Image(nsImage: icon)
                                    .resizable()
                                    .frame(width: 18, height: 18)
                            }
                            Text(app.name)
                        }
                    }
                    .toggleStyle(.checkbox)
                    .accessibilityLabel(app.name)
                }
            }
            Text("Every other app stays paused, so your words never land somewhere you didn't choose. Add apps any time from the menu bar: “Resume Always for …”.")
                .font(.caption)
                .foregroundColor(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
        .frame(maxWidth: 440, alignment: .leading)
        .onAppear { apps = Self.runningApps() }
    }

    private func binding(for bundleId: String) -> Binding<Bool> {
        Binding(
            get: { state.resumedBundleIds.contains(bundleId) },
            set: { on in state.setAppPaused(bundleId: bundleId, paused: on ? false : nil) }
        )
    }

    /// Regular (Dock) apps currently running, minus Always itself and
    /// system apps nobody dictates into.
    private static func runningApps() -> [AppEntry] {
        let skip: Set<String> = [
            FocusedAppMonitor.ownBundleId,
            "com.apple.finder",
            "com.apple.systempreferences",
            "com.apple.ActivityMonitor",
        ]
        var seen = Set<String>()
        return NSWorkspace.shared.runningApplications
            .filter { $0.activationPolicy == .regular }
            .compactMap { app -> AppEntry? in
                guard let id = app.bundleIdentifier, !skip.contains(id), seen.insert(id).inserted
                else { return nil }
                return AppEntry(bundleId: id, name: app.localizedName ?? id, icon: app.icon)
            }
            .sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
            .prefix(8)
            .map { $0 }
    }
}
