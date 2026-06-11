//! Linux overlay companion process for Always
//! 
//! This is a separate process that connects to the daemon via UDS socket,
//! receives daemon events, and renders overlay UI. It mirrors the macOS 
//! overlay behavior using the same daemon event stream.

pub mod main;
pub mod renderer;
pub mod state;
pub mod uds_client;