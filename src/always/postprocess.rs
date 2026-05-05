use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use anyhow::{Context, Result};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::config::PostprocessConfig;
use crate::always::context_vocab::ContextVocabulary;
use crate::always::text::Vocabulary;

/// Smart post-processing with auto-learning and grammar correction
#[derive(Debug, Clone)]
pub struct PostProcessor {
    /// Carried for API stability (constructors accept it) but no
    /// longer read after the pipeline-simplification pass — the LLM
    /// `correct_grammar` path is the only remaining transformation.
    /// Will be dropped from the constructor signature in a follow-up
    /// once all call sites stop passing it.
    #[allow(dead_code)]
    base_vocab: Vocabulary,
    #[allow(dead_code)]
    context_vocab: Arc<Mutex<ContextVocabulary>>,
    learning_enabled: bool,
    groq_api_key: Option<String>,
    cache: Arc<Mutex<HashMap<String, String>>>,
    #[allow(dead_code)] // reserved for future LRU eviction policy
    cache_order: Arc<Mutex<VecDeque<String>>>,
    config: PostprocessConfig,
}

#[derive(Debug, Serialize, Deserialize)]
struct CorrectionEntry {
    original: String,
    corrected: String,
    timestamp: i64,
    context: String,
}

impl PostProcessor {
    pub fn new(
        base_vocab: Vocabulary,
        context_vocab: Arc<Mutex<ContextVocabulary>>,
        _learning_enabled: bool,
        groq_api_key: Option<String>,
    ) -> Self {
        Self::new_with_config(
            base_vocab,
            context_vocab,
            PostprocessConfig::default(),
            groq_api_key,
        )
    }

    pub fn new_with_config(
        base_vocab: Vocabulary,
        context_vocab: Arc<Mutex<ContextVocabulary>>,
        config: PostprocessConfig,
        groq_api_key: Option<String>,
    ) -> Self {
        Self {
            base_vocab,
            context_vocab,
            learning_enabled: false,
            groq_api_key,
            cache: Arc::new(Mutex::new(HashMap::new())),
            cache_order: Arc::new(Mutex::new(VecDeque::new())),
            config,
        }
    }

    /// Single optional cleanup pass.
    ///
    /// Pre-simplification this method ran four steps (base vocab regex,
    /// context vocab substring replace, route/root disambiguation, LLM
    /// grammar). Each was added bug-by-bug; together they produced
    /// non-debuggable interactions (e.g. `idea` being rewritten to
    /// `IntelliJ IDEA` even after the LLM step was disabled).
    ///
    /// Now this is just the LLM call when `grammar_correction_enabled`
    /// is true; otherwise raw passthrough. Strict prompt rules in
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

    pub fn learn_correction(&self, original: &str, corrected: &str, context: &str) -> Result<()> {
        if !self.learning_enabled {
            return Ok(());
        }

        let entry = CorrectionEntry {
            original: original.to_string(),
            corrected: corrected.to_string(),
            timestamp: chrono::Utc::now().timestamp(),
            context: context.to_string(),
        };

        // Save to learning database
        let vocab_path = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?
            .join(".always")
            .join("learning.json");

        let mut learning: Vec<CorrectionEntry> = if vocab_path.exists() {
            let content = std::fs::read_to_string(&vocab_path)?;
            serde_json::from_str(&content).unwrap_or_default()
        } else {
            Vec::new()
        };

        learning.push(entry);

        // Keep only last N entries based on config
        if learning.len() > self.config.learning_history_limit {
            learning = learning
                .into_iter()
                .rev()
                .take(self.config.learning_history_limit)
                .collect();
        }

        std::fs::write(&vocab_path, serde_json::to_string_pretty(&learning)?)?;

        // Update vocabulary.json if the correction is significant
        self.update_vocabulary_from_learning(original, corrected)?;

        Ok(())
    }

    fn update_vocabulary_from_learning(&self, original: &str, corrected: &str) -> Result<()> {
        let vocab_path = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?
            .join(".always")
            .join("vocabulary.json");

        if !vocab_path.exists() {
            return Ok(());
        }

        let mut vocab: Value = serde_json::from_str(&std::fs::read_to_string(&vocab_path)?)?;

        // Add to corrections if not already present
        if let Some(corrections) = vocab.get_mut("corrections")
            && let Some(corrections_map) = corrections.as_object_mut()
            && !corrections_map.contains_key(original)
        {
            corrections_map.insert(
                original.to_string(),
                serde_json::Value::String(corrected.to_string()),
            );

            std::fs::write(&vocab_path, serde_json::to_string_pretty(&vocab)?)?;
        }

        Ok(())
    }

    // `apply_learned_corrections` and `code_aware_pattern_match` were
    // removed in the pipeline-simplification pass. Both were called
    // unconditionally from `event_loop::apply_vocabulary` and produced
    // unpredictable interactions with the other rewriters. The single
    // remaining cleanup path is the optional LLM `correct_grammar`
    // call (gated on `postprocess_enabled`). See `docs/ARCHITECTURE.md`
    // and the `pre-pipeline-simplification` git tag for the historical
    // design.
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `process` returns the input verbatim when grammar correction
    /// is disabled — voice-to-text default is "deliver what was said".
    #[tokio::test]
    async fn process_passthrough_when_disabled() {
        let processor = PostProcessor::new(
            Vocabulary::default_patterns(),
            Arc::new(Mutex::new(ContextVocabulary::new(None))),
            false, // grammar_correction_enabled = false
            None,
        );
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
        let processor = PostProcessor::new(
            Vocabulary::default_patterns(),
            Arc::new(Mutex::new(ContextVocabulary::new(None))),
            true, // grammar_correction_enabled = true
            None, // groq_api_key = None
        );
        let out = processor.process("hello world", None).await.unwrap();
        assert_eq!(out, "hello world");
    }
}
