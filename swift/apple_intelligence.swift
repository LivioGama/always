import Dispatch
import Foundation
import FoundationModels

@available(macOS 26.0, *)
@Generable
private struct CleanedTranscript: Sendable {
    let cleanedText: String
}

// MARK: - Swift implementation for Apple LLM integration
// Compiled via Cargo build script for Apple Silicon targets.

private typealias ResponsePointer = UnsafeMutablePointer<AppleLLMResponse>

private func duplicateCString(_ text: String) -> UnsafeMutablePointer<CChar>? {
    return text.withCString { basePointer in
        guard let duplicated = strdup(basePointer) else {
            return nil
        }
        return duplicated
    }
}

private func truncatedText(_ text: String, limit: Int) -> String {
    guard limit > 0 else { return text }
    let words = text.split(
        maxSplits: .max,
        omittingEmptySubsequences: true,
        whereSeparator: { $0.isWhitespace || $0.isNewline }
    )
    if words.count <= limit {
        return text
    }
    return words.prefix(limit).joined(separator: " ")
}

@_cdecl("is_apple_intelligence_available")
public func isAppleIntelligenceAvailable() -> Int32 {
    guard #available(macOS 26.0, *) else {
        return 0
    }

    let model = SystemLanguageModel.default
    switch model.availability {
    case .available:
        return 1
    case .unavailable:
        return 0
    }
}

// MARK: - Prewarmed session pool (experiment; harness-only, see mode bits)
//
// A session is single-use here (reusing one would append every previous
// transcript to its context), so the pool holds ONE fresh session for the
// current prompt, created and `prewarm()`ed ahead of time. Measured: no
// gain over a fresh session (generation dominates), so production does not
// use it.

@available(macOS 26.0, *)
private final class SessionPool: @unchecked Sendable {
    static let shared = SessionPool()
    private let lock = NSLock()
    private var prompt: String?
    private var ready: LanguageModelSession?

    func take(prompt: String) -> LanguageModelSession? {
        lock.lock()
        defer { lock.unlock() }
        guard self.prompt == prompt, let s = ready else { return nil }
        ready = nil
        return s
    }

    func refill(prompt: String) {
        let session = LanguageModelSession(model: SystemLanguageModel.default, instructions: prompt)
        session.prewarm()
        lock.lock()
        self.prompt = prompt
        ready = session
        lock.unlock()
    }
}

@_cdecl("apple_intelligence_prewarm")
public func appleIntelligencePrewarm(_ systemPrompt: UnsafePointer<CChar>) {
    guard #available(macOS 26.0, *) else { return }
    guard SystemLanguageModel.default.availability == .available else { return }
    let prompt = String(cString: systemPrompt)
    Task.detached(priority: .utility) { SessionPool.shared.refill(prompt: prompt) }
}

@_cdecl("process_text_with_system_prompt_apple")
public func processTextWithSystemPrompt(
    _ systemPrompt: UnsafePointer<CChar>,
    _ userContent: UnsafePointer<CChar>,
    maxTokens: Int32
) -> UnsafeMutablePointer<AppleLLMResponse> {
    // Production keeps the original behaviour (fresh session + guided
    // generation): the interleaved A/B harness (2026-10-14, n=12, load avg
    // 60-130) measured pooled-prewarmed + plain at p50 9966 ms vs 8472 ms
    // for this path — session setup is not the bottleneck; generation is.
    // Prompt size was the only lever that helped (-20% with the compact
    // prompt). The paste path no longer waits on this call at all.
    return processTextWithSystemPromptMode(systemPrompt, userContent, maxTokens: maxTokens, mode: 2)
}

/// `mode` bit 0 = take the prewarmed pooled session (and refill it);
/// bit 1 = guided generation (@Generable) with plain fallback. Production
/// uses 2 (see above); the A/B latency harness drives the others.
@_cdecl("process_text_with_system_prompt_apple_mode")
public func processTextWithSystemPromptMode(
    _ systemPrompt: UnsafePointer<CChar>,
    _ userContent: UnsafePointer<CChar>,
    maxTokens: Int32,
    mode: Int32
) -> UnsafeMutablePointer<AppleLLMResponse> {
    let usePool = mode & 1 != 0
    let useStructured = mode & 2 != 0
    let swiftSystemPrompt = String(cString: systemPrompt)
    let swiftUserContent = String(cString: userContent)
    let responsePtr = ResponsePointer.allocate(capacity: 1)
    responsePtr.initialize(to: AppleLLMResponse(response: nil, success: 0, error_message: nil))

    guard #available(macOS 26.0, *) else {
        responsePtr.pointee.error_message = duplicateCString(
            "Apple Intelligence requires macOS 26 or newer."
        )
        return responsePtr
    }

    let model = SystemLanguageModel.default
    guard model.availability == .available else {
        responsePtr.pointee.error_message = duplicateCString(
            "Apple Intelligence is not currently available on this device."
        )
        return responsePtr
    }

    let tokenLimit = max(0, Int(maxTokens))
    let semaphore = DispatchSemaphore(value: 0)

    final class ResultBox: @unchecked Sendable {
        var response: String?
        var error: String?
    }
    let box = ResultBox()

    Task.detached(priority: .userInitiated) {
        defer { semaphore.signal() }
        do {
            let pooled = usePool ? SessionPool.shared.take(prompt: swiftSystemPrompt) : nil
            let session = pooled
                ?? LanguageModelSession(model: model, instructions: swiftSystemPrompt)
            if usePool {
                // Line up the next call's session while this one generates.
                Task.detached(priority: .utility) {
                    SessionPool.shared.refill(prompt: swiftSystemPrompt)
                }
            }
            var output: String

            if useStructured {
                do {
                    let structured = try await session.respond(
                        to: swiftUserContent,
                        generating: CleanedTranscript.self
                    )
                    output = structured.content.cleanedText
                } catch {
                    let fallbackGeneration = try await session.respond(to: swiftUserContent)
                    output = fallbackGeneration.content
                }
            } else {
                output = try await session.respond(to: swiftUserContent).content
            }

            if tokenLimit > 0 {
                output = truncatedText(output, limit: tokenLimit)
            }
            box.response = output
        } catch {
            box.error = error.localizedDescription
        }
    }

    semaphore.wait()

    if let response = box.response {
        responsePtr.pointee.response = duplicateCString(response)
        responsePtr.pointee.success = 1
    } else {
        responsePtr.pointee.error_message = duplicateCString(box.error ?? "Unknown error")
    }

    return responsePtr
}

@_cdecl("free_apple_llm_response")
public func freeAppleLLMResponse(_ response: UnsafeMutablePointer<AppleLLMResponse>?) {
    guard let response = response else { return }

    if let responseStr = response.pointee.response {
        free(UnsafeMutablePointer(mutating: responseStr))
    }

    if let errorStr = response.pointee.error_message {
        free(UnsafeMutablePointer(mutating: errorStr))
    }

    response.deallocate()
}
