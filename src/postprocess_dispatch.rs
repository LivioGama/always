//! Postprocessing backend dispatch — Groq vs local LLM.
//!
//! Mirrors the STT dispatch pattern in `stt_dispatch.rs`. The daemon can
//! use either the remote Groq API or a local llama.cpp model for grammar
//! correction and style transforms.

use std::str::FromStr;

#[cfg(feature = "local-postprocess")]
use std::sync::Arc;

use anyhow::Result;

#[cfg(feature = "local-postprocess")]
use parking_lot::Mutex;

/// Failure modes for postprocessing calls.
///
/// Mirrors `SttError` from `stt.rs` with postprocess-specific variants.
#[derive(Debug, thiserror::Error)]
pub enum PostprocessError {
    #[error("Groq rate-limited the request after {attempts} attempts")]
    RateLimited { attempts: u32 },
    #[error("Groq returned a server error after {attempts} attempts (last status {status})")]
    ServerError { status: u16, attempts: u32 },
    #[error("network error talking to Groq after {attempts} attempts: {source}")]
    Network {
        attempts: u32,
        #[source]
        source: reqwest::Error,
    },
    #[error("Groq postprocessing circuit breaker is open (cool-down {remaining_ms} ms remaining)")]
    Unavailable { remaining_ms: u64 },
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl PostprocessError {
    /// Whether the daemon should fall back to local LLM on this error.
    /// Only the circuit breaker triggers fallback — other errors have
    /// already exhausted the retry budget and should bubble up.
    pub fn should_fall_back(&self) -> bool {
        matches!(self, PostprocessError::Unavailable { .. })
    }
}

/// User's postprocessing backend pick. Serialises to a single TEXT
/// column in the prefs DB (`groq` or `local:<model_id>`). The
/// FromStr/Display impls own that wire format so the DB layer and the
/// UDS server stay in sync.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum PostprocessBackendChoice {
    /// Remote Groq LLM API. Requires `GROQ_API_KEY`.
    #[default]
    Groq,
    /// Local LLM model identified by the registry id (e.g.
    /// `qwen2.5-3b-instruct-q4_k_m`). Must be downloaded before it can be
    /// activated.
    Local { model_id: String },
}

impl std::fmt::Display for PostprocessBackendChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Groq => f.write_str("groq"),
            Self::Local { model_id } => write!(f, "local:{model_id}"),
        }
    }
}

impl FromStr for PostprocessBackendChoice {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s == "groq" {
            return Ok(Self::Groq);
        }
        if let Some(rest) = s.strip_prefix("local:") {
            if rest.is_empty() {
                anyhow::bail!("local backend missing model id");
            }
            return Ok(Self::Local {
                model_id: rest.to_string(),
            });
        }
        anyhow::bail!("invalid postprocess backend: {s} (expected 'groq' or 'local:<id>')");
    }
}

/// Slot for lazily-loaded local LLM engine.
#[cfg(feature = "local-postprocess")]
enum LocalSlot {
    Ready(Arc<dyn crate::always::postprocess::LocalPostprocessor>),
    Failed,
}

#[cfg(feature = "local-postprocess")]
type LocalLoader =
    Box<dyn Fn() -> Result<Arc<dyn crate::always::postprocess::LocalPostprocessor>> + Send + Sync>;

/// Wrapper that falls back to a local LLM on Groq unavailability.
///
/// Mirrors `FallbackTranscriber` from `stt_dispatch.rs`. The local engine
/// is only loaded on the first fallback (3–10 s for a cold model load).
#[cfg(feature = "local-postprocess")]
pub struct FallbackPostprocessor {
    primary: Arc<crate::always::postprocess::PostProcessor>,
    /// Lazily-initialized local engine (`None` = not yet attempted).
    local: Mutex<Option<LocalSlot>>,
    loader: LocalLoader,
    model_id: String,
}

#[cfg(feature = "local-postprocess")]
impl FallbackPostprocessor {
    pub fn new(
        primary: Arc<crate::always::postprocess::PostProcessor>,
        model_id: String,
        loader: LocalLoader,
    ) -> Self {
        Self {
            primary,
            local: Mutex::new(None),
            loader,
            model_id,
        }
    }

    fn local_engine(&self) -> Option<Arc<dyn crate::always::postprocess::LocalPostprocessor>> {
        let slot = self.local.lock();
        match &*slot {
            Some(LocalSlot::Ready(t)) => return Some(Arc::clone(t)),
            Some(LocalSlot::Failed) => return None,
            None => {}
        }
        drop(slot);
        tracing::info!(model = %self.model_id, "postprocess_fallback_engaged, loading local model");
        let loaded = match (self.loader)() {
            Ok(t) => t,
            Err(e) => {
                tracing::error!(model = %self.model_id, error = %e, "postprocess_fallback_load_failed");
                *self.local.lock() = Some(LocalSlot::Failed);
                return None;
            }
        };
        *self.local.lock() = Some(LocalSlot::Ready(Arc::clone(&loaded)));
        tracing::info!(model = %self.model_id, "postprocess_fallback_loaded");
        Some(loaded)
    }
}

/// Local LLM postprocessor implementation using llama-cpp-2.
#[cfg(feature = "local-postprocess")]
pub struct LlamaPostprocessor {
    model_path: std::path::PathBuf,
}

#[cfg(feature = "local-postprocess")]
impl LlamaPostprocessor {
    pub fn load(model_path: &std::path::Path) -> Result<Self> {
        // Verify model exists
        if !model_path.exists() {
            anyhow::bail!("Model file not found: {}", model_path.display());
        }
        tracing::info!(model_path = %model_path.display(), "llama_postprocessor_loaded");
        Ok(Self {
            model_path: model_path.to_path_buf(),
        })
    }
}

#[cfg(feature = "local-postprocess")]
impl crate::always::postprocess::LocalPostprocessor for LlamaPostprocessor {
    fn correct(&self, system_prompt: &str, user_text: &str) -> Result<String, PostprocessError> {
        use std::process::{Command, Stdio};

        // Format ChatML prompt manually (Qwen2.5 uses ChatML)
        let prompt = format!(
            "<|im_start|>system\n{}\n<|im_end|>\n<|im_start|>user\n{}\n<|im_end|>\n<|im_start|>assistant\n",
            system_prompt, user_text
        );

        // Call llama.cpp CLI
        let output = Command::new("llama-cli")
            .arg("-m")
            .arg(&self.model_path)
            .arg("-p")
            .arg(&prompt)
            .arg("-n")
            .arg("500") // max tokens like Groq
            .arg("--temp")
            .arg("0.3") // temperature like Groq
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| {
                PostprocessError::Other(anyhow::anyhow!(
                    "Failed to run llama-cli: {}. Make sure llama.cpp is installed and in PATH.",
                    e
                ))
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(PostprocessError::Other(anyhow::anyhow!(
                "llama-cli failed: {}",
                stderr
            )));
        }

        let output_text = String::from_utf8_lossy(&output.stdout).to_string();

        // Clean up ChatML tokens from output
        let cleaned = output_text
            .strip_prefix("<|im_start|>assistant\n")
            .unwrap_or(&output_text)
            .strip_suffix("<|im_end|>")
            .unwrap_or(&output_text)
            .trim()
            .to_string();

        Ok(cleaned)
    }
}
