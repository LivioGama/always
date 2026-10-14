import Foundation

// Stub implementation when the Speech framework / SFSpeechRecognizer is not available.
// Compiled when the build environment does not support the Apple on-device STT backend.

private typealias ResponsePointer = UnsafeMutablePointer<AppleSttResponse>

@_cdecl("is_apple_stt_available")
public func isAppleSttAvailable() -> Int32 {
    return 0
}

@_cdecl("transcribe_wav_with_apple_stt")
public func transcribeWavWithAppleStt(
    _ wavPath: UnsafePointer<CChar>,
    _ lang: UnsafePointer<CChar>?,
    _ phrases: UnsafePointer<CChar>?
) -> UnsafeMutablePointer<AppleSttResponse> {
    let responsePtr = ResponsePointer.allocate(capacity: 1)
    responsePtr.initialize(to: AppleSttResponse(text: nil, success: 0, error_message: nil, confidence: -1))

    let msg = "Apple STT not available in this build (SDK requirement not met)."
    responsePtr.pointee.error_message = strdup(msg)

    return responsePtr
}

@_cdecl("free_apple_stt_result")
public func freeAppleSttResult(_ response: UnsafeMutablePointer<AppleSttResponse>?) {
    guard let response = response else { return }

    if let text = response.pointee.text {
        free(UnsafeMutablePointer(mutating: text))
    }

    if let errorMessage = response.pointee.error_message {
        free(UnsafeMutablePointer(mutating: errorMessage))
    }

    response.deallocate()
}

// ---- Streaming recognition: unavailable in stub builds ----
// `apple_stt_stream_start` returns NULL, so the daemon never holds a session
// handle and falls back to the file path; the remaining entry points exist
// only so the Rust side links against older SDKs.

@_cdecl("apple_stt_stream_start")
public func appleSttStreamStart(
    _ lang: UnsafePointer<CChar>?,
    _ phrases: UnsafePointer<CChar>?
) -> UnsafeMutableRawPointer? {
    return nil
}

@_cdecl("apple_stt_stream_set_callback")
public func appleSttStreamSetCallback(
    _ handle: UnsafeMutableRawPointer,
    _ cb: apple_stt_partial_cb?,
    _ ctx: UnsafeMutableRawPointer?
) {}

@_cdecl("apple_stt_stream_feed")
public func appleSttStreamFeed(
    _ handle: UnsafeMutableRawPointer,
    _ samples: UnsafePointer<Int16>,
    _ count: Int32
) {}

@_cdecl("apple_stt_stream_partial")
public func appleSttStreamPartial(
    _ handle: UnsafeMutableRawPointer,
    _ seen: UnsafePointer<CChar>?
) -> UnsafeMutablePointer<CChar>? {
    return nil
}

@_cdecl("apple_stt_stream_finish")
public func appleSttStreamFinish(
    _ handle: UnsafeMutableRawPointer,
    _ timeoutMs: Int32
) -> UnsafeMutablePointer<CChar>? {
    return nil
}

@_cdecl("apple_stt_stream_outcome")
public func appleSttStreamOutcome(_ handle: UnsafeMutableRawPointer) -> Int32 {
    return 3
}

@_cdecl("apple_stt_stream_free")
public func appleSttStreamFree(_ handle: UnsafeMutableRawPointer?) {}

@_cdecl("free_apple_stt_string")
public func freeAppleSttString(_ ptr: UnsafeMutablePointer<CChar>?) {
    guard let ptr else { return }
    free(ptr)
}
