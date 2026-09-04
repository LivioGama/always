<div align="center">

# Always

### **The always-on dictation app that doesn't get in your way.**

Speak. It types. Anywhere. Every app.

[![CI](https://github.com/LivioGama/always/actions/workflows/ci.yml/badge.svg)](https://github.com/LivioGama/always/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/LivioGama/always)](https://github.com/LivioGama/always/releases/latest)
[![License](https://img.shields.io/github/license/LivioGama/always)](LICENSE)
[![macOS 14+](https://img.shields.io/badge/macOS-14%2B-black?logo=apple)](#build--install)

</div>

---

## What It Is

An always-on dictation tool for macOS. You speak, and your words are transcribed and typed into whatever application has focus. There is no push-to-talk and no window to click into — the microphone is live, and speech becomes text.

Other dictation tools want to be the center of attention. Always wants to disappear.

```
You speak  ────►  VAD detects speech  ────►  STT transcribes  ────►  Types at cursor
                   (Silero)                (Groq cloud or          (CGEventTap
                                           local ONNX model)       keyboard events)
```

That's the loop. A background daemon captures audio, runs voice activity detection, transcribes speech, and types the result into whatever app has focus. A native menu bar app shows status, manages settings, and stays out of the way.

---

## Key Features

* **Always-on, never-in-the-way.** No push-to-talk, no app to open. The daemon listens continuously with a streaming VAD; transcription is only invoked when speech actually happens.
* **Pastes at cursor in any app.** Text is typed into the focused application via Core Graphics keyboard events — works in any macOS app including ones with custom text engines (Xcode, JetBrains, native textareas).
* **Auto-enter on by default.** Text is typed and sent automatically. Hold **⌘ (Cmd)** while speaking to prevent auto-enter for that utterance.
* **"My Voice" speaker verification.** Teach Always your voice with three quick recording samples (WeSpeaker ResNet34, VoxCeleb-en). Once enrolled and enabled, dictation responds only to you — movies, music, videos, and other people around you are ignored, even while media keeps playing at full volume.
* **Cloud or local transcription.** Use Groq's cloud API (~80 MB RSS, fast, needs API key) or run models locally for offline privacy — Parakeet, Moonshine, Nemotron, Canary, and more. Local models can also be armed as automatic fallback for the cloud backend.
* **Smart filter pipeline.** Hallucination detection catches the "thanks for watching", "[Music]", and repeated-token garbage that speech models occasionally invent from silence. Rule-filtered utterances are still copied to the clipboard so a wrong filter verdict costs a manual ⌘V, not the whole utterance.
* **Vocabulary biasing & corrections.** A glossary of your project's jargon feeds the model's prompt and a post-processor fixes common mistranscriptions. Stops "Kubernetes" from becoming "cuber netties". Corrections learn over time — ambiguous terms are context-resolved, unambiguous ones are deterministic.
* **Microphone conflict handling.** Auto-pauses when another app takes the mic, auto-resumes when it's released (~3 s cooldown). Shows human-readable app names (resolves bundle IDs via LaunchServices). Excludes metering-only apps like the Sound Settings extension and always-on system listeners like Siri/CoreSpeech.
* **Native menu bar app.** Lives in the menu bar, starts in background (settings window not shown on launch). Status overlay HUD, settings, onboarding, Sparkle auto-update — all native SwiftUI.
* **Onboarding.** A 5-step tutorial (Welcome → Permissions → My Voice → Tips → Ready) shown once to new users. App logo, Siri-aware narration (uses `say` CLI, silent if no Siri voice), confirmation dialog when skipping voice enrollment, animated typing demo on the final screen.
* **Open source.** AGPL-3.0. No telemetry, no analytics, no account. Your audio goes to Groq's API and back (cloud mode) or stays entirely on your machine (local mode).

---

## Shortcuts

| Shortcut | Action |
|---|---|
| **Hold ⌘ (Cmd)** while speaking | Prevent auto-enter for this utterance |
| **⌃⌥V** | Paste raw unfiltered text (force-paste the last transcript) |
| **⌃⌥W** | Correct last transcript and feed the vocabulary/glossary |
| **⌃⌥P** | Pause / resume dictation |
| **⌃⌥A** | Toggle auto-enter |
| **⌃⌥X** | Log a manual correction (captures the last transcript for review) |
| **⌃⌥⇧P** | Master pause / mute (survives across all pause sources) |

All shortcuts are configurable via Settings → Shortcuts or `always config set shortcut_<name> <combo>`. Changes take effect immediately — no restart needed.

---

## Architecture

Two processes, one Unix domain socket between them:

```
┌──────────────────────────┐         UDS                ┌────────────────────────┐
│  Rust daemon             │  ──── always.sock ────►    │  Swift menu-bar app    │
│  (always-daemon)         │  ◄──── commands ─────      │  (Always.app)          │
└──────────────────────────┘                            └────────────────────────┘
        │                                                       │
        ▼                                                       ▼
   SoX `rec` audio capture                              Status overlay HUD
   Silero VAD                                           Settings + onboarding
   Speaker verification (WeSpeaker)                     Sparkle auto-update
   STT (Groq cloud / local ONNX)                        "My Voice" enrollment
   Filter pipeline + glossary                           Menu bar item
   CGEventTap keyboard typing
```

* The **daemon** is the source of truth. It runs audio capture (SoX `rec`, 16 kHz mono), VAD (Silero), speaker verification (WeSpeaker), transcription (Groq cloud or local ONNX models), the filter pipeline, and paste injection via Core Graphics keyboard events.
* The **GUI** is a SwiftUI menu bar app. It subscribes to the daemon's event stream and renders the overlay; user actions are forwarded back as commands over the socket.

**Socket:** Unix domain socket at `~/Library/Caches/Always/always.sock`. Messages are newline-delimited JSON; the daemon broadcasts events, the GUI sends commands. The daemon sends its full current state on connect, so a reconnecting GUI is never out of sync.

**Bundle ID:** `com.always.v3`

---

## Models

Always supports a cloud backend and a set of downloadable local models. Local models run offline; the cloud backend is faster to start and needs an API key.

### Groq (cloud)

Fast, lightweight (~80 MB RSS). Requires a Groq API key, managed in Settings → Models with save/delete. API keys live in the OS Keychain — never in env vars, never in config files, never in logs.

### Local STT

Downloadable ONNX models that run entirely on-device. Switching models takes effect on the next utterance. A local model may be armed as fallback for the cloud backend — failures fall back silently but are logged.

Available models include Parakeet, Moonshine (including streaming variants for live preview text), Nemotron 3.5 ASR (multilingual streaming), Canary, GigaAM, SenseVoice, and Cohere. Streaming models expose partial text events so the overlay shows live text while you're still talking.

Daemon memory with a local STT model: **~700–900 MB RSS** (expected — the model weights are loaded in-process). With Groq cloud: **~80 MB RSS** (inference is server-side).

---

## Onboarding

Shown **once**, to someone who has never been through it, and never again. Five steps:

1. **Welcome** — app logo, Siri-aware narration
2. **Permissions** — Microphone and Accessibility are required and gate the Continue button; Input Monitoring is offered but optional (enables global shortcuts, not dictation)
3. **My Voice** — three enrollment recordings, always skippable
4. **Tips** — usage guidance
5. **Ready** — animated typing demo

Narration uses the macOS `say` CLI, which respects the system's Siri voice setting — it's silent if no Siri voice is installed. A confirmation dialog appears when skipping voice enrollment. The window is always dismissable; no step holds a first-run user hostage. The Groq API key is deliberately not part of this flow (local models need none) — it lives in Settings → Models.

Completion is a recorded fact (`onboardingCompletedV1`), not inferred from whether a key exists. Existing installs are never re-shown onboarding.

---

## Settings

Nine panels in the settings window:

| Panel | What |
|---|---|
| **General** | Language, display mode, sound cues, startup behavior |
| **Models** | Cloud/local backend selection, API key management, model downloads |
| **My Voice** | Speaker enrollment, enable/disable gate, threshold tuning |
| **Permissions** | Microphone, Accessibility, Input Monitoring status |
| **Behavior** | Silence window, auto-enter, live preview, adaptive silence, idle pause |
| **Shortcuts** | All keyboard shortcuts, configurable inline |
| **Library** | Vocabulary (glossary) and Snippets (text expansion) tabs |
| **History** | Transcript history |
| **About** | Version, update check, links |

---

## My Voice (Speaker Verification)

Optional. When on, only the enrolled user's voice is transcribed — other people, television, meeting audio, and music are ignored.

**Enrollment** records three passes (normal, lower, louder), each needing 5 s of voice. Each pass is stored as its own embedding plus their combined average. Scoring compares an utterance's embedding against the **best-matching enrolled style**, not only the average.

The model (WeSpeaker ResNet34, VoxCeleb-en) requires 0.5 s of voice minimum — this is the floor on how fast identity can be established. Checks repeat as the user speaks, so a long utterance ends when *they* stop, not when the room goes quiet. Audio that fails verification is discarded before any transcription is paid for.

Every sentence re-proves identity from scratch — there is no trust window. The cost is up to ~2 s of verify wait at the start of each utterance under the gate; the benefit is that the mic only ever engages for the enrolled user.

Threshold is a preference (default 0.40). Your voiceprint embeddings stay local and are never transmitted.

---

## Build & Install

### From source (development)

```bash
./scripts/dev-rebuild.sh            # debug profile (default — transcripts visible in logs)
./scripts/dev-rebuild.sh release    # release profile (transcripts hidden)
```

This is the only supported build path. It kills the running app and daemon, builds Rust and Swift, bundles Sparkle, signs, deploys to `/Applications/Always.app`, relaunches, and **verifies the running processes are newer than the binaries** — exiting non-zero if a stale process is still alive.

**Nothing is "done" until the new build is running.** "Built" and "deployed" are different claims, and only "running" is testable.

### Requirements

* **macOS 14 (Sonoma) or newer** on Apple Silicon
* Rust toolchain
* Xcode / Swift toolchain
* SoX (`rec`) for audio capture

### Tests

```bash
cargo test --features macos --lib -- mic_monitor
```

---

## Logs

```bash
always logs --pretty        # Live-tail with emoji decoration
always logs --console       # Open in macOS Console.app
always logs --path          # Print log directory and exit
always logs --since 1h      # Filter by time (1h, 30m, 1d)
always logs --level error   # Filter by level
```

Logs live at `~/Library/Logs/Always/` (JSON lines, daily rotation). Transcripts are only logged in debug builds, or with `ALWAYS_LOG_TRANSCRIPTS=1` — off by default in release builds for privacy.

---

## Privacy

* **Cloud mode sends audio to Groq.** That's the only network destination. Local mode sends nothing.
* **No telemetry.** The daemon does not phone home, ever.
* **No analytics.** No Sentry, no Mixpanel, no Segment.
* **API keys live in the OS Keychain.** Never in env vars, never in config files, never in logs.
* **Logs do not contain transcript text by default.** Set `ALWAYS_LOG_TRANSCRIPTS=1` to opt in for debugging; off by default in release builds.
* **Daemon runs as your user.** Not root. Not LaunchDaemon.
* **Speaker verification data stays local.** Your voiceprint embeddings are stored locally and never transmitted.

---

## Documentation

| Document | What |
|----------|------|
| [`SPEC.md`](SPEC.md) | Normative product specification — the behaviour the code answers to |
| [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) | Process layout, UDS protocol, resilience contract |
| [`docs/DEVELOPMENT.md`](docs/DEVELOPMENT.md) | Build, test, contribute |
| [`docs/RELEASE.md`](docs/RELEASE.md) | How releases are cut |
| [`docs/TROUBLESHOOTING.md`](docs/TROUBLESHOOTING.md) | Microphone permission, common issues |
| [`SECURITY.md`](SECURITY.md) | Reporting vulnerabilities, embargo policy |
| [`CHANGELOG.md`](CHANGELOG.md) | Versioned changelog |
| [`CONTRIBUTING.md`](CONTRIBUTING.md) | PR + commit conventions |

---

## License

[AGPL-3.0](LICENSE). You can use, modify, and distribute Always, including commercially — but if you modify it and distribute it (including as a network service), you must share your modifications under the same license. See the full license text for details.

---

<div align="center">

**Built by humans who got tired of typing.**

[Issues](https://github.com/LivioGama/always/issues) ·
[Discussions](https://github.com/LivioGama/always/discussions) ·
[Releases](https://github.com/LivioGama/always/releases)

</div>
