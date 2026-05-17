use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use anyhow::{Context, Result};
use parking_lot::Mutex;
use serde_json::Value;

use super::config::PostprocessConfig;

/// Maximum number of cached (input → corrected) entries before we
/// evict the oldest. Each entry is bounded by the LLM's max_tokens
/// response cap (~500 chars) so 1 000 entries ~= 1 MB upper bound.
const CACHE_MAX_ENTRIES: usize = 1_000;

/// Optional LLM cleanup pass for the transcript.
///
/// Single transformation: send the transcript to the Groq LLM with a
/// strict system prompt (see `glossary::build_postprocess_prompt`) and
/// return the cleaned text. Falls through to the raw input when
/// `grammar_correction_enabled` is false or no API key is configured.
#[derive(Debug, Clone)]
pub struct PostProcessor {
    groq_api_key: Option<String>,
    /// Memoization of `(transcript → corrected)` pairs. Bounded by
    /// `CACHE_MAX_ENTRIES`: the previous implementation never evicted
    /// and grew unboundedly across long daemon sessions.
    cache: Arc<Mutex<HashMap<String, String>>>,
    /// Insertion order used for LRU-style eviction. The pair
    /// `(cache, cache_order)` is treated as a single unit; both locks
    /// are taken together in `insert_cached`.
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

        self.insert_cached(text.to_string(), corrected.clone());
        Ok(corrected)
    }

    /// Insert a `(input, corrected)` pair into the cache with LRU
    /// eviction. The previous implementation only used the HashMap
    /// half and the unbounded VecDeque was reserved-but-never-used.
    /// Long daemon sessions accumulated entries indefinitely.
    fn insert_cached(&self, text: String, corrected: String) {
        let mut cache = self.cache.lock();
        let mut order = self.cache_order.lock();
        // Replace via `insert`: returns the previous value if the key was
        // present, in which case we already had this entry and just need
        // to bump its position in the LRU queue. Otherwise it's a fresh
        // insert and we may need to evict.
        if cache.insert(text.clone(), corrected).is_some() {
            order.retain(|k| k != &text);
            order.push_back(text);
            return;
        }
        order.push_back(text);
        while order.len() > CACHE_MAX_ENTRIES {
            if let Some(victim) = order.pop_front() {
                cache.remove(&victim);
            } else {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `process` returns the input verbatim when grammar correction
    /// is disabled — voice-to-text default is "deliver what was said".
    #[tokio::test]
    async fn process_passthrough_when_disabled() {
        let cfg = PostprocessConfig { grammar_correction_enabled: false, ..Default::default() };
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
