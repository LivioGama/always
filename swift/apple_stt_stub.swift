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
