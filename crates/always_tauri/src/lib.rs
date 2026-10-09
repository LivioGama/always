//! Always Tauri v2 — the system-tray frontend for the Always speech-to-text
//! daemon.
//!
//! This crate does **not** replace the daemon. It communicates with the
//! running `always` daemon over a Unix Domain Socket using the JSON-over-
//! lines protocol v12 (see [`crate::uds_client::DaemonCommand`] and
//! [`crate::uds_client::DaemonEvent`]).
//!
#![warn(clippy::print_stdout, clippy::print_stderr)]

pub mod tray;
pub mod uds_client;
pub mod window_manager;