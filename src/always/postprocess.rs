use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use anyhow::{Context, Result};
use parking_lot::Mutex;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::config::PostprocessConfig;
use crate::always::context_vocab::ContextVocabulary;
use crate::always::text::Vocabulary;

/// Smart post-processing with auto-learning and grammar correction
#[derive(Debug, Clone)]
pub struct PostProcessor {
    base_vocab: Vocabulary,
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

    pub async fn process(&self, text: &str, context: Option<&str>) -> Result<String> {
        let mut result = text.to_string();

        // Step 1: Apply base vocabulary corrections
        result = self.base_vocab.apply(&result);

        // Step 2: Apply context-aware corrections
        {
            let context_vocab = self.context_vocab.lock();
            let context_corrections = context_vocab.get_context_corrections();
            for (wrong, correct) in context_corrections {
                result = result.replace(&wrong, &correct);
            }
        }

        // Step 3: Context-aware disambiguation
        result = self.disambiguate_context(&result, context);

        // Step 4: Grammar correction with Groq Llama 3 (if API key available and enabled)
        if self.config.grammar_correction_enabled
            && let Some(ref api_key) = self.groq_api_key
        {
            result = self
                .correct_grammar(&result, api_key)
                .await
                .unwrap_or(result);
        }

        Ok(result)
    }

    fn disambiguate_context(&self, text: &str, context: Option<&str>) -> String {
        let mut result = text.to_string();

        if let Some(ctx) = context {
            // Filesystem context: "route" -> "root"
            if ctx.contains("/") || ctx.contains("file") || ctx.contains("path") {
                result = result.replace("route", "root");
                result = result.replace("rute", "root");
                result = result.replace("rowt", "root");
            }

            // Git context: "branch" -> "branch" (no change)
            // "checkout" -> "checkout"

            // Code context: "break" -> "break" (no change in code)
            if ctx.contains("code") || ctx.contains("function") || ctx.contains("loop") {
                // Keep "break" as is in code context
            } else {
                // In natural language, "brake" -> "break"
                result = result.replace("brake", "break");
            }
        }

        result
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
                        "content": text
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

    pub fn apply_learned_corrections(&self, text: &str) -> String {
        let mut result = text.to_string();

        let Some(home) = dirs::home_dir() else {
            tracing::warn!("home directory unavailable; skipping learned corrections");
            return result;
        };
        let vocab_path = home.join(".always").join("learning.json");

        if vocab_path.exists()
            && let Ok(content) = std::fs::read_to_string(&vocab_path)
            && let Ok(learning) = serde_json::from_str::<Vec<CorrectionEntry>>(&content)
        {
            for entry in learning {
                result = result.replace(&entry.original, &entry.corrected);
            }
        }

        result
    }

    pub fn code_aware_pattern_match(&self, text: &str) -> String {
        let mut result = text.to_string();

        // Code-specific patterns
        let patterns = vec![
            (r"\bimport\s+(\w+)\s+from", "import $1 from"),
            (r"\bconst\s+(\w+)\s*=", "const $1 ="),
            (r"\blet\s+(\w+)\s*=", "let $1 ="),
            (r"\bfunction\s+(\w+)\s*\(", "function $1("),
            (r"\bclass\s+(\w+)\s*{", "class $1 {"),
            (r"\bif\s*\(([^)]+)\)\s*{", "if ($1) {"),
            (r"\bfor\s*\(([^)]+)\)\s*{", "for ($1) {"),
            (r"\bwhile\s*\(([^)]+)\)\s*{", "while ($1) {"),
            (r"\breturn\s+([^;]+);", "return $1;"),
            (r"\bconsole\.log\(([^)]+)\)", "console.log($1)"),
        ];

        for (pattern, replacement) in patterns {
            if let Ok(regex) = Regex::new(pattern) {
                result = regex.replace_all(&result, replacement).to_string();
            }
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disambiguates_route_to_root_in_filesystem_context() {
        let processor = PostProcessor::new(
            Vocabulary::default_patterns(),
            Arc::new(Mutex::new(ContextVocabulary::new(None))),
            false,
            None,
        );

        assert_eq!(
            processor.disambiguate_context("go to route", Some("file path")),
            "go to root"
        );
    }

    #[test]
    fn applies_learned_corrections() {
        let processor = PostProcessor::new(
            Vocabulary::default_patterns(),
            Arc::new(Mutex::new(ContextVocabulary::new(None))),
            false,
            None,
        );

        assert_eq!(
            processor.apply_learned_corrections("test text"),
            "test text"
        );
    }
}
