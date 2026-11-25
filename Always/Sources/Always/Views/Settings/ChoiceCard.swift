import SwiftUI

/// One selectable option on the Models tab: a bordered card with a title,
/// a one-line description, an "Active" badge when selected and a "Use"
/// button otherwise. Transcription engines and Postprocess modes both use
/// it, so the two choices look and behave the same (owner request
/// 2026-11-25).
struct ChoiceCard<Footer: View>: View {
    let title: String
    let detail: String
    let isActive: Bool
    var isLoading = false
    var isDisabled = false
    var warning: String?
    let onUse: () -> Void
    @ViewBuilder var footer: () -> Footer

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .top, spacing: 12) {
                VStack(alignment: .leading, spacing: 4) {
                    HStack(spacing: 6) {
                        Text(title)
                            .font(.subheadline)
                            .fontWeight(.semibold)
                        if isActive {
                            Text("Active")
                                .font(.caption2)
                                .padding(.horizontal, 6)
                                .padding(.vertical, 2)
                                .background(
                                    RoundedRectangle(cornerRadius: 4)
                                        .fill(Color.accentColor.opacity(0.25))
                                )
                        }
                    }
                    Text(detail)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                    if let warning {
                        Label(warning, systemImage: "exclamationmark.triangle.fill")
                            .font(.caption)
                            .foregroundStyle(.orange)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
                Spacer()
                if isActive {
                    Text("Active")
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                } else {
                    Button(action: onUse) {
                        HStack(spacing: 4) {
                            if isLoading {
                                ProgressView()
                                    .controlSize(.small)
                            }
                            Text(isLoading ? "Loading…" : "Use")
                                .font(.caption)
                        }
                    }
                    .controlSize(.small)
                    .disabled(isDisabled)
                }
            }
            footer()
        }
        .padding(10)
        .background(
            RoundedRectangle(cornerRadius: 8)
                .stroke(isActive ? Color.accentColor.opacity(0.6) : Color.secondary.opacity(0.2))
        )
    }
}

extension ChoiceCard where Footer == EmptyView {
    init(
        title: String,
        detail: String,
        isActive: Bool,
        isLoading: Bool = false,
        isDisabled: Bool = false,
        warning: String? = nil,
        onUse: @escaping () -> Void
    ) {
        self.init(
            title: title,
            detail: detail,
            isActive: isActive,
            isLoading: isLoading,
            isDisabled: isDisabled,
            warning: warning,
            onUse: onUse,
            footer: { EmptyView() }
        )
    }
}
