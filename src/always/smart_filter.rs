use crate::always::ai_filter::{create_ai_filter, TranscriptionResult};
use crate::always::filter;
use crate::always::AlwaysConfig;

pub async fn should_accept_with_ai(text: &str, config: &AlwaysConfig) -> (bool, String, Option<String>) {
    // Try AI filtering first if available
    if let Some(ai_filter) = create_ai_filter(config) {
        match ai_filter.evaluate_transcription(text, config).await {
            Ok(result) => {
                let reason = if result.should_accept {
                    format!("AI: {} (confidence: {:.1}%)", result.reason, result.confidence_score * 100.0)
                } else {
                    format!("AI: {} (confidence: {:.1}%)", result.reason, result.confidence_score * 100.0)
                };

                // Return corrected text if different and accepted
                let corrected = if result.corrected_text != text && result.should_accept {
                    Some(result.corrected_text)
                } else {
                    None
                };

                return (result.should_accept, reason, corrected);
            }
            Err(e) => {
                eprintln!("AI filter failed, falling back to rule-based: {}", e);
                // Fall through to rule-based filtering
            }
        }
    }

    // Fallback to existing rule-based filtering
    let filter_result = filter::should_accept_with_reason(text, config);
    let accepted = matches!(filter_result, filter::FilterReason::None);
    let reason = filter_result.to_log_string();

    (accepted, reason, None)
}

// Synchronous version that uses fallback rules
pub fn should_accept_with_simple_ai(text: &str, config: &AlwaysConfig) -> (bool, String, Option<String>) {
    if let Some(ai_filter) = create_ai_filter(config) {
        let result = ai_filter.evaluate_fallback(text);
        let reason = format!("Simple AI: {} (confidence: {:.1}%)", result.reason, result.confidence_score * 100.0);

        let corrected = if result.corrected_text != text && result.should_accept {
            Some(result.corrected_text)
        } else {
            None
        };

        (result.should_accept, reason, corrected)
    } else {
        // Ultimate fallback to existing rule-based filtering
        let filter_result = filter::should_accept_with_reason(text, config);
        let accepted = matches!(filter_result, filter::FilterReason::None);
        let reason = filter_result.to_log_string();

        (accepted, reason, None)
    }
}