import Foundation

/// Today's Groq usage as last reported by the daemon (`GroqUsage` event).
/// Its own store so a per-request update re-renders only the usage bar.
final class GroqUsageStore: ObservableObject {
    static let shared = GroqUsageStore()

    @Published private(set) var usage: GroqUsageData?

    func apply(_ next: GroqUsageData?) {
        guard let next else { return }
        DispatchQueue.main.async {
            if self.usage != next { self.usage = next }
        }
    }
}

extension GroqUsageData {
    /// Share (0…1) of the free daily allowance used: the tightest of audio
    /// time, LLM tokens and requests per day.
    var freeUsedFraction: Double {
        var parts = [
            audio_seconds / max(1, audio_seconds_free),
            llm_tokens / max(1, llm_tokens_free),
        ]
        for (limit, remaining) in [
            (stt_requests_limit, stt_requests_remaining),
            (llm_requests_limit, llm_requests_remaining),
        ] {
            if let limit, let remaining, limit > 0 {
                parts.append((limit - remaining) / limit)
            }
        }
        return min(1, max(0, parts.max() ?? 0))
    }
}
