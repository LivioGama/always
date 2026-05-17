import Foundation

enum GroqKeyValidationResult: Equatable {
    case valid
    case invalid(String)
}

func groqKeyValidationResult(statusCode: Int?) -> GroqKeyValidationResult {
    statusCode == 200
        ? .valid
        : .invalid("Invalid API key - Groq rejected the credentials")
}

func groqKeyValidationResult(error: Error) -> GroqKeyValidationResult {
    .invalid("Could not reach Groq: \(error.localizedDescription)")
}

func shouldPersistApiKey(_ apiKey: String) -> Bool {
    !apiKey.isEmpty && !apiKey.allSatisfy { $0 == "•" }
}
