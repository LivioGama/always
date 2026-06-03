//! Always-on voice activation.

pub mod audio;
pub mod auto_enter_countdown;
pub mod clipboard_watcher;
pub mod config;
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
#[cfg(feature = "local-stt")]
pub mod stt_local;
pub mod telemetry;
pub mod text_match;
pub mod uds_server;
pub mod vad;
pub mod vocab;

pub use config::AlwaysConfig;
pub use event_loop::run;
