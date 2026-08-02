import Foundation

/// Utility for reshaping and validating transcription text before final paste.
/// Applies capitalization, punctuation, and formatting rules to improve text quality.
enum TextReshaper {

    /// Reshape the given text with capitalization, punctuation, and formatting rules.
    /// - Parameter text: The raw transcription text to reshape
    /// - Returns: The reshaped text
    static func reshape(_ text: String) -> String {
        var result = text

        // Apply transformations in order
        result = capitalizeFirstLetter(result)
        result = fixSentenceSpacing(result)
        result = addMissingPeriods(result)
        result = fixCommonTypos(result)

        return result
    }

    /// Capitalize the first letter of the text.
    private static func capitalizeFirstLetter(_ text: String) -> String {
        guard !text.isEmpty else { return text }
        let first = text.prefix(1).capitalized
        let rest = text.dropFirst()
        return first + rest
    }

    /// Fix spacing issues (multiple spaces, space before punctuation).
    private static func fixSentenceSpacing(_ text: String) -> String {
        var result = text

        // Replace multiple spaces with single space
        while result.contains("  ") {
            result = result.replacingOccurrences(of: "  ", with: " ")
        }

        // Remove space before common punctuation
        for punct in [".", ",", "!", "?", ":", ";"] {
            result = result.replacingOccurrences(of: " \(punct)", with: punct)
        }

        // Add space after punctuation if missing (except for single-letter words)
        for punct in [".", "!", "?", ":", ";"] {
            // Look for punctuation followed by a lowercase letter (not already spaced)
            let pattern = "\(punct)([a-z])"
            result = result.replacingOccurrences(
                of: pattern,
                with: "\(punct) $1",
                options: .regularExpression
            )
        }

        return result
    }

    /// Add missing period at the end if the text looks like a complete sentence.
    private static func addMissingPeriods(_ text: String) -> String {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return text }

        // Don't add period if already ends with punctuation
        let lastChar = trimmed.suffix(1)
        if [".", "!", "?", ":", ";"].contains(lastChar) {
            return text
        }

        // Don't add period if ends with incomplete word patterns
        let incompletePatterns = ["ing ", "ed ", "tion ", "ment ", "ness "]
        for pattern in incompletePatterns {
            if trimmed.hasSuffix(pattern) {
                return text
            }
        }

        // Add period if text is reasonably long and looks like a sentence
        // (has at least one space and doesn't end with common prepositions)
        if trimmed.count > 10 && trimmed.contains(" ") {
            let prepositions = ["in ", "on ", "at ", "to ", "for ", "with ", "by ", "from "]
            let endsWithPreposition = prepositions.contains { trimmed.hasSuffix($0) }
            if !endsWithPreposition {
                return text + "."
            }
        }

        return text
    }

    /// Fix common transcription typos and formatting issues.
    private static func fixCommonTypos(_ text: String) -> String {
        var result = text

        // Fix "i" to "I" when it's a standalone word
        result = result.replacingOccurrences(
            of: "\\bi\\b",
            with: "I",
            options: .regularExpression
        )

        // Fix common homophone errors (basic set)
        result = result.replacingOccurrences(of: " its a", with: " it's a")
        result = result.replacingOccurrences(of: " its the", with: " it's the")
        result = result.replacingOccurrences(of: " your welcome", with: " you're welcome")

        // Fix double negatives (simple cases)
        result = result.replacingOccurrences(of: "dont", with: "don't")
        result = result.replacingOccurrences(of: "cant", with: "can't")
        result = result.replacingOccurrences(of: "wont", with: "won't")
        result = result.replacingOccurrences(of: "im", with: "I'm")

        return result
    }
}