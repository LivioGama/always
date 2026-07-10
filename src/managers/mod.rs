//! Background managers that own long-lived daemon state.
//!
//! Currently houses the model registry. Lives outside `always::` so it
//! can be reused from CLI commands, the UDS server, and the event loop
//! without circular module boundaries.

pub mod llm_model_registry;
pub mod model_download;
pub mod model_registry;

pub use llm_model_registry::{LlmModelInfo, LlmModelRegistry};
pub use model_download::{
    PersistedVerdict, VerifyCache, compute_sha256, load_verify_cache, save_verify_cache,
    verify_sha256,
};
pub use model_registry::{DownloadProgress, EngineType, ModelEvent, ModelInfo, ModelRegistry};
