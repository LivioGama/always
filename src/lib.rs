//! always — Voice activation daemon with Groq STT.
#![warn(clippy::print_stdout, clippy::print_stderr)]

pub mod always;
pub mod config;
pub mod db;
pub mod glossary;
pub mod http_client;
pub mod stt;
