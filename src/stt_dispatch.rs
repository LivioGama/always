//! Picks the right [`Transcriber`] for the daemon's current config —
//! the only file aware of *both* the remote Groq path and the local
//! `transcribe-rs` engines.
//!
//! Why a dispatch module: `vad.rs` and `event_loop.rs` should only see
//! a `dyn Transcriber`. Local-vs-remote selection happens once at
//! daemon start (and again on hot-swap when the user changes the
//! active model from the Settings UI), so the rest of the codebase
//! stays backend-agnostic.

use std::str::FromStr;
use std::sync::Arc;

use anyhow::{Context, Result};

use crate::always::AlwaysConfig;
use crate::managers::model_registry::ModelRegistry;
use crate::stt::{GroqTranscriber, Transcriber};

/// User's transcription backend pick. Serialises to a single TEXT
/// column in the prefs DB (`groq` or `local:<model_id>`). The
/// FromStr/Display impls own that wire format so the DB layer and the
/// UDS server stay in sync.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscriberBackendChoice {
    /// Remote Groq Whisper API. Requires `GROQ_API_KEY`.
    Groq,
    /// Local model identified by the registry id (e.g.
    /// `parakeet-tdt-0.6b-v3`). Must be downloaded before it can be
    /// activated — [`build_transcriber`] falls back to Groq when the
    /// model file is missing on disk.
    Local { model_id: String },
}

impl Default for TranscriberBackendChoice {
    fn default() -> Self {
        Self::Groq
    }
}

impl std::fmt::Display for TranscriberBackendChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Groq => f.write_str("groq"),
            Self::Local { model_id } => write!(f, "local:{model_id}"),
        }
    }
}

impl FromStr for TranscriberBackendChoice {
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
        anyhow::bail!("invalid transcriber backend: {s} (expected 'groq' or 'local:<id>')");
    }
}

/// Construct the [`Transcriber`] the daemon should use for the next
/// utterance, based on `cfg.transcriber_backend` and what's actually
/// downloaded.
///
/// Fallback policy:
///
/// * Local backend, model on disk → [`LocalTranscriber`].
/// * Local backend, model missing or `local-stt` feature off →
///   Groq (with a warning). The alternative — refusing to start —
///   leaves users stranded if they delete a downloaded model.
/// * Groq backend → [`GroqTranscriber`] using the config's API key.
pub fn build_transcriber(
    cfg: &AlwaysConfig,
    registry: &ModelRegistry,
) -> Result<Arc<dyn Transcriber>> {
    match &cfg.transcriber_backend {
        TranscriberBackendChoice::Groq => Ok(build_groq(cfg)?),
        TranscriberBackendChoice::Local { model_id } => {
            let info = registry
                .get(model_id)
                .with_context(|| format!("unknown local model id: {model_id}"))?;
            if !info.is_downloaded {
                tracing::warn!(
                    model = %model_id,
                    "local model not downloaded, falling back to remote Groq"
                );
                return build_groq(cfg);
            }
            build_local(cfg, registry, &info)
        }
    }
}

fn build_groq(cfg: &AlwaysConfig) -> Result<Arc<dyn Transcriber>> {
    let key = cfg
        .groq_stt_api_key
        .as_deref()
        .filter(|k| !k.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Groq backend selected but GROQ_API_KEY is not set. \
                 Configure it via `always config set groq_api_key <key>` or \
                 select a downloaded local model in Settings → Models."
            )
        })?;
    Ok(Arc::new(GroqTranscriber::new(key)))
}

#[cfg(feature = "local-stt")]
fn build_local(
    cfg: &AlwaysConfig,
    registry: &ModelRegistry,
    info: &crate::managers::model_registry::ModelInfo,
) -> Result<Arc<dyn Transcriber>> {
    use crate::always::stt_local::LocalTranscriber;

    let path = registry
        .model_path(&info.id)
        .ok_or_else(|| anyhow::anyhow!("registry missing path for {}", info.id))?;
    // `auto` is the configured-language sentinel for "let the engine
    // decide". Whisper / Canary / SenseVoice all accept None for this;
    // monolingual engines (Parakeet V2 / Moonshine / GigaAM) ignore it.
    let lang = if cfg.lang == "auto" || cfg.lang.is_empty() {
        None
    } else {
        Some(cfg.lang.clone())
    };
    let t = LocalTranscriber::load(info.engine_type, &path, lang)
        .with_context(|| format!("loading local engine for model {}", info.id))?;
    tracing::info!(
        model = %info.id,
        engine = ?info.engine_type,
        "local_transcriber_ready"
    );
    Ok(Arc::new(t))
}

#[cfg(not(feature = "local-stt"))]
fn build_local(
    cfg: &AlwaysConfig,
    _registry: &ModelRegistry,
    info: &crate::managers::model_registry::ModelInfo,
) -> Result<Arc<dyn Transcriber>> {
    tracing::error!(
        model = %info.id,
        "local-stt feature not compiled in; falling back to Groq"
    );
    build_groq(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_groq() {
        assert_eq!(
            "groq".parse::<TranscriberBackendChoice>().unwrap(),
            TranscriberBackendChoice::Groq
        );
    }

    #[test]
    fn parse_local() {
        let parsed: TranscriberBackendChoice =
            "local:parakeet-tdt-0.6b-v3".parse().unwrap();
        assert!(matches!(
            parsed,
            TranscriberBackendChoice::Local { ref model_id } if model_id == "parakeet-tdt-0.6b-v3"
        ));
    }

    #[test]
    fn display_round_trips() {
        let g = TranscriberBackendChoice::Groq;
        assert_eq!(g.to_string(), "groq");
        let l = TranscriberBackendChoice::Local {
            model_id: "small".into(),
        };
        assert_eq!(l.to_string(), "local:small");
        assert_eq!(
            l.to_string().parse::<TranscriberBackendChoice>().unwrap(),
            l
        );
    }

    #[test]
    fn rejects_bad_inputs() {
        assert!("".parse::<TranscriberBackendChoice>().is_err());
        assert!("local:".parse::<TranscriberBackendChoice>().is_err());
        assert!("openai".parse::<TranscriberBackendChoice>().is_err());
    }
}
