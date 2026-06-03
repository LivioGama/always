//! Always-on voice activation.

pub mod audio;
pub mod config;
pub mod daemon;
pub mod event_loop;
pub mod filter;
pub mod hud;
pub mod keyboard;
pub mod loading;
pub mod log;
pub mod notification;
pub mod paste;
pub mod pause;
pub mod text;
pub mod vad;
pub mod context_vocab;
pub mod postprocess;
pub mod vocab;

pub use config::AlwaysConfig;
pub use event_loop::run;
