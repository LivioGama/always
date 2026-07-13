<div align="center">

# Always

### **The voice-to-text daemon that doesn't get in your way.**

Speak. It pastes. Anywhere. Every app.

[![CI](https://github.com/rtk-ai/always/actions/workflows/ci.yml/badge.svg)](https://github.com/rtk-ai/always/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/rtk-ai/always)](https://github.com/rtk-ai/always/releases/latest)
[![License](https://img.shields.io/github/license/rtk-ai/always)](LICENSE)
[![macOS 14+](https://img.shields.io/badge/macOS-14%2B-black?logo=apple)](#installation)
[![Linux](https://img.shields.io/badge/Linux-CLI--only-orange?logo=linux)](#installation)

</div>

---

## 🎯 What It Is For

You don't dictate into a window. You don't open an app. You don't press a hotkey. You **speak**, and your words appear in whatever app you were already typing in — Slack, VS Code, the browser address bar, your terminal — exactly where the cursor is.

Other dictation tools want to be the center of attention. Always wants to disappear.

```
You speak  ────►  VAD detects speech  ────►  Whisper transcribes  ────►  Pastes at cursor
   ~0.0s              ~0.05s                    ~0.3-0.6s                  ~0.65s end-to-end
```

That's it. That's the loop. Background daemon, native menubar overlay that flashes for ~200 ms, paste lands, you keep working.

---

## 🌟 What Makes It Special

* **Always-on, never-in-the-way.** No push-to-talk, no app to open. The daemon listens 24/7 with a streaming VAD; Whisper is only invoked when speech actually happens. Your CPU + Groq quota are not on fire.
* **End-to-end latency under 700 ms.** Speculative transcription kicks off Whisper at *tentative* silence so the result is usually back before final silence is confirmed.
* **Zero-config paste.** Pastes wherever the cursor is via Core Graphics keyboard events — works in *any* macOS app including ones with custom text engines (Xcode, JetBrains, native textareas).
* **Smart filter pipeline.** Three layers (hard blocklist → AI filter → Whisper hallucination detector) catch the "thanks for watching", "[Music]", "*upbeat music*" garbage Whisper occasionally invents from silence.
* **"My Voice" speaker verification.** Teach Always your voice with three quick recording samples. Once enrolled and enabled, dictation responds ONLY to you — movies, music, videos, and other people around you are ignored, even while media keeps playing at full volume.
* **Glossary biasing.** A `glossary.json` of your project's jargon feeds Whisper's 224-token prompt and a post-processor fixes common mistranscriptions. Stops "Kubernetes" from becoming "cuber netties".
* **Custom vocabulary import.** `always vocab import` mines real user data: macOS Text Replacements (System Settings → Keyboard), every app in `/Applications`, your SuperWhisper recording history (statistical outliers from past transcripts), and MacWhisper's Replacements list. Re-runs **merge** with the existing glossary, never overwrite — your hand-tuned entries always survive.
* **Self-improving glossary.** Hit `⌃⌥X` after correcting a botched transcript and the daemon adds the `(wrong → right)` pair to your glossary automatically. Optional passive mode watches the clipboard for re-copies and queues candidate corrections for review.
* **Native menubar app.** Status overlay, settings window, "Check for Updates…" via Sparkle, "Open today's log" via Terminal — all native SwiftUI, code-signed, notarized, sandboxed.
* **Configurable via GUI *or* CLI.** Every setting reachable from both. Pair it with your dotfiles. `always config preset low|normal|high` for one-shot environment changes.
* **Production-grade.** Tracing-based structured logs, parking_lot hot-path locks, exponential-backoff + circuit-breaker on the Groq client, Sparkle auto-update, Homebrew tap, signed DMG, SBOM, cosign signatures, SLSA Level 3 provenance.
* **Open source.** Apache-2.0. No telemetry, no analytics, no account, no SaaS. Your audio goes to Groq's API and back; everything else stays on your laptop.

---

## 📊 How It Compares

| | **Always** | Wispr Flow | SuperWhisper | MacWhisper | Whispering | Talon |
|---|:---:|:---:|:---:|:---:|:---:|:---:|
| **Always-on (no hotkey)** | ✅ | ❌ push-to-talk | ❌ push-to-talk | ❌ push-to-talk | ❌ push-to-talk | partial |
| **Pastes at cursor in any app** | ✅ | ✅ | ✅ | ❌ window only | ❌ window only | ✅ |
| **Open source** | ✅ Apache-2.0 | ❌ | ❌ | ❌ | ✅ MIT | partial |
| **Local-only option** | roadmap | ❌ | ✅ | ✅ | ✅ | ✅ |
| **Bundle size** | **~30 MB** | ~150 MB | ~180 MB | ~250 MB | ~80 MB | ~200 MB |
| **Memory at idle** | **~25 MB** | ~150 MB | ~200 MB | ~300 MB | ~120 MB | ~400 MB |
| **End-to-end latency** | **~650 ms** (Groq) | ~800 ms | ~600 ms (local large) / 1500 ms+ | ~1200 ms | varies | varies |
| **Hallucination filter** | ✅ 3-layer | ❌ | partial | ❌ | ❌ | ❌ |
| **Custom glossary** | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| **Glossary auto-import** | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ |
| **Speaker verification** | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ |
| **Free / no subscription** | ✅ | ❌ $12/mo | ❌ $90 + Pro | ❌ $59 + Pro | ✅ | ✅ |
| **CLI scriptable** | ✅ full | ❌ | ❌ | ❌ | ❌ | ✅ |
| **Linux daemon** | ✅ CLI | ❌ | ❌ | ❌ | ❌ | partial |
| **Auto-update (Sparkle)** | ✅ | proprietary | ✅ | ✅ | manual | ✅ |
| **Signed + notarized** | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |

> Bundle / memory numbers are best-effort measurements at the time of writing on macOS 14 arm64. Latency varies with network. We measured Groq's `whisper-large-v3-turbo` for 1-2 s utterances.

---

## 🚀 Quick Start

### macOS (recommended)

```bash
# Homebrew tap (when v1.0 ships)
brew install --cask rtk-ai/tap/always

# Or download the signed + notarized DMG from Releases:
# https://github.com/rtk-ai/always/releases/latest
```

Requires **macOS 14 (Sonoma) or newer** on Apple Silicon. The bundle is ~30 MB. Sparkle auto-update keeps you on the latest version.

### From source

```bash
cargo install --git https://github.com/rtk-ai/always always
cargo install --git https://github.com/rtk-ai/always always --no-default-features --features linux
```

The source install ships **only the `always` daemon binary**. The Mac menubar app + overlay live in the `Always.app` bundle — install the cask above to get them.

### Linux

The daemon builds and runs on Linux with `--features linux`, but global keyboard shortcuts and clipboard paste are stubs (planned). Voice → Whisper transcription works, suitable for piping into your own automation.

```bash
docker compose up
# or:
cargo install --git https://github.com/rtk-ai/always always --no-default-features --features linux
```

See `Dockerfile` for the official image.

### Windows

Same story as Linux — `--features windows` builds the CLI daemon; keyboard listener + clipboard paste are stubs. PRs welcome.

---

## 🛠 First-Run Setup

```bash
# 1. Get a free Groq API key — https://console.groq.com/keys
always config set groq_api_key sk_…

# 2. (Optional) Tune for your environment
always config preset normal     # default — quiet office
always config preset high       # quieter rooms / soft voice
always config preset low        # cafés, open plan, fans

# 3. Start the daemon
always start

# 4. Speak. Anywhere. Your text appears.
```

That's it. The status bar icon turns red when you speak, purple when Whisper is transcribing, then gone. Your text is at the cursor.

---

## 🎙️ "My Voice" Speaker Verification

Teach Always your voice so it only responds to you:

```bash
# Check enrollment status
always voice status

# Record three samples (normal, lower, louder)
always voice record normal
always voice record lower
always voice record louder

# Enable the voice gate
always voice enable

# Disable the gate (voiceprint kept)
always voice disable

# Delete your voiceprint
always voice clear
```

Or use the **My Voice** panel in the macOS app settings for a guided enrollment experience with real-time audio level meters and progress indicators.

---

## 📋 CLI Reference

### Lifecycle

```bash
always start                # Start daemon in background
always stop                 # Stop daemon
always status               # Show pid + log path
always run                  # Run in foreground (debugging)
always toggle-pause         # Mute mic without killing the daemon
always toggle-auto-enter    # Toggle "press Return after paste"
```

### Configuration

```bash
always config show                              # All current values
always config preset <low|normal|high>          # Apply Mic Sensitivity preset
always config set <key> <value>                 # Tweak one setting
always config reset                             # Revert to defaults
always config delete-key <groq_api_key|deepgram_api_key>   # Wipe from Keychain
```

Full settings reference → [`docs/advanced_settings.md`](docs/advanced_settings.md).

### Logs

```bash
always logs --pretty        # Live-tail with emoji decoration
always logs --console       # Open in macOS Console.app
always logs --path          # Print log directory and exit
always logs --since 1h      # Filter by time (1h, 30m, 1d)
always logs --level error   # Filter by level
```

Logs live at `~/Library/Logs/Always/always.YYYY-MM-DD` (macOS) or `$XDG_STATE_HOME/always/` (Linux), JSON-lines format. Daily rotation, 7-day retention.

### Vocabulary

```bash
always vocab import         # Pull macOS Text Replacements + SuperWhisper + MacWhisper + installed apps
always vocab extract        # Extract from current project (planned)
```

### Corrections

```bash
always corrections list                # Pending entries awaiting review
always corrections approve <UUID>      # Apply one to ~/.always/glossary.json
always corrections reject <UUID>       # Drop one
always corrections clear               # Drop everything
always corrections capture             # Manually run the active capture flow (no hotkey needed)
```

See [`docs/advanced_settings.md#manual-correction-capture`](docs/advanced_settings.md) for the hotkey workflow and the opt-in passive watcher.

### Daemon Arguments

| Flag | Default | Description |
|------|---------|-------------|
| `-l, --lang <CODE>` | `en` | Language code passed to Whisper |
| `-t, --timeout <SECS>` | `30` | Hard cap on a single utterance recording |
| `-s, --silence <SECS>` | `2.0` | Silence required to end an utterance |
| `--auto-enter` | off | Press Return after every paste |

Pass to `start` or `run`:

```bash
always start --silence 1.5 --auto-enter
```

---

## ⚙️ Default Settings

> See **[docs/advanced_settings.md](docs/advanced_settings.md)** for what every knob means and when to move it.

| Setting | Default | Why |
|---------|:---:|------|
| Mic Sensitivity preset | **Normal** | Tuned for typical office voice |
| `stt_energy_threshold` | `0.012` | Low enough to catch speech, high enough to ignore AC hum |
| `hear_energy_threshold` | `0.001` | Pre-VAD overlay flash hint |
| `stt_silence` | `2.0 s` | Long enough to tolerate natural mid-sentence pauses |
| `stt_cooldown_ms` | `150 ms` | Debounces noisy VAD edges without slowing rapid commands |
| `silero_threshold` | `0.5` | Canonical Silero VAD default |
| `speaker_gate_enabled` | `false` | "My Voice" speaker verification gate |
| `speaker_gate_threshold` | `0.5` | Speaker similarity threshold (0.0-1.0) |
| `shortcut_log_correction` | `ctrl+alt+x` | Hotkey to capture a manual correction |
| `passive_correction_capture` | `false` | Opt-in clipboard watcher for re-copies |

---

## 🏗️ Architecture

Two processes, one Unix domain socket between them:

```
┌──────────────────────────┐         UDS                ┌────────────────────────┐
│  Rust daemon (always)    │  ──── always.sock ────►    │  Swift menu-bar app    │
│  (CLI binary, ~23 MB)    │  ◄──── commands ─────      │  (Always.app)          │
└──────────────────────────┘                            └────────────────────────┘
        │                                                       │
        ▼                                                       ▼
   SoX `rec` audio                                    StatusOverlay HUD
   Silero VAD                                         Settings + onboarding
   Groq Whisper                                       Sparkle auto-update
   Speaker verification                              "My Voice" enrollment
   pbcopy + CGEventTap
```

* The **daemon** is the source of truth. It runs the audio capture, VAD, Whisper, filter pipeline, speaker verification, and paste injection.
* The **Mac app** is a thin SwiftUI client. It subscribes to the daemon's event stream and renders an overlay; user actions are forwarded back as `DaemonCommand` JSON lines.

Wire format is versioned (`Hello { version: 1 }` is the first frame on every connection). Mismatched daemon/app versions disconnect cleanly instead of silently corrupting.

Full details: [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).

---

## ⚡ Performance

Measured on M2 Pro, Sonoma 14.5, fiber → Groq EU region, 1-2 s utterances:

| Phase | p50 | p95 |
|-------|:---:|:---:|
| VAD silence detection | 30 ms | 60 ms |
| WAV encode (in-memory) | < 1 ms | 2 ms |
| HTTP/2 + multipart upload | 60 ms | 180 ms |
| Whisper inference (Groq) | 280 ms | 480 ms |
| Filter + post-process | 5 ms | 15 ms |
| `pbcopy` + Cmd+V dispatch | 8 ms | 25 ms |
| **End-to-end (silence → paste)** | **~650 ms** | **~1.0 s** |

Speculative transcription cuts another ~400 ms on cooperative speech patterns where the user pauses briefly mid-utterance.

Daemon RAM at idle: **~25 MB**. Daemon RAM during active transcription: **~45 MB peak**. Whisper runs server-side at Groq, not in-process — your laptop fan stays off.

---

## 🔐 Privacy

* **Audio is sent to Groq.** That's the only network destination. Groq's privacy policy: https://wow.groq.com/privacy-policy.
* **No telemetry.** The daemon does not phone home, ever.
* **No analytics.** No Sentry, no Mixpanel, no Segment, no anything.
* **API keys live in the OS Keychain.** Never in env vars, never in config files, never in logs.
* **Logs do not contain transcript text by default.** Set `ALWAYS_LOG_TRANSCRIPTS=1` to opt in for debugging; off by default in release builds.
* **Daemon runs as your user.** Not root. Not LaunchDaemon. Single user.
* **Speaker verification data stays local.** Your voiceprint embeddings are stored locally and never transmitted.

---

## 🏭 Production-Grade Engineering

This is not a weekend hack — it's built like infrastructure.

* **CI gate.** Every PR runs `cargo fmt --check`, `cargo clippy --all-targets --all-features --locked -- -D warnings`, full test suite, `cargo audit`, `cargo deny check`, `cargo machete`, code coverage upload to Codecov, CodeQL static analysis, Dependabot.
* **Tests.** 100+ Rust tests covering hot-path locks, retry/backoff, circuit breaker, UDS protocol, hallucination detection, vocabulary application, audio buffer pool. Plus 21 Swift tests covering UDS decoding, settings, sensitivity-preset round-trip.
* **Resilience.** Groq client retries 429/5xx three times with exponential backoff + jitter. Three consecutive failures opens a circuit breaker for 60 s, falling back to a rule-based filter so the daemon doesn't stall during an outage. parking_lot mutexes everywhere on the audio hot path so a panic never poisons a lock. Speculation thread wrapped in `catch_unwind`.
* **Observability.** `tracing` + `tracing-subscriber` JSON to file, pretty stderr in foreground, `oslog` integration for Console.app on macOS. Daily log rotation, 7-day retention.
* **Supply chain.** Signed releases via `cosign` (keyless OIDC), SLSA Level 3 provenance via the `slsa-github-generator` reusable workflow, CycloneDX SBOM published with every release, license policy in `deny.toml`.
* **Distribution.** Signed + notarized DMG, Homebrew cask, Sparkle auto-update with EdDSA-signed appcast, and source install via `cargo install --git`.

Full self-assessment: **[docs/ASSESSMENT.md](docs/ASSESSMENT.md)**.

---

## 📚 Documentation

| Document | What |
|----------|------|
| [`docs/advanced_settings.md`](docs/advanced_settings.md) | Every preference, what it does, when to move it |
| [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) | Process layout, UDS protocol, resilience contract |
| [`docs/DEVELOPMENT.md`](docs/DEVELOPMENT.md) | Build, test, contribute |
| [`docs/RELEASE.md`](docs/RELEASE.md) | How releases are cut |
| [`docs/TROUBLESHOOTING.md`](docs/TROUBLESHOOTING.md) | Microphone permission, common issues |
| [`SECURITY.md`](SECURITY.md) | Reporting vulnerabilities, embargo policy |
| [`CHANGELOG.md`](CHANGELOG.md) | Versioned changelog |
| [`CONTRIBUTING.md`](CONTRIBUTING.md) | PR + commit conventions |
| [`docs/ASSESSMENT.md`](docs/ASSESSMENT.md) | Production-readiness scorecard |

---

## 🗺️ Roadmap

* [ ] Native audio capture via `cpal` — drop the SoX subprocess
* [ ] Local Whisper option — privacy-first mode, no Groq round-trip
* [ ] CGEventTap-native keyboard listener — drop unmaintained `rdev`
* [ ] Linux ALSA backend
* [ ] Windows clipboard / hotkey support
* [ ] In-line corrections UI ("did you mean…")
* [ ] Dictation grammar / commands ("delete that", "new line")

PRs welcome. Bug reports welcome. Star the repo if you're using it daily — that's how we measure whether the work is paying off.

---

## 📄 License

[Apache-2.0](LICENSE). Use it commercially, fork it, do whatever — we just ask that you keep the notice and don't sue us.

---

<div align="center">

**Built by humans who got tired of typing.**

[Issues](https://github.com/rtk-ai/always/issues) ·
[Discussions](https://github.com/rtk-ai/always/discussions) ·
[Releases](https://github.com/rtk-ai/always/releases)

</div>
