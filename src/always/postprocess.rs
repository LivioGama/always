use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use anyhow::{Context, Result};
use parking_lot::Mutex;
use serde_json::Value;

use super::config::PostprocessConfig;

/// Optional LLM cleanup pass for the transcript.
///
/// Single transformation: send the transcript to the Groq LLM with a
/// strict system prompt (see `glossary::build_postprocess_prompt`) and
/// return the cleaned text. Falls through to the raw input when
/// `grammar_correction_enabled` is false or no API key is configured.
#[derive(Debug, Clone)]
pub struct PostProcessor {
    groq_api_key: Option<String>,
    cache: Arc<Mutex<HashMap<String, String>>>,
    #[allow(dead_code)] // reserved for future LRU eviction policy
    cache_order: Arc<Mutex<VecDeque<String>>>,
    config: PostprocessConfig,
}

impl PostProcessor {
    pub fn new(groq_api_key: Option<String>) -> Self {
        Self::new_with_config(PostprocessConfig::default(), groq_api_key)
    }

    pub fn new_with_config(config: PostprocessConfig, groq_api_key: Option<String>) -> Self {
        Self {
            groq_api_key,
            cache: Arc::new(Mutex::new(HashMap::new())),
            cache_order: Arc::new(Mutex::new(VecDeque::new())),
            config,
        }
    }

    /// Single optional cleanup pass.
    ///
    /// Just the LLM call when `grammar_correction_enabled` is true;
    /// otherwise raw passthrough. Strict prompt rules in
    /// `glossary::build_postprocess_prompt` keep the LLM from inventing
    /// substitutions. Voice-to-text delivers what was said by default.
    pub async fn process(&self, text: &str, _context: Option<&str>) -> Result<String> {
        if !self.config.grammar_correction_enabled {
            return Ok(text.to_string());
        }
        let Some(ref api_key) = self.groq_api_key else {
            return Ok(text.to_string());
        };
        Ok(self
            .correct_grammar(text, api_key)
            .await
            .unwrap_or_else(|_| text.to_string()))
    }

    async fn correct_grammar(&self, text: &str, api_key: &str) -> Result<String> {
        // Check cache first
        if let Some(cached) = self.cache.lock().get(text) {
            return Ok(cached.clone());
        }

        let client = reqwest::Client::new();
        let response = client
            .post("https://api.groq.com/openai/v1/chat/completions")
            .header("Authorization", format!("Bearer {}", api_key))
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({
                "model": self.config.groq_model,
                "messages": [
                    {
                        "role": "system",
                        "content": crate::glossary::postprocess_system_prompt()
                    },
                    {
                        "role": "user",
                        "content": format!("<transcript>{}</transcript>", text)
                    }
                ],
                "temperature": 0.1,
                "max_tokens": 500
            }))
            .send()
            .await
            .context("Failed to call Groq API")?;

        if !response.status().is_success() {
            let error = response.text().await.unwrap_or_default();
            anyhow::bail!("Groq API error: {}", error);
        }

        let json: Value = response
            .json()
            .await
            .context("Failed to parse Groq response")?;
        let corrected = json["choices"][0]["message"]["content"]
            .as_str()
            .context("Invalid Groq response format")?
            .trim()
            .to_string();

        // Cache the result
        self.cache
            .lock()
            .insert(text.to_string(), corrected.clone());

        Ok(corrected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `process` returns the input verbatim when grammar correction
    /// is disabled — voice-to-text default is "deliver what was said".
    #[tokio::test]
    async fn process_passthrough_when_disabled() {
        let mut cfg = PostprocessConfig::default();
        cfg.grammar_correction_enabled = false;
        let processor = PostProcessor::new_with_config(cfg, None);
        let out = processor
            .process("I have an idea about Kubernetes.", None)
            .await
            .unwrap();
        assert_eq!(out, "I have an idea about Kubernetes.");
    }

    /// Even with grammar correction enabled, missing API key falls
    /// through to passthrough (we don't make the daemon hang on a
    /// configuration error).
    #[tokio::test]
    async fn process_passthrough_when_enabled_but_no_api_key() {
        let processor = PostProcessor::new(None);
        let out = processor.process("hello world", None).await.unwrap();
        assert_eq!(out, "hello world");
    }
}
