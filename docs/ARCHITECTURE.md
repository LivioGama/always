# 🏛️ Architecture

## Process layout

```
┌──────────────────────────┐         UDS                ┌────────────────────────┐
│  Rust daemon (`always`)  │  ──── always.sock ────►    │  Swift menu-bar app    │
│  (CLI binary)            │  ◄──── commands ─────      │  (Always.app)          │
└──────────────────────────┘                            └────────────────────────┘
        │                                                       │
        ▼                                                       ▼
   SoX `rec` (audio)                                  StatusOverlay (HUD)
   Groq Whisper API                                   SettingsWindow
   pbcopy + CGEventTap                                Onboarding
```

* The daemon runs voice activity detection, calls Groq Whisper, applies
  hard / AI / hallucination filters, post-processes the text, and pastes
  via `pbcopy` + Core Graphics keyboard events.
* The Mac app subscribes to a UDS event stream and renders an overlay.
  It also forwards user commands (`TogglePause`, `ToggleAutoEnter`) the
  other direction so the daemon stays the single owner of state.

## UDS protocol (v1)

Each line is a JSON object. Daemon → app uses `DaemonEvent` (tagged
enum); app → daemon uses `DaemonCommand`. The first frame on every
connection is `Hello { version: u32 }` so a stale Mac app refuses to
talk to a daemon it was not built against. See
[`src/always/event.rs`](../src/always/event.rs) and
[`Always/Sources/Always/Services/UDSClient.swift`](../Always/Sources/Always/Services/UDSClient.swift).

```
Daemon                                 Mac app (UDSClient)
  │                                          │
  │  ── Hello { version: 1 } ───────────────►│
  │                                          │ verifies version
  │  ── Paused / Resumed ───────────────────►│ initial state
  │  ── AutoEnterEnabled / Disabled ───────►│
  │  ── ListeningStarted ───────────────────►│
  │                                          │
  │  ◄── { "type":"TogglePause" } ───────────│ user clicked menu item
  │                                          │
  │  validates + rate-limits, executes       │
  │                                          │
  │  ── Paused ─────────────────────────────►│ broadcast new state
  │  ── Heartbeat (every 5s) ───────────────►│ keeps watchdog happy
  │                                          │
  │  ── VoiceActivityDetected ──────────────►│ overlay turns red
  │  ── TranscribingStarted ────────────────►│ overlay turns purple
  │  ── TranscriptFinal { text } ───────────►│ HUD flash + log line
```

Bumping `PROTOCOL_VERSION` in `event.rs` requires bumping
`UDS_PROTOCOL_VERSION` in `UDSClient.swift` in the same PR; the
`tests/uds_protocol_test.rs` and `AlwaysTests::testProtocolVersionMatchesDaemon`
tests pin both.

## Resilience contract

* **Groq STT**: 3 retries on 429 / 5xx with exponential backoff
  (200 → 400 → 800 ms + jitter). After 3 consecutive failures the
  process-wide circuit breaker opens for 60s, surfacing
  `SttError::Unavailable` so the daemon falls through to the rule-based
  fallback in [`ai_filter::evaluate_fallback`](../src/always/ai_filter.rs)
  rather than stalling on each utterance. Contract pinned in
  [`tests/stt_resilience_test.rs`](../tests/stt_resilience_test.rs).
* **Mutexes** in the audio hot path use `parking_lot` (poison-free) and
  the speculative-transcription thread is wrapped in `catch_unwind` so
  one bad utterance can't stall every following one.
* **Orphan watchdog** in `uds_server.rs` exits the daemon if no UDS
  client connects for `ORPHAN_TIMEOUT_SECS` — prevents a stale daemon
  outliving the Mac app.

## Cross-platform layout

* `feature = "macos"` (default): full GUI overlay, `pbcopy`+CGEventTap
  paste, `rdev` global hotkeys (P2.2 will swap this for native
  `CGEventTap`), `oslog` Console.app integration.
* `feature = "linux"`: builds the daemon CLI only. Audio capture +
  paste + key listener return `NotImplemented`. The Linux Docker image
  uses ALSA via SoX `rec`.
* `feature = "windows"`: same as Linux — stub-only for now.

CI builds all three on every PR.

## Core Components

- `src/always/vad.rs` — Voice activity detection & recording
- `src/always/audio.rs` — Optimized audio processing with memory pooling
- `src/always/filter.rs` — Two-tier intelligent filtering
- `src/always/config.rs` — Configuration management
- `src/always/event_loop.rs` — Main daemon processing loop
- `src/always/uds_server.rs` — Unix domain socket event/control stream for the UI
- `Always/Sources/Always/` — macOS menu bar app, onboarding, settings, and overlay UI

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
