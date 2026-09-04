//! Always — the first speech-to-text software designed to be always on
//! and free your hands from the affordance.

pub mod audio;
pub mod auto_enter_countdown;
pub mod chunker;
pub mod clipboard_watcher;
pub mod common_words;
pub mod config;
pub mod context_heuristics;
pub mod correction;
pub mod correction_extract;
pub mod correction_metrics;
pub mod correction_queue;
pub mod correction_request;
pub mod daemon;
pub mod dictation;
pub mod enrollment;
pub mod event;
pub mod event_loop;
pub mod filter;
pub mod filter_config;
pub mod focus_state;
pub mod hallucination;
pub mod hud;
pub mod idle_watcher;
pub mod keyboard;
pub mod keyring;
pub mod localization;
pub mod log;
pub mod mic_monitor;
pub mod mic_watcher;
pub mod notification;
pub mod paste;
pub mod pause;
pub mod per_app;
pub mod postprocess;
pub mod snippets;
pub mod speaker_embed;
pub mod speech_action;
pub mod status_sound;
#[cfg(feature = "local-stt")]
pub mod stt_local;
pub mod telemetry;
pub mod text_match;
pub mod transcript_stream;
pub mod uds_server;
pub mod vad;
pub mod vad_silero;
pub mod vocab;
pub mod voiceprint;

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub mod apple_intelligence;

pub use config::AlwaysConfig;
pub use event_loop::run;
