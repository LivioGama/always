import SwiftUI

struct PermissionsPanel: View {
    @ObservedObject private var permissions = PermissionsManager.shared

    private var allGranted: Bool {
        permissions.micStatus.isOK && permissions.accessibilityStatus.isOK
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                header
                permissionRows
                rebuildNote
            }
            .padding(20)
        }
        .onAppear {
            permissions.refresh()
        }
    }

    private var header: some View {
        HStack(alignment: .center, spacing: 12) {
            Image(systemName: allGranted ? "checkmark.shield.fill" : "exclamationmark.shield.fill")
                .font(.system(size: 28))
                .foregroundColor(allGranted ? .green : .orange)

            VStack(alignment: .leading, spacing: 3) {
                Text("Permissions")
                    .font(.title3.bold())
                Text(allGranted ? "Always has the macOS access it needs." : "Some macOS access still needs attention.")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }

            Spacer()

            Button {
                permissions.refresh()
            } label: {
                Label("Re-check", systemImage: "arrow.clockwise")
            }
            .controlSize(.small)
        }
    }

    private var permissionRows: some View {
        VStack(alignment: .leading, spacing: 10) {
            permissionRow(
                icon: "mic.fill",
                title: "Microphone",
                status: micStatusText,
                detail: "Records your voice for dictation.",
                granted: permissions.micStatus.isOK,
                actionTitle: micActionTitle,
                action: handleMicAction
            )

            permissionRow(
                icon: "cursorarrow.click.2",
                title: "Accessibility",
                status: permissions.accessibilityStatus.isOK ? "Granted" : "Not granted",
                detail: "Pastes transcripts and lets Always track the active input context.",
                granted: permissions.accessibilityStatus.isOK,
                actionTitle: permissions.accessibilityStatus.isOK ? nil : "Open Settings",
                action: handleAccessibilityAction
            )
        }
    }

    private var rebuildNote: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Development builds")
                .font(.subheadline.bold())
            Text("Debug rebuilds use a stable Apple Development signature when available so macOS keeps Always permissions across reinstalls.")
                .font(.caption)
                .foregroundColor(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(10)
        .background(Color.secondary.opacity(0.08))
        .cornerRadius(6)
    }

    private func permissionRow(
        icon: String,
        title: String,
        status: String,
        detail: String,
        granted: Bool,
        actionTitle: String?,
        action: @escaping () -> Void
    ) -> some View {
        HStack(alignment: .top, spacing: 12) {
            Image(systemName: granted ? "checkmark.circle.fill" : "exclamationmark.circle.fill")
                .font(.title3)
                .foregroundColor(granted ? .green : .orange)
                .frame(width: 24)

            VStack(alignment: .leading, spacing: 4) {
                HStack(spacing: 8) {
                    Image(systemName: icon)
                        .foregroundColor(.secondary)
                    Text(title)
                        .font(.headline)
                    Text(status)
                        .font(.caption.bold())
                        .foregroundColor(granted ? .green : .orange)
                }
                Text(detail)
                    .font(.caption)
                    .foregroundColor(.secondary)
            }

            Spacer(minLength: 12)

            if let actionTitle {
                Button(actionTitle, action: action)
                    .controlSize(.small)
            }
        }
        .padding(10)
        .background(Color.secondary.opacity(0.08))
        .cornerRadius(6)
    }

    private var micStatusText: String {
        switch permissions.micStatus {
        case .granted:
            return "Granted"
        case .notDetermined:
            return "Not requested"
        case .denied:
            return "Denied"
        case .restricted:
            return "Restricted"
        }
    }

    private var micActionTitle: String? {
        switch permissions.micStatus {
        case .granted:
            return nil
        case .notDetermined:
            return "Request"
        case .denied, .restricted:
            return "Open Settings"
        }
    }

    private func handleMicAction() {
        if permissions.micStatus == .notDetermined {
            permissions.requestMicrophoneIfNeeded()
        } else {
            permissions.openSystemSettings(for: .microphone)
        }
    }

    private func handleAccessibilityAction() {
        _ = permissions.requestAccessibilityIfNeeded()
        if !permissions.accessibilityStatus.isOK {
            permissions.openSystemSettings(for: .accessibility)
        }
    }
}
