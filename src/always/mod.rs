//! Always-on voice activation.

pub mod ai_filter;
pub mod audio;
pub mod auto_enter_countdown;
pub mod clipboard_watcher;
pub mod config;
pub mod context_vocab;
pub mod correction;
pub mod correction_queue;
pub mod daemon;
pub mod event;
pub mod event_loop;
pub mod filter;
pub mod filter_config;
pub mod hallucination;
pub mod hud;
pub mod idle_watcher;
pub mod keyboard;
pub mod keyring;
pub mod log;
pub mod mic_monitor;
pub mod notification;
pub mod paste;
pub mod pause;
pub mod per_app;
pub mod postprocess;
pub mod smart_filter;
#[cfg(feature = "local-stt")]
pub mod stt_local;
pub mod telemetry;
pub mod text;
pub mod text_match;
pub mod uds_server;
pub mod vad;
pub mod vocab;

// DEPRECATED: File-based state persistence replaced by UDS event streaming
// Kept for reference only - will be removed in future version
#[deprecated(since = "0.14.0", note = "Use UDS event streaming instead")]
pub mod state;

pub use config::AlwaysConfig;
pub use event_loop::run;
