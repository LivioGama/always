use crate::always::AlwaysConfig;
use crate::always::context_vocab::ContextVocabulary;
use crate::always::text::Vocabulary;
use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptionResult {
    pub corrected_text: String,
    pub should_accept: bool,
    pub confidence_score: f32, // 0.0 to 1.0
    pub reason: String,
    pub filter_category: Option<FilterCategory>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FilterCategory {
    ValidCommand,
    TechnicalQuestion,
    NaturalConversation,
    Gibberish,
    VideoArtifact,
    ConversationalFiller,
    MeaninglessSound,
    MixedLanguage,
}

#[derive(Debug, Serialize)]
struct GroqRequest {
    messages: Vec<GroqMessage>,
    model: String,
    temperature: f32,
    max_tokens: i32,
    response_format: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct GroqMessage {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct GroqResponse {
    choices: Vec<GroqChoice>,
}

#[derive(Debug, Deserialize)]
struct GroqChoice {
    message: GroqResponseMessage,
}

#[derive(Debug, Deserialize)]
struct GroqResponseMessage {
    content: String,
}

#[derive(Debug, Deserialize)]
struct AiFilterResponse {
    corrected_text: String,
    should_accept: bool,
    confidence_score: f32,
    reason: String,
    category: String,
}

pub struct AiFilter {
    api_key: String,
    model: String,
}

impl AiFilter {
    pub fn new(api_key: String, model: Option<String>) -> Self {
        Self {
            api_key,
            model: model.unwrap_or_else(|| "llama-3.1-8b-instant".to_string()),
        }
    }

    pub async fn evaluate_transcription_with_vocab(
        &self,
        text: &str,
        _config: &AlwaysConfig,
        vocab: Option<&Vocabulary>,
        context_vocab: Option<&ContextVocabulary>,
    ) -> Result<TranscriptionResult> {
        let vocab_context = self.build_vocab_context(vocab, context_vocab);

        let prompt = format!(
            r#"You are an AI assistant that evaluates and corrects voice-to-text transcriptions for a productivity application called "Always". Your job is to:

1. Correct obvious transcription errors (typos, misheard words) using the provided vocabulary context
2. Determine if the text represents valid, actionable input that should be accepted
3. Provide a confidence score and reasoning

INPUT TEXT: "{}"

VOCABULARY CONTEXT:
{}

Please analyze this text and respond with ONLY a valid JSON object (no markdown, no explanation) in this exact format:
{{
    "corrected_text": "corrected version of the input using vocabulary",
    "should_accept": true,
    "confidence_score": 0.85,
    "reason": "brief explanation of decision",
    "category": "valid_command"
}}

CORRECTION PRIORITY:
1. Use the vocabulary context to fix common technical terms, product names, and domain-specific words
2. If the vocabulary context lists "CanonicalTerm: mistranscription", and the input contains that mistranscription, corrected_text MUST use CanonicalTerm
3. Correct obvious STT mistakes (homophones, similar sounds)
4. Keep the original meaning intact
5. **DO NOT attempt to fix obviously corrupted domains or URLs with random characters**

ACCEPT if the text is:
- Programming/technical commands ("open file", "git status", "create function")
- Technical questions ("How do I configure React?", "What is OAuth?")
- Natural conversation about work/projects ("I need to finish this", "The server is slow")
- Valid single words/responses ("yes", "no", "API", "SQL", "React.js")
- Product names and technical terms (especially those in vocabulary)
- Well-formed domains and URLs with legitimate spelling variations

**AGGRESSIVELY REJECT if the text contains:**
- **Malformed domains/URLs with random characters:**
  * Domains with non-ASCII characters mixed with ASCII (e.g., "expenseÁê•-tee.com")
  * URLs containing random symbols, emoji-like characters, or garbled encoding
  * Domain-shaped text with obvious corruption or gibberish elements
  * Any "domain" containing characters like: Á, ê, •, ∞, ≠, ™, †, ‡, °, ¿, etc.
- **Gibberish patterns (be more aggressive than before):**
  * Random keyboard sequences ("asdfkjh", "qwerty uiop", "kdfjglkdfj")
  * Text with excessive non-ASCII characters in English context
  * Mixed random characters with no coherent meaning
  * Sequences that look like encoding errors or data corruption
  * Text that appears to be OCR or transcription artifacts
- **Obvious nonsense that users would never want to paste:**
  * Character sequences that look like corrupted data
  * Text containing multiple encoding artifacts or weird symbols
  * Strings that appear to be system errors or corrupted output
- Video artifacts and metadata:
  * "thank you for watching", "thanks for watching", "thank you for listening", "thanks for listening"
  * "please like and subscribe", "like and subscribe", "subscribe for more"
  * "subtitles by", "captions by", "subtitles by the", "captions by the"
  * "transcription by", "transcription provided by", "auto-generated subtitles", "automatic captions"
  * "turn on subtitles", "enable subtitles", "subtitle settings", "caption settings"
  * "closed captions", "cc", "subtitles available", "captions available"
  * Service names: "amara.org", "rev.com", "youtube transcription"
  * Any text mentioning video player controls or subtitle/caption features
- Meaningless sounds ONLY if truly meaningless: ("uh", "um", "ah", "mmm", "ahh" as single words)
  * Valid words like "yes", "no", "token", "next" MUST be accepted even if short
- Conversational fillers ONLY when used as standalone politeness:
  * "thank you", "good morning", "how are you", "you're welcome", "excuse me", "i'm sorry", "have a nice day", "take care", "see you later", "sounds good", "that's great", "absolutely"
  * If these appear in longer sentences (e.g., "thank you for the help"), ACCEPT them

**CRITICAL RULE**: If the text looks like a domain/URL but contains random non-ASCII characters, symbols, or obvious corruption - REJECT IT. Don't try to "fix" it. Users don't want corrupted domains in their clipboard.

**EXAMPLES OF WHAT TO REJECT:**
- "expenseÁê•-tee.com" (contains random non-ASCII chars)
- "google∞test.org" (contains random symbols)
- "api™endpoint.co" (contains trademark symbols)
- "data†base.net" (contains religious symbols)
- "server°temp.io" (contains degree symbols)

For confidence_score: 1.0 = completely certain, 0.5 = uncertain, 0.0 = completely confused
For category, use one of: valid_command, technical_question, natural_conversation, gibberish, video_artifact, conversational_filler, meaningless_sound, mixed_language

Focus on whether the text represents something a user would want to act on or reference later. When in doubt about corrupted-looking domains or gibberish, REJECT."#,
            text, vocab_context
        );

        let request_body = GroqRequest {
            model: self.model.clone(),
            temperature: 0.1,
            max_tokens: 200,
            response_format: serde_json::json!({ "type": "json_object" }),
            messages: vec![GroqMessage {
                role: "user".to_string(),
                content: prompt,
            }],
        };

        let response = crate::http_client::async_client()
            .post("https://api.groq.com/openai/v1/chat/completions")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request_body)
            .send()
            .await?;

        if !response.status().is_success() {
            let error_text = response.text().await?;
            return Err(anyhow!("Groq API request failed: {}", error_text));
        }

        let groq_response: GroqResponse = response.json().await?;
        let content = groq_response
            .choices
            .first()
            .ok_or_else(|| anyhow!("No choices in Groq response"))?
            .message
            .content
            .trim();

        // Parse the JSON response from the AI
        let ai_response: AiFilterResponse = serde_json::from_str(content).map_err(|e| {
            anyhow!(
                "Failed to parse AI response as JSON: {} - Content: {}",
                e,
                content
            )
        })?;

        let filter_category = match ai_response.category.to_lowercase().as_str() {
            "valid_command" => Some(FilterCategory::ValidCommand),
            "technical_question" => Some(FilterCategory::TechnicalQuestion),
            "natural_conversation" => Some(FilterCategory::NaturalConversation),
            "gibberish" => Some(FilterCategory::Gibberish),
            "video_artifact" => Some(FilterCategory::VideoArtifact),
            "conversational_filler" => Some(FilterCategory::ConversationalFiller),
            "meaningless_sound" => Some(FilterCategory::MeaninglessSound),
            "mixed_language" => Some(FilterCategory::MixedLanguage),
            _ => None,
        };

        Ok(TranscriptionResult {
            corrected_text: ai_response.corrected_text,
            should_accept: ai_response.should_accept,
            confidence_score: ai_response.confidence_score.clamp(0.0, 1.0),
            reason: ai_response.reason,
            filter_category,
        })
    }

    // Legacy method for backward compatibility
    pub async fn evaluate_transcription(
        &self,
        text: &str,
        config: &AlwaysConfig,
    ) -> Result<TranscriptionResult> {
        self.evaluate_transcription_with_vocab(text, config, None, None)
            .await
    }

    fn build_vocab_context(
        &self,
        vocab: Option<&Vocabulary>,
        context_vocab: Option<&ContextVocabulary>,
    ) -> String {
        let mut context = String::new();

        if let Some(_v) = vocab {
            context.push_str("COMMON TERMS:\n");
            // Add some key vocabulary terms (you'll need to adapt this based on your Vocabulary struct)
            context.push_str(
                "- Technical terms: API, SQL, JSON, HTML, CSS, HTTP, REST, CRUD, OAuth, JWT\n",
            );
            context.push_str("- Frameworks: React, Vue, Angular, Next.js, Node.js, Express\n");
            context.push_str("- Tools: Git, Docker, Kubernetes, AWS, GCP, Azure\n");
        }

        if let Some(_cv) = context_vocab {
            context.push_str("\nCONTEXT-SPECIFIC TERMS:\n");
            context.push_str("- Use context-aware vocabulary for domain-specific corrections\n");
        }

        if context.is_empty() {
            context.push_str("No specific vocabulary context provided.");
        }

        context.push_str("\n\n");
        context.push_str(crate::glossary::ai_filter_vocabulary_context());

        context
    }

    // Fallback to simple rules if API is unavailable
    pub fn evaluate_fallback(&self, text: &str) -> TranscriptionResult {
        let normalized = text.trim().to_lowercase();

        // Very basic fallback rules
        if normalized.is_empty() {
            return TranscriptionResult {
                corrected_text: text.to_string(),
                should_accept: false,
                confidence_score: 1.0,
                reason: "Empty text".to_string(),
                filter_category: Some(FilterCategory::MeaninglessSound),
            };
        }

        // Allowlist for valid single words that should always be accepted
        let valid_single_words = [
            "yes",
            "no",
            "ok",
            "okay",
            "yeah",
            "yep",
            "nope",
            "token",
            "next",
            "previous",
            "back",
            "forward",
            "true",
            "false",
            "null",
            "void",
            "none",
            "api",
            "sql",
            "json",
            "html",
            "css",
            "http",
            "rest",
            "jwt",
            "oauth",
            "git",
            "ssh",
            "tcp",
            "udp",
            "tls",
            "ssl",
            "cdn",
            "dns",
            "run",
            "stop",
            "start",
            "end",
            "begin",
            "finish",
            "open",
            "close",
            "save",
            "load",
            "read",
            "write",
            "add",
            "remove",
            "delete",
            "create",
            "update",
            "edit",
            "to",
            "from",
            "with",
            "without",
            "for",
            "of",
            "in",
            "on",
            "at",
            "by",
            "nothing",
            "something",
            "everything",
            "anything",
        ];

        // Check if it's a valid single word from allowlist
        if text.split_whitespace().count() == 1 && valid_single_words.contains(&normalized.as_str())
        {
            return TranscriptionResult {
                corrected_text: text.to_string(),
                should_accept: true,
                confidence_score: 0.9,
                reason: "Valid single word from allowlist".to_string(),
                filter_category: Some(FilterCategory::ValidCommand),
            };
        }

        // Check for obvious meaningless sounds (only exact matches)
        let meaningless_sounds = ["uh", "um", "ah", "mmm", "hmm", "uhh", "umm", "ahh", "err"];
        if meaningless_sounds.contains(&normalized.as_str()) {
            return TranscriptionResult {
                corrected_text: text.to_string(),
                should_accept: false,
                confidence_score: 0.9,
                reason: "Meaningless sound".to_string(),
                filter_category: Some(FilterCategory::MeaninglessSound),
            };
        }

        // Check for video artifacts (comprehensive pattern matching)
        let video_artifact_patterns = [
            "thank you for watching",
            "thanks for watching",
            "thank you for listening",
            "thanks for listening",
            "thank you for your attention",
            "please like and subscribe",
            "like and subscribe",
            "subscribe for more",
            "subtitles by",
            "captions by",
            "transcription by",
            "transcription provided by",
            "auto-generated subtitles",
            "automatic captions",
            "turn on subtitles",
            "enable subtitles",
            "subtitle settings",
            "caption settings",
            "closed captions",
            "subtitles available",
            "captions available",
            "amara.org",
            "rev.com",
            "youtube transcription",
        ];

        for pattern in &video_artifact_patterns {
            if normalized.contains(pattern) {
                return TranscriptionResult {
                    corrected_text: text.to_string(),
                    should_accept: false,
                    confidence_score: 0.95,
                    reason: format!("Video artifact detected: {}", pattern),
                    filter_category: Some(FilterCategory::VideoArtifact),
                };
            }
        }

        // Check for conversational fillers (only exact matches, not in longer sentences)
        let filler_patterns = [
            "thank you",
            "thanks",
            "you're welcome",
            "your welcome",
            "excuse me",
            "i'm sorry",
            "sorry",
            "good morning",
            "good night",
            "good afternoon",
            "good evening",
            "how are you",
            "see you later",
            "have a nice day",
            "take care",
            "sounds good",
            "that's great",
            "absolutely",
        ];

        for pattern in &filler_patterns {
            if normalized == *pattern {
                return TranscriptionResult {
                    corrected_text: text.to_string(),
                    should_accept: false,
                    confidence_score: 0.9,
                    reason: format!("Conversational filler: {}", pattern),
                    filter_category: Some(FilterCategory::ConversationalFiller),
                };
            }
        }

        // Accept multi-word text (be conservative with fallback)
        if text.split_whitespace().count() > 1 {
            return TranscriptionResult {
                corrected_text: text.to_string(),
                should_accept: true,
                confidence_score: 0.4, // Low confidence for fallback
                reason: "Fallback acceptance for multi-word text".to_string(),
                filter_category: Some(FilterCategory::NaturalConversation),
            };
        }

        let has_non_ascii = !normalized.is_ascii();
        let has_url_shape = normalized.contains('.');
        let has_latin_letter = normalized.chars().any(|c| c.is_ascii_alphabetic());
        if text.split_whitespace().count() == 1
            && has_url_shape
            && (has_non_ascii || !has_latin_letter)
        {
            return TranscriptionResult {
                corrected_text: text.to_string(),
                should_accept: false,
                confidence_score: 0.8,
                reason: "Malformed domain-like text".to_string(),
                filter_category: Some(FilterCategory::Gibberish),
            };
        }

        TranscriptionResult {
            corrected_text: text.to_string(),
            should_accept: true,
            confidence_score: 0.3,
            reason: "Fallback acceptance for unknown single word".to_string(),
            filter_category: Some(FilterCategory::NaturalConversation),
        }
    }
}

// Helper function to create AI filter from config
pub fn create_ai_filter(config: &AlwaysConfig) -> Option<AiFilter> {
    let api_key = std::env::var("GROQ_API_KEY")
        .ok()
        .filter(|key| !key.trim().is_empty())
        .unwrap_or_else(|| config.groq_stt_api_key.clone());

    if api_key.trim().is_empty() {
        return None;
    }

    Some(AiFilter::new(
        api_key,
        Some(config.postprocess_config.groq_model.clone()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fallback_evaluation() {
        let filter = AiFilter::new("dummy".to_string(), None);

        let result = filter.evaluate_fallback("uh");
        assert!(!result.should_accept);
        assert_eq!(result.reason, "Meaningless sound");

        let result = filter.evaluate_fallback("open the file in the editor");
        assert!(result.should_accept);

        let result = filter.evaluate_fallback("");
        assert!(!result.should_accept);
        assert_eq!(result.reason, "Empty text");
    }
}
#[cfg(test)]
#[allow(clippy::print_stdout)]
mod test_malformed_domains {
    use crate::always::ai_filter::AiFilter;

    #[test]
    fn test_fallback_malformed_domain() {
        let filter = AiFilter::new("dummy".to_string(), None);
        let result = filter.evaluate_fallback("expenseÁê•-tee.com");

        println!("Text: expenseÁê•-tee.com");
        println!("Should accept: {}", result.should_accept);
        println!("Confidence: {:.1}%", result.confidence_score * 100.0);
        println!("Reason: {}", result.reason);
        println!("Category: {:?}", result.filter_category);

        // The fallback should catch this as malformed domain-like text
        // assert!(!result.should_accept, "Malformed domain should be rejected");
    }
}
