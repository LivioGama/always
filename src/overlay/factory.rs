//! Backend factory for creating the appropriate overlay renderer.
//!
//! This module provides a factory function that detects the display server
//! and creates the appropriate backend (X11, Wayland, or fallback).

use super::backend::{BackendType, OverlayBackend};
use super::renderer::X11Renderer;
use super::wayland::WaylandRenderer;
use anyhow::{Context, Result};
use tracing::{info, warn};

/// Create the appropriate overlay renderer based on the display server.
///
/// This function detects the available backend and creates the corresponding
/// renderer instance. It prefers Wayland if available, falls back to X11,
/// and returns an error if no suitable backend is found.
pub fn create_renderer() -> Result<OverlayBackend> {
    let backend_type = super::backend::detect_backend();
    info!("Detected backend: {}", backend_type.name());

    match backend_type {
        BackendType::Wayland => {
            // Try Wayland first
            match WaylandRenderer::new() {
                Ok(renderer) => {
                    info!("Using Wayland layer-shell renderer");
                    Ok(OverlayBackend::Wayland(renderer))
                }
                Err(e) => {
                    warn!("Wayland renderer failed: {}, falling back to X11", e);
                    // Fall back to X11 if Wayland fails
                    create_x11_renderer()
                }
            }
        }
        BackendType::X11 => create_x11_renderer(),
        BackendType::None => {
            anyhow::bail!(
                "No display server detected. Set DISPLAY or WAYLAND_DISPLAY environment variable."
            );
        }
    }
}

/// Create an X11 renderer.
fn create_x11_renderer() -> Result<OverlayBackend> {
    info!("Using X11 renderer");
    X11Renderer::new()
        .map(OverlayBackend::X11)
        .context("Failed to create X11 renderer")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backend_detection() {
        // Test that backend detection doesn't crash
        let backend = crate::backend::detect_backend();
        // We can't assert a specific backend in tests since it depends on the environment
        // but we can verify it returns a valid value
        match backend {
            BackendType::X11 | BackendType::Wayland | BackendType::None => {}
        }
    }
}
