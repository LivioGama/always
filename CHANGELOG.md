# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- **Tauri v2 migration** — Application now uses Tauri v2 runtime: a single-process architecture with a Rust backend and a web frontend (React + Vite + Tailwind) in a native WebView container. Eliminates UDS daemon, orphan watchdog, and cross-process IPC.
- **Marketing website** (`web/`) — Professional landing page with Hero, Features (7-card grid), How It Works (3-step), Download (platform selector), Screenshots placeholders, and Footer. Built with React 19, Vite 7, Tailwind CSS 4, TypeScript 5.8.
- **Platform-specific install instructions** — README.md updated with installation commands for macOS (Homebrew tap + DMG), Linux (DEB/RPM), and Windows (MSI). Includes post-install permission steps for each platform.
- **Keyboard shortcuts table** — All six shortcuts documented in README.md with Linux (Super/Ctrl+Alt) and Windows (Win/Ctrl+Alt) modifier translations.
- **System requirements** — Documented minimum specs (4 GB RAM, 80 MB disk, platform version requirements) in README.md and the Download section.
- **docs/SETUP.md** — Comprehensive platform-specific setup guide covering:
  - **macOS:** DMG installation, Homebrew tap, microphone/input monitoring/accessibility permissions, quarantine override
  - **Linux:** DEB/RPM install, systemd service (`also-daemon`), udev rules for audio device access, ALSA/PipeWire/PulseAudio config
  - **Windows:** MSI install with admin privileges, Windows 10/11 microphone privacy settings
  - **Common:** API key configuration, local model download, log file locations, verbose logging
- **Updated docs/ARCHITECTURE.md** — Complete rewrite documenting Tauri v2 architecture:
  - Single-process design (Rust backend + WebView frontend)
  - Component diagram with cpal, Silero VAD, STT dispatch, AI filter, rdev
  - Full data flow: microphone → cpal → VAD → Groq/local STT → filter → paste/overlay
  - Frontend structure (web/ with Vite + React + Tailwind)
  - Security model (keychain storage, local-only voiceprint, TLS-only network, zero telemetry)
  - Cross-platform abstraction matrix (audio, paste, hotkeys, overlay, keychain)
  - Build & distribution commands and output formats
  - Tauri v2 Pages roadmap for full app migration
- **Data flow documentation** — Complete dictation pipeline documented with component diagram in ARCHITECTURE.md.

### Changed
- README.md rewritten with cross-platform focus: install instructions, shortcuts table, features list, documentation links, and system requirements added.
- Architecture documentation migrated from Swift-daemon + UDS model to Tauri v2 single-process model.
- CI output updated to document Tauri build targets (macOS dmg, Linux deb/rpm, Windows msi).

### Fixed
- Cross-platform permission documentation ensures users know exactly what to grant on each platform — previously only macOS permissions were well-documented.

## [0.0.1] - 2026-06-06

First tagged build. No prior public releases.
