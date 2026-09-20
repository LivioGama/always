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
    responsePtr.initialize(to: AppleSttResponse(text: nil, success: 0, error_message: nil))

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

// Streaming session stubs — never available in stub builds.

@_cdecl("apple_stt_stream_supported")
public func appleSttStreamSupported() -> Int32 { 0 }

@_cdecl("apple_stt_stream_start")
public func appleSttStreamStart(
    _ lang: UnsafePointer<CChar>?,
    _ phrases: UnsafePointer<CChar>?
) -> UnsafeMutableRawPointer? { nil }

@_cdecl("apple_stt_stream_push")
public func appleSttStreamPush(
    _ session: UnsafeMutableRawPointer?,
    _ samples: UnsafePointer<Float>?,
    _ count: Int
) -> UnsafeMutablePointer<CChar>? { nil }

@_cdecl("apple_stt_stream_finish")
public func appleSttStreamFinish(
    _ session: UnsafeMutableRawPointer?
) -> UnsafeMutablePointer<AppleSttResponse> {
    let responsePtr = UnsafeMutablePointer<AppleSttResponse>.allocate(capacity: 1)
    responsePtr.initialize(to: AppleSttResponse(text: nil, success: 0, error_message: nil))
    responsePtr.pointee.error_message = strdup("Apple STT streaming not available in this build.")
    return responsePtr
}

@_cdecl("apple_stt_stream_cancel")
public func appleSttStreamCancel(_ session: UnsafeMutableRawPointer?) {}

@_cdecl("free_apple_stt_string")
public func freeAppleSttString(_ s: UnsafeMutablePointer<CChar>?) {
    if let s { free(s) }
}
