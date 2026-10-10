# 🏛️ Architecture

> **Tauri v2 — The Modern Stack**

Always is now built as a **Tauri v2** application: a Rust backend handles audio capture, speech-to-text, and input simulation, while a lightweight web frontend (React + Vite + Tailwind) renders the UI in a native WebView container.

---

## Process Layout

```
┌──────────────────────────────────────────────────────────────┐
│                        Tauri Runtime                         │
│  ┌───────────────────────┐    ┌────────────────────────────┐ │
│  │   Rust Backend (Core)  │    │   Web Frontend (Tauri Page)│ │
│  │                       │    │                            │ │
│  │  • Audio Capture      │◄──►│  • React + Vite + Tailwind │ │
│  │  • STT Dispatch       │ IPC│  • Settings UI             │ │
│  │  • Voice Enrollment   │    │  • Overlay HUD             │ │
│  │  • Config / Storage   │    │  • Onboarding              │ │
│  │  • Hotkey Registration│    │                            │ │
│  │  • Paste / Input Sim  │    │  • WebView (Blink engine)  │ │
│  └───────────────────────┘    └────────────────────────────┘ │
│                                                              │
│  ┌──────────────────────────────────────────────────────────┐│
│  │              System Integration Layer                     ││
│  │  • cpal (audio) · rdev (hotkeys) · clipboard              ││
│  │  • CoreGraphics (macOS) · X11/Wayland (Linux)             ││
│  │  • Keyring (macOS) · libsecret (Linux) · CNG (Windows)    ││
│  └──────────────────────────────────────────────────────────┘│
└──────────────────────────────────────────────────────────────┘
```

### Key Design Decisions

- **Single process.** Tauri v2 runs the Rust backend and the WebView in the same process. No UDS, no separate daemon — this eliminates the orphan-watchdog complexity and the IPC serialization overhead.
- **Web frontend as a Tauri Page.** The settings, onboarding, and overlay UI are a React SPA served by the Tauri WebView. Vite is used for development; the production bundle is pre-built and embedded in the Rust binary.
- **Rust backend exposes commands** to the frontend via Tauri's IPC layer. These commands are async-safe and can be called from JavaScript:

```rust
// Rust side
#[tauri::command]
async fn start_listening(handle: tauri::ipc::CommandHandle) {
    // Start audio capture, VAD, STT pipeline
}

// JavaScript side
import { invoke } from '@tauri-apps/api/core';
invoke('start_listening');
```

---

## Component Diagram

```
┌─────────────────────────────────────────────────────────┐
│                    Application Entry                     │
│                   (src-tauri/src/main.rs)                │
│                                                         │
│  ┌─────────────┐  ┌──────────────┐  ┌────────────────┐ │
│  │   tauri::run │  │   Menu Bar   │  │   System Tray  │ │
│  │  (Tauri app) │  │   Integration│  │   Integration  │ │
│  └──────┬───────┘  └──────┬───────┘  └───────┬────────┘ │
│         │                 │                   │           │
│         ▼                 ▼                   ▼           │
│  ┌──────────────────────────────────────────────────────┐ │
│  │                  Command Router                       │ │
│  │  start_listening · stop_listening · toggle_pause      │ │
│  │  get_config · set_config · enroll_voice · paste_text  │ │
│  │  get_status · download_model · list_models            │ │
│  └───────────────────────┬──────────────────────────────┘ │
│                          │                                 │
│        ┌─────────────────┼─────────────────┐              │
│        ▼                 ▼                  ▼              │
│  ┌───────────┐   ┌──────────────┐   ┌──────────────┐     │
│  │  Audio     │   │  STT         │   │  Input       │     │
│  │  Capture   │   │  Pipeline    │   │  Simulation  │     │
│  │  (cpal)    │   │  (Groq/      │   │  (rdev +     │     │
│  │            │   │   whisper    │   │  clipboard)  │     │
│  │  • VAD     │   │              │   │              │     │
│  │  • Buffer  │   │  • Cloud     │   │  • Key events│     │
│  │  • Stream  │   │  • Local     │   │  • Paste     │     │
│  └───────────┘   │  • Filter    │   └──────────────┘     │
│                  └──────────────┘                         │
│                                                            │
│  ┌───────────┐   ┌──────────┐   ┌──────────────────────┐ │
│  │  Config    │   │  Voice   │   │  Model Registry      │ │
│  │  Manager   │   │  Print   │   │  (transcribe-rs)     │ │
│  │  (JSON)    │   │  (ort)   │   │                      │ │
│  │            │   │          │   │  • Parakeet          │ │
│  │  • Groq    │   │  • 3     │   │  • Whisper-CPP       │ │
│  │  • Local   │   │    samples│  │  • Moonshine         │ │
│  │  • API key │   │  • Embed │   │  • SenseVoice        │ │
│  │    (keyring)│  │  ding    │   │  • Canary            │ │
│  └───────────┘   └──────────┘   └──────────────────────┘ │
└─────────────────────────────────────────────────────────┘
```

---

## Data Flow

### Dictation Pipeline

```
┌──────────┐     ┌────────┐     ┌──────────┐     ┌─────────┐     ┌────────┐
│ Microphone│────►│  cpal  │────►│  Silero  │────►│  Groq   │────►│  Text  │
│  (OS API)│     │ Stream │     │   VAD    │     │ Whisper │     │  output│
└──────────┘     └────────┘     └──────────┘     └─────────┘     └────┬───┘
                                                                      │
                                          ┌───────────────────────────┘
                                          ▼
┌──────────┐     ┌──────────────┐     ┌───────────────┐     ┌───────────────┐
│ Grammar  │◄────│  AI Filter   │◄────│  Hallucination│◄────│  Post-process │
│ Correct  │     │  (GPT-OSS)   │     │  Filter       │     │  (spacing,    │
│          │     │              │     │               │     │  punctuation) │
└────┬─────┘     └──────────────┘     └───────────────┘     └───────┬───────┘
     │                                                               │
     ▼                                                               ▼
┌─────────────────────────────────────────────────────────────────────────┐
│                              Output Layer                                │
│  ┌────────────┐   ┌────────────┐   ┌───────────────┐   ┌────────────┐ │
│  │  Clipboard │   │  rdev      │   │  Tauri IPC    │   │  Overlay   │ │
│  │  + paste   │   │  events    │   │  → Webview    │   │  (React)   │ │
│  └────────────┘   └────────────┘   └───────────────┘   └────────────┘ │
└─────────────────────────────────────────────────────────────────────────┘
```

1. **Audio Capture** — `cpal` streams raw PCM from the OS audio API (CoreAudio, ALSA, WASAPI) in shared mode, ensuring no exclusive device access and no volume drop for other apps.
2. **Voice Activity Detection** — Silero V4 ONNX model detects speech vs. silence in real-time, 128 ms buffer chunks.
3. **STT Dispatch** — Routes audio to either:
   - **Groq Cloud:** HTTP POST with streaming response
   - **Local Model:** `transcribe-rs` with whisper, Parakeet, Moonshine, or SenseVoice via ONNX Runtime
4. **Filtering Pipeline** — Rule-based hallucination filter → AI grammar correction (GPT-OSS) → post-processing (spacing, punctuation).
5. **Output** — Pasted via clipboard + simulated key events (rdev). Simultaneously sent to the WebView for the live overlay.

---

## Frontend Architecture

```
web/
├── index.html                    # Entry point
├── vite.config.ts                # Vite + React + Tailwind
├── tailwind.config.js            # Tailwind configuration
├── postcss.config.js             # PostCSS + autoprefixer
├── tsconfig.json                 # TypeScript config
├── package.json                  # Dependencies
└── src/
    ├── main.tsx                   # React DOM mount
    ├── App.tsx                    # Root component — marketing site
    ├── index.css                  # Global styles (Tailwind imports)
    └── components/                # Future: Tauri-page components
```

The web frontend uses:

- **React 19** with JSX transform
- **Vite 7** as the build tool and dev server
- **Tailwind CSS 4** with the `@tailwindcss/vite` plugin
- **TypeScript 5.8** with strict mode

The marketing website (`web/src/App.tsx`) is a standalone Vite SPA. When integrated into Tauri, the Tauri Pages (settings, onboarding) will share the same React/Tailwind stack.

---

## Security Model

| Layer | Mechanism |
|---|---|
| **API keys** | Stored in OS keychain (macOS Keychain, libsecret, CNG) via the `keyring` crate — never in config files or logs |
| **Voiceprint** | Local-only; embeddings stored in `~/.also/voiceprint.bin`, encrypted at rest |
| **Network** | TLS-only for Groq; no other outbound connections |
| **Telemetry** | None. Zero analytics, zero accounts, zero phoning home |
| **Sandbox** | Tauri's CSP headers restrict the WebView; `allowlist` controls which system APIs the frontend can call |

---

## Cross-Platform Features

Tauri v2 provides a unified abstraction layer:

| Feature | macOS | Linux | Windows |
|---|---|---|---|
| **Audio** | cpal → CoreAudio | cpal → ALSA/PipeWire | cpal → WASAPI |
| **Paste** | rdev + paste CLI | xclip / wl-clipboard | rdev + clipboard |
| **Hotkeys** | rdev (global) | rdev (global) | rdev (global) |
| **Overlay** | Native WebView window | X11/Wayland window | Native window |
| **Keychain** | Keychain (keyring) | libsecret (keyring) | CNG (keyring) |
| **Tray** | tauri::TrayIcon | tauri::TrayIcon | tauri::TrayIcon |
| **Updates** | Sparkle (future) | Manual / Flatpak | Windows Update (future) |

---

## Build & Distribution

### Project Structure

```
also/
├── src/                    # Rust backend (Tauri commands)
│   ├── main.rs             # Entry point + Tauri setup
│   ├── audio.rs            # Audio capture via cpal
│   ├── stt.rs              # STT dispatch (cloud/local)
│   ├── filter.rs           # Hallucination + AI filters
│   ├── config.rs           # Config management
│   ├── voiceprint.rs       # Speaker verification
│   └── ...
├── src-tauri/              # Tauri project root
│   ├── Cargo.toml          # Rust dependencies
│   ├── tauri.conf.json     # Tauri configuration
│   ├── build.rs            # Build script
│   ├── capabilities/       # WebView permissions
│   ├── icons/              # App icons
│   ├── gen/                # Generated platform code
│   ├── payloads/           # Tauri payload bundle
│   └── src/                # Tauri-specific Rust code
├── web/                    # Frontend SPA (Vite + React)
│   ├── package.json
│   ├── vite.config.ts
│   └── src/
└── docs/                   # Documentation
```

### Build Commands

```bash
# Development (with live-reload web frontend)
cd src-tauri && cargo tauri dev

# Production build
cd src-tauri && cargo tauri build

# Build for specific platform
cargo tauri build --target x86_64-apple-darwin   # macOS
cargo tauri build --target x86_64-unknown-linux-gnu  # Linux
cargo tauri build --target x86_64-pc-windows-msvc  # Windows
```

### Output Formats

| Platform | Format | Notes |
|---|---|---|
| macOS | `.dmg` (signed & notarized) | Sparkle auto-update wiring |
| Linux | `.deb` / `.rpm` | systemd service included |
| Windows | `.msi` | Administrator install recommended |

---

## Performance

- **Memory:** ~80 MB base (Groq mode), ~850 MB with local model active
- **Latency:** ~200-400 ms end-to-end (Groq cloud), ~500-1200 ms (local)
- **Startup:** < 1 second (daemonless — Tauri single process)
- **Audio buffer:** 128 ms chunks via cpal stream

---

## Future Architecture: Tauri v2 Pages

The full Always app will migrate from the marketing site to Tauri Pages:

```
Tauri Pages:
├── /                    → Onboarding (voice enrollment)
├── /settings/general    → Language, startup, display
├── /settings/models     → STT backend, API key, model management
├── /settings/voice      → Voice enrollment & tuning
├── /settings/shortcuts  → Hotkey configuration
├── /settings/library    → Vocabulary glossary
├── /history             → Transcript history
└── /                    → Live overlay (shown on top of other windows)
```

Each page is a React component, styled with Tailwind, communicating with the Rust backend via `@tauri-apps/api/core`'s `invoke()`.