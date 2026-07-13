//! Backend abstraction for overlay renderers.
//!
//! This module defines the enum for different rendering backends
//! (X11, Wayland, etc.) so the state logic remains backend-independent.

use super::state::OverlayState;
use anyhow::Result;

/// Available overlay backends.
pub enum OverlayBackend {
    /// X11 backend (works on X11 sessions and XWayland)
    X11(super::renderer::X11Renderer),
    /// Wayland backend with layer-shell support
    Wayland(super::wayland::WaylandRenderer),
}

impl std::fmt::Debug for OverlayBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OverlayBackend::X11(_) => write!(f, "X11Renderer"),
            OverlayBackend::Wayland(_) => write!(f, "WaylandRenderer"),
        }
    }
}

impl OverlayBackend {
    /// Update the overlay state.
    pub async fn update_state(&mut self, state: OverlayState) -> Result<()> {
        match self {
            OverlayBackend::X11(renderer) => renderer.update_state(state).await,
            OverlayBackend::Wayland(renderer) => renderer.update_state(state).await,
        }
    }

    /// Wait for backend-specific events (e.g., window close, exposure).
    pub async fn wait_for_event(&mut self) -> Result<()> {
        match self {
            OverlayBackend::X11(renderer) => renderer.wait_for_event().await,
            OverlayBackend::Wayland(renderer) => renderer.wait_for_event().await,
        }
    }
}

/// Detect the appropriate backend for the current session.
///
/// Returns the backend type that should be used based on environment
/// variables and session type.
pub fn detect_backend() -> BackendType {
    // Check for Wayland session
    if std::env::var("WAYLAND_DISPLAY").is_ok() {
        // Prefer Wayland if available
        // TODO: Check if layer-shell protocol is available
        BackendType::Wayland
    } else if std::env::var("DISPLAY").is_ok() {
        // Fall back to X11
        BackendType::X11
    } else {
        // No display server detected
        BackendType::None
    }
}

/// Available backend types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendType {
    /// X11 backend (works on X11 sessions and XWayland)
    X11,
    /// Wayland backend with layer-shell support
    Wayland,
    /// No backend available (headless or no display server)
    None,
}

impl BackendType {
    /// Returns true if this backend is available on the current system.
    #[allow(dead_code)]
    pub fn is_available(&self) -> bool {
        match self {
            BackendType::X11 => std::env::var("DISPLAY").is_ok(),
            BackendType::Wayland => std::env::var("WAYLAND_DISPLAY").is_ok(),
            BackendType::None => false,
        }
    }

    /// Get a human-readable name for this backend.
    pub fn name(&self) -> &'static str {
        match self {
            BackendType::X11 => "X11",
            BackendType::Wayland => "Wayland",
            BackendType::None => "None",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backend_type_names() {
        assert_eq!(BackendType::X11.name(), "X11");
        assert_eq!(BackendType::Wayland.name(), "Wayland");
        assert_eq!(BackendType::None.name(), "None");
    }

    #[test]
    fn test_backend_type_equality() {
        assert_eq!(BackendType::X11, BackendType::X11);
        assert_ne!(BackendType::X11, BackendType::Wayland);
        assert_ne!(BackendType::Wayland, BackendType::None);
    }

    #[test]
    fn test_backend_detection_does_not_crash() {
        // Test that backend detection doesn't crash
        let backend = detect_backend();
        // We can't assert a specific backend in tests since it depends on the environment
        // but we can verify it returns a valid value
        match backend {
            BackendType::X11 | BackendType::Wayland | BackendType::None => {}
        }
    }

    #[test]
    fn test_backend_availability() {
        // Test that availability checks work correctly
        // We can't set environment variables in tests easily, so we just verify the logic
        let _x11_backend = BackendType::X11;
        let _wayland_backend = BackendType::Wayland;
        let none_backend = BackendType::None;

        // None should never be available
        assert!(!none_backend.is_available());
    }
}
