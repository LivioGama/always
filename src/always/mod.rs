//! Always — the first speech-to-text software designed to be always on
//! and free your hands from the affordance.

pub mod audio;
pub mod auto_enter_countdown;
pub mod ax_edit;
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
pub mod det_correct;
pub mod dictation;
pub mod enrollment;
pub mod event;
pub mod event_loop;
pub mod filter;
pub mod filter_config;
pub mod focus_state;
pub mod hallucination;
pub mod held;
pub mod history;
pub mod hud;
pub mod idle_watcher;
pub mod keyboard;
pub mod keyring;
pub mod level_meter;
#[cfg(target_os = "linux")]
pub mod linux_keys;
pub mod live_preview;
pub mod live_type;
pub mod localization;
pub mod log;
pub mod mic_monitor;
pub mod mic_watcher;
pub mod notification;
pub mod paste;
pub mod pause;
pub mod pause_commands;
#[cfg(windows)]
pub(crate) mod peer_transport_windows;
pub mod per_app;
pub mod permissions;
pub mod postprocess;
pub mod preview_asr;
pub mod rejected;
mod replay_tests;
pub mod snippets;
pub mod speaker_embed;
pub mod speaker_trim;
pub mod speech_action;
pub mod status_sound;
#[cfg(feature = "local-stt")]
pub mod stt_local;
pub mod telemetry;
pub mod text_match;
pub mod transcript_stream;
pub mod uds_server;
pub mod updater;
pub mod vad;
pub mod vad_silero;
pub mod vocab;
pub mod voice_edit;
pub mod voice_edit_grammar;
mod voice_edit_live;
mod voice_edit_replay;
pub mod voiceprint;

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub mod apple_intelligence;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub mod apple_stt;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub mod stt_apple;
#[cfg(target_os = "windows")]
pub mod win32_process;

pub use config::AlwaysConfig;
pub use event_loop::run;

/// Serialises the RSS-growth regression tests (Silero / speaker-embed /
/// mic-probe). They measure `getrusage` peak RSS inside one shared test
/// process; running concurrently would count each other's allocations
/// and produce nonsense verdicts.
#[cfg(test)]
pub(crate) mod leak_probe_lock {
    use parking_lot::Mutex;
    pub(crate) static LOCK: Mutex<()> = Mutex::new(());
}
