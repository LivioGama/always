# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed
- **status overlay:** switching Settings → Status Overlay between Normal, Compact and Hidden now changes the indicator already on screen, instead of waiting for the next one (or a restart). Picking Normal or Compact shows a short preview, and the setting now says where the indicator appears: centred near the bottom of the screen your pointer is on, above the Dock.

## [0.1.0] - 2026-10-15

The first public release, installable on macOS, Linux and Windows with one command. It also ships everything listed under 0.0.2 below, which was never published on its own.

### Added
- **updates:** one updater for macOS, Linux and Windows replaces Sparkle. Always checks GitHub Releases at start and once a day and tells you when a newer version exists; it never installs by itself. `always update` (or the macOS **Install update** button) downloads the release and verifies every file, including the installer, against a minisign key built into the app before installing; `always update --check` only reports. Turn the check off with `always config set update_check false` or `ALWAYS_UPDATE_CHECK=0`. ([#122](https://github.com/LivioGama/always/pull/122))
- **install:** one command installs Always: `curl -fsSL https://raw.githubusercontent.com/LivioGama/always/main/scripts/install-always.sh | bash` on macOS and Linux, `irm https://raw.githubusercontent.com/LivioGama/always/main/scripts/install.ps1 | iex` on Windows. It picks the build for your platform and verifies its SHA-256 before installing. ([#108](https://github.com/LivioGama/always/pull/108))
- **release:** every release now ships macOS arm64, Linux x86_64 and aarch64 (tar.gz, deb, rpm) and Windows x86_64 builds, each with a `.sha256` checksum and a build provenance attestation you can check with `gh attestation verify`. ([#108](https://github.com/LivioGama/always/pull/108))
- **vocabulary:** on its first launch on macOS, Always imports your installed app names, your macOS Text Replacements and the dictionaries you built in Wispr Flow and SuperWhisper into its own, so it spells your words right from day one. ([#113](https://github.com/LivioGama/always/pull/113))

### Changed
- **website:** a redesigned landing page with a live dictation demo, a to-scale timeline of one sentence, and the install command for your operating system. ([#108](https://github.com/LivioGama/always/pull/108))

### Removed
- **install:** the DMG download and Homebrew cask instructions are gone; install with the one-line installer or the release archives. The macOS app is ad-hoc signed and not notarized, and the installer-delivered app opens without a quarantine prompt. ([#108](https://github.com/LivioGama/always/pull/108))

### Fixed
- **audio:** a capture helper that dies on startup is now always detected, so Always falls back to the plain recorder instead of sometimes listening to nothing. ([#108](https://github.com/LivioGama/always/pull/108))
- **my voice:** first-time voice enrollment now shows "Preparing voice model…" until recording has really started, and the speaker model downloads in the background at startup, so you no longer speak into a silent wait that ends in "not enough speech captured". ([#108](https://github.com/LivioGama/always/pull/108))
- **clipboard:** curly quotes, dashes, accents and other non-ASCII text are no longer pasted as mojibake ("it‚Äôs") when the daemon runs without a UTF-8 locale. ([#108](https://github.com/LivioGama/always/pull/108))
- **windows:** the daemon builds on Windows again, clipboard read and write work there, and the administrator check reports correctly. ([#108](https://github.com/LivioGama/always/pull/108))

## [0.0.2] - 2026-10-15

### Fixed
- **No dictated words lost** — at chunk seams (quiet-point cuts + 0.6 s seam context, duplicate words merged), in a quiet last sentence, to the grammar pass, or when the last chunk's STT pass fails (committed chunks are still pasted; the tail audio is saved and recovered by the Parakeet preview when available).
- **My Voice speaker gate never destroys the user** — early drop needs ~1 s of continuously non-matching audio; an unavailable embedder fails open; phone/reel speech around the user's words is trimmed before STT.
- **Master mute** fires once per Fn press (rdev no longer double-delivers it).
- **Recorder** is no longer recycled every ~100 s while media plays (24 h hygiene cap instead).
- Boot with Groq selected and no key falls back to on-device Apple STT instead of blocking every utterance.

### Added
- Live streaming Apple STT (strictly on-device): partials while speaking, near-instant final.
- Echo-cancelling capture helper (`always-capture`, VPIO) with `rec` fallback; engaged only while other audio plays.
- Instant live preview (Apple + Parakeet), deterministic "Always Fast" postprocess mode, voice-edit commands on the last paste ("scratch that", "change X to Y").
- Onboarding: setup mode for voice testing, HUD suppressed during setup, voiceprint enables the gate on completion.


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
