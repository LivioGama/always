# 🏛️ Architecture

## Core Components

- `src/always/vad.rs` — Voice activity detection & recording
- `src/always/audio.rs` — Optimized audio processing with memory pooling
- `src/always/filter.rs` — Two-tier intelligent filtering
- `src/always/config.rs` — Configuration management
- `src/always/event_loop.rs` — Main daemon processing loop
- `src/always/uds_server.rs` — Unix domain socket event/control stream for the UI
- `AlwaysApp/Sources/AlwaysApp/` — macOS menu bar app, onboarding, settings, and overlay UI

## Performance Optimizations

- 🧠 **Memory Pooling** — Reused audio buffers
- 🌐 **Connection Reuse** — HTTP client pooling
- ⚡ **Optimized VAD** — Fast energy detection
- 🎙️ **Efficient Recording** — Persistent SoX processes
- 🎯 **Smart Filtering** — Two-stage filtering
- ⏱️ **Speculative Transcription** — Starts transcription during tentative silence and reuses the result if speech does not resume

## Vocabulary System

- 📚 Base vocabulary — Common programming terms
- 🔍 Context vocabulary — Project-specific terms
- 🧠 Learning system — Groq-powered corrections
- 🎨 Pattern matching — Code patterns & file paths

## UI and Control Plane

The Swift app talks to the Rust daemon over a Unix domain socket. The daemon
broadcasts events such as listening, voice activity, pause/resume, auto-enter,
and transcription state. The menu bar app and overlay subscribe to those events
instead of polling the legacy state file.

Keyboard shortcuts are configured through `shortcut_pause` and
`shortcut_auto_enter`. Changes are read when the daemon starts, so users should
restart the daemon after editing shortcuts in Settings.

## Current Release Note

The app currently builds and tests in Swift debug mode during local development.
Release-mode verification should remain part of the stabilization roadmap until
the overlay regression mentioned in `README.md` is fully closed.
