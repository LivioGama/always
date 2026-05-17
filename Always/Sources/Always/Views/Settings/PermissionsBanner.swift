import SwiftUI

/// Non-modal banner that surfaces missing TCC permissions inline at the
/// top of the Settings window. Pattern mirrors Handy's
/// `AccessibilityPermissions.tsx` — a single card that:
///   * hides itself once both permissions are OK
///   * shows status badges and a per-permission "Open System Settings"
///     button so the user can fix things without leaving the app
///   * auto-refreshes via `PermissionsManager`'s focus + poll hooks
///
/// Kept deliberately compact: we don't want this to dominate the
/// Settings window when the user just wants to tweak energy thresholds.
struct PermissionsBanner: View {
    @ObservedObject private var perms = PermissionsManager.shared

    /// Hidden entirely once both permissions are granted. The user
    /// shouldn't see "Permissions: all good ✓" indefinitely after
    /// onboarding — they already know.
    private var isHidden: Bool {
        perms.micStatus.isOK && perms.accessibilityStatus.isOK
    }

    var body: some View {
        if isHidden {
            EmptyView()
        } else {
            VStack(alignment: .leading, spacing: 8) {
                HStack(spacing: 8) {
                    Image(systemName: "exclamationmark.shield.fill")
                        .foregroundColor(.orange)
                    Text("Permissions needed")
                        .font(.headline)
                        .foregroundColor(.orange)
                    Spacer()
                    Button {
                        perms.refresh()
                    } label: {
                        Image(systemName: "arrow.clockwise")
                    }
                    .buttonStyle(.borderless)
                    .help("Re-check permissions")
                }

                if !perms.micStatus.isOK {
                    permissionRow(
                        title: "Microphone",
                        detail: detailForMic(perms.micStatus),
                        action: .microphone,
                        actionTitle: perms.micStatus == .notDetermined
                            ? "Request"
                            : "Open System Settings"
                    )
                }

                if !perms.accessibilityStatus.isOK {
                    permissionRow(
                        title: "Accessibility",
                        detail: "Required to paste transcripts into the focused app and to position the voice indicator near your cursor.",
                        action: .accessibility,
                        actionTitle: "Open System Settings"
                    )
                }
            }
            .padding(10)
            .background(
                RoundedRectangle(cornerRadius: 8)
                    .fill(Color.orange.opacity(0.08))
            )
            .overlay(
                RoundedRectangle(cornerRadius: 8)
                    .stroke(Color.orange.opacity(0.35), lineWidth: 1)
            )
        }
    }

    private func permissionRow(
        title: String,
        detail: String,
        action: PermissionsManager.Permission,
        actionTitle: String
    ) -> some View {
        HStack(alignment: .top, spacing: 10) {
            VStack(alignment: .leading, spacing: 2) {
                Text(title)
                    .font(.subheadline.bold())
                Text(detail)
                    .font(.caption2)
                    .foregroundColor(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 8)
            Button {
                handleAction(action)
            } label: {
                Text(actionTitle)
                    .font(.caption)
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
        }
    }

    private func handleAction(_ permission: PermissionsManager.Permission) {
        switch permission {
        case .microphone:
            if perms.micStatus == .notDetermined {
                // Request the system prompt; refreshes status in the
                // PermissionsManager callback.
                perms.requestMicrophoneIfNeeded()
            } else {
                perms.openSystemSettings(for: .microphone)
            }
        case .accessibility:
            // Always-try-prompt first; if the system already showed it
            // once, this is a cheap no-op and we just deep-link.
            _ = perms.requestAccessibilityIfNeeded()
            if !perms.accessibilityStatus.isOK {
                perms.openSystemSettings(for: .accessibility)
            }
        }
    }

    private func detailForMic(_ status: PermissionsManager.MicStatus) -> String {
        switch status {
        case .denied:
            return "Always cannot capture voice. Open System Settings → Privacy & Security → Microphone and allow Always."
        case .restricted:
            return "Restricted by an MDM profile or parental controls. Voice capture will not work until this is changed."
        case .notDetermined:
            return "Always needs microphone access to detect when you speak. Click Request to show the system prompt."
        case .granted:
            return ""
        }
    }
}
