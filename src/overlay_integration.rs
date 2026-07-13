//! Overlay companion process integration
//!
//! This module provides integration with the overlay companion process.
//! When the overlay feature is enabled, it provides the `overlay run` command.

use anyhow::{Context, Result};

pub fn run() -> Result<()> {
    // Launch the overlay binary
    let current_exe = std::env::current_exe()?;
    let overlay_binary = current_exe
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Failed to get parent directory"))?
        .join("always-overlay");

    if !overlay_binary.exists() {
        anyhow::bail!("Overlay binary not found at: {}", overlay_binary.display());
    }

    eprintln!("Launching overlay binary: {}", overlay_binary.display());

    // Spawn the overlay process
    let child = std::process::Command::new(&overlay_binary)
        .spawn()
        .context("Failed to launch overlay binary")?;

    // Detach the process so it runs independently
    // The overlay will handle its own lifecycle

    eprintln!("Overlay process launched with PID: {}", child.id());
    Ok(())
}
