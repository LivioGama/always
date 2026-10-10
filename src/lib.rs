//! always — Voice activation daemon with Groq STT.
#![warn(clippy::print_stdout, clippy::print_stderr)]
// Many modules contain macOS-specific code that becomes dead code on
// linux/windows builds; allow dead code and unused imports for non-macOS targets.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]
#![cfg_attr(not(target_os = "macos"), allow(unused_imports))]

pub mod always;
pub mod config;
pub mod db;
pub mod glossary;
pub mod http_client;
pub mod managers;
pub mod stt;
pub mod stt_dispatch;
