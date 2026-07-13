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
    !apiKey.isEmpty && !isMaskedApiKeyPlaceholder(apiKey)
}

func isMaskedApiKeyPlaceholder(_ apiKey: String) -> Bool {
    let trimmed = apiKey.trimmingCharacters(in: .whitespacesAndNewlines)
    guard !trimmed.isEmpty else {
        return false
    }
    if trimmed.contains("(in keychain)") {
        return true
    }
    return trimmed.allSatisfy { character in
        character == "•" || character == "*" || character == "●"
    }
}
