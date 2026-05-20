//! Background managers that own long-lived daemon state.
//!
//! Currently houses the model registry. Lives outside `always::` so it
//! can be reused from CLI commands, the UDS server, and the event loop
//! without circular module boundaries.

pub mod model_registry;

pub use model_registry::{
    DownloadProgress, EngineType, ModelEvent, ModelInfo, ModelRegistry,
};
