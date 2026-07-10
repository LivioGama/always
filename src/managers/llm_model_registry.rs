//! LLM model registry for local postprocessing fallback.
//!
//! Manages local LLM models (quantized gguf files) for grammar correction
//! and style transforms when Groq is unavailable. Simpler than model_registry.rs
//! since gguf models are single files (no tar.gz extraction needed).

use anyhow::Result;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;

use super::model_download::{VerifyCache, compute_sha256, verify_sha256};

/// LLM model information.
#[derive(Debug, Clone)]
pub struct LlmModelInfo {
    /// Unique identifier (e.g., "qwen2.5-3b-instruct-q4_k_m").
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Download URL for the gguf file.
    pub url: String,
    /// Expected SHA256 hash of the gguf file.
    pub sha256: String,
    /// File size in bytes.
    pub size_bytes: u64,
    /// Whether this is the recommended default model.
    pub is_recommended: bool,
}

/// Registry of known LLM models.
pub struct LlmModelRegistry {
    models_dir: PathBuf,
    catalog: HashMap<String, LlmModelInfo>,
    verify_cache: VerifyCache,
}

impl LlmModelRegistry {
    /// Build the registry from the hardcoded catalog.
    pub fn new(models_dir: PathBuf) -> Self {
        let mut catalog = HashMap::new();
        populate_catalog(&mut catalog);

        Self {
            models_dir,
            catalog,
            verify_cache: VerifyCache::default(),
        }
    }

    /// Get all available models from the catalog.
    pub fn available_models(&self) -> &HashMap<String, LlmModelInfo> {
        &self.catalog
    }

    /// Get a specific model by ID.
    pub fn get_model(&self, id: &str) -> Option<&LlmModelInfo> {
        self.catalog.get(id)
    }

    /// Get the recommended default model.
    pub fn recommended_model(&self) -> Option<&LlmModelInfo> {
        self.catalog
            .values()
            .find(|m| m.is_recommended)
            .or_else(|| self.catalog.values().next())
    }

    /// Get the local path for a model.
    pub fn model_path(&self, model_id: &str) -> Option<PathBuf> {
        let _model = self.catalog.get(model_id)?;
        Some(self.models_dir.join(format!("{}.gguf", model_id)))
    }

    /// Download a model from its URL.
    pub fn download_model(&self, model_id: &str) -> Result<()> {
        let model = self
            .catalog
            .get(model_id)
            .ok_or_else(|| anyhow::anyhow!("Unknown model: {}", model_id))?;

        let path = self.model_path(model_id).unwrap();
        let tmp_path = path.with_extension("gguf.tmp");

        // Create models directory if needed
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Download
        tracing::info!(model = model_id, url = %model.url, "llm_model_download_starting");
        let response = reqwest::blocking::get(&model.url)?;
        if !response.status().is_success() {
            anyhow::bail!("Failed to download model: HTTP {}", response.status());
        }

        let bytes = response.bytes()?;
        let mut file = File::create(&tmp_path)?;
        file.write_all(&bytes)?;

        tracing::info!(
            model = model_id,
            bytes = bytes.len(),
            "llm_model_download_complete"
        );

        // Verify SHA256
        tracing::info!(model = model_id, "llm_model_verify_sha256");
        verify_sha256(&tmp_path, Some(&model.sha256), model_id)?;

        // Atomic rename
        fs::rename(&tmp_path, &path)?;
        tracing::info!(model = model_id, "llm_model_ready");

        Ok(())
    }

    /// Check if a model is downloaded and verified.
    pub fn is_downloaded(&self, model_id: &str) -> bool {
        let Some(path) = self.model_path(model_id) else {
            return false;
        };
        let Some(model) = self.catalog.get(model_id) else {
            return false;
        };

        if !path.exists() {
            return false;
        }

        // Verify SHA256 if cache misses
        let metadata = match fs::metadata(&path) {
            Ok(m) => m,
            Err(_) => return false,
        };

        let mtime = match metadata.modified() {
            Ok(t) => t,
            Err(_) => return false,
        };
        let len = metadata.len();
        let key = (path.clone(), len, mtime);

        if let Some(&verdict) = self.verify_cache.lock().get(&key) {
            return verdict;
        }

        // Compute fresh hash
        let verdict = match compute_sha256(&path) {
            Ok(actual) => actual == model.sha256,
            Err(_) => false,
        };

        self.verify_cache.lock().insert(key, verdict);
        verdict
    }
}

/// Populate the catalog with known LLM models.
fn populate_catalog(catalog: &mut HashMap<String, LlmModelInfo>) {
    // Qwen2.5-3B-Instruct-Q4_K_M - 2.0 GB, fast inference
    // Real file from official Qwen repo, SHA256 computed from actual download
    catalog.insert(
        "qwen2.5-3b-instruct-q4_k_m".to_string(),
        LlmModelInfo {
            id: "qwen2.5-3b-instruct-q4_k_m".to_string(),
            name: "Qwen2.5-3B-Instruct Q4_K_M".to_string(),
            url: "https://huggingface.co/Qwen/Qwen2.5-3B-Instruct-GGUF/resolve/main/qwen2.5-3b-instruct-q4_k_m.gguf".to_string(),
            sha256: "626b4a6678b86442240e33df819e00132d3ba7dddfe1cdc4fbb18e0a9615c62d".to_string(),
            size_bytes: 2_007_000_000, // ~2.0 GB (actual download size)
            is_recommended: true,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_includes_qwen_model() {
        let mut catalog = HashMap::new();
        populate_catalog(&mut catalog);

        assert!(catalog.contains_key("qwen2.5-3b-instruct-q4_k_m"));
        let model = catalog.get("qwen2.5-3b-instruct-q4_k_m").unwrap();
        assert_eq!(model.id, "qwen2.5-3b-instruct-q4_k_m");
        assert!(model.is_recommended);
    }

    #[test]
    fn catalog_has_recommended_model() {
        let mut catalog = HashMap::new();
        populate_catalog(&mut catalog);

        let recommended: Vec<&str> = catalog
            .values()
            .filter(|m| m.is_recommended)
            .map(|m| m.id.as_str())
            .collect();
        assert_eq!(recommended, vec!["qwen2.5-3b-instruct-q4_k_m"]);
    }

    #[test]
    fn each_catalog_entry_has_url_and_sha256() {
        let mut catalog = HashMap::new();
        populate_catalog(&mut catalog);

        for m in catalog.values() {
            assert!(!m.url.is_empty(), "{} missing url", m.id);
            assert_eq!(m.sha256.len(), 64, "{} has malformed sha256", m.id);
        }
    }

    #[test]
    fn registry_returns_recommended_model() {
        let dir = std::env::temp_dir().join("always-llm-registry-test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let registry = LlmModelRegistry::new(dir.clone());
        let recommended = registry.recommended_model();
        assert!(recommended.is_some());
        assert_eq!(recommended.unwrap().id, "qwen2.5-3b-instruct-q4_k_m");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn model_path_constructed_correctly() {
        let dir = std::env::temp_dir().join("always-llm-registry-test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let registry = LlmModelRegistry::new(dir.clone());
        let path = registry.model_path("qwen2.5-3b-instruct-q4_k_m");
        assert!(path.is_some());
        assert_eq!(path.unwrap(), dir.join("qwen2.5-3b-instruct-q4_k_m.gguf"));

        let _ = fs::remove_dir_all(&dir);
    }
}
