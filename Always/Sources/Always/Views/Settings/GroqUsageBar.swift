import SwiftUI

/// Today's Groq usage under the API key (owner request 2026-11-25). Free
/// plan: how much of the free daily allowance is used — Groq refuses past
/// it, it never bills. Paid plan: the estimated spend so far today.
struct GroqUsageBar: View {
    @ObservedObject private var store: GroqUsageStore = .shared

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            if let usage = store.usage {
                if usage.plan == "paid" {
                    paid(usage)
                } else {
                    free(usage)
                }
            } else {
                Text("Usage today shows up after your first Groq request.")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }
        }
    }

    private func free(_ usage: GroqUsageData) -> some View {
        let used = usage.freeUsedFraction
        return VStack(alignment: .leading, spacing: 4) {
            HStack {
                Text("Free usage today").font(.caption)
                Spacer()
                Text("\(Int((used * 100).rounded()))% used")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .monospacedDigit()
            }
            ProgressView(value: used)
                .progressViewStyle(.linear)
                .tint(used >= 0.9 ? .orange : .accentColor)
            Text(freeDetail(usage))
                .font(.caption2)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    private func paid(_ usage: GroqUsageData) -> some View {
        HStack {
            Text("Paid usage today").font(.caption)
            Spacer()
            Text(String(format: "≈ $%.2f", usage.cost_usd))
                .font(.caption)
                .fontWeight(.semibold)
                .monospacedDigit()
        }
        .help("Estimated from Groq's published prices and what Always sent today (UTC). Groq's console has the exact bill.")
    }

    private func freeDetail(_ usage: GroqUsageData) -> String {
        let audio = "\(duration(usage.audio_seconds)) of \(duration(usage.audio_seconds_free)) audio"
        let tokens = "\(Int(usage.llm_tokens / 1000))K of \(Int(usage.llm_tokens_free / 1000))K cleanup tokens"
        return "\(audio) · \(tokens). Past it, Groq refuses requests until its daily limit resets; the free plan is never billed."
    }

    private func duration(_ seconds: Double) -> String {
        let minutes = Int(seconds / 60)
        return minutes >= 60 ? "\(minutes / 60)h \(minutes % 60)m" : "\(minutes)m"
    }
}
