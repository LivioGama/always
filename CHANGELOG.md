# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Voice-to-text daemon (Groq Whisper STT) with Silero VAD, hallucination filter, and UDS event streaming to the Swift menu-bar app.
- Manual correction capture: ⌃⌥X hotkey plus optional passive clipboard mode → `~/.always/glossary.json` (`always corrections list/approve/reject/clear/capture`).
- Settings sidebar panels: General, Models, Permissions, Behavior, Shortcuts, Vocabulary, History, About.
- Sparkle auto-update wiring, signed release pipeline (DMG, notarization, cosign, SLSA, Homebrew tap PR).
- `.github/dependabot.yml`; CI concurrency control and SwiftPM cache.

### Changed
- CI: `cargo clippy --all-targets --all-features --locked -D warnings`, blocking `cargo audit`, runner pinned to `macos-14`.
- `SECURITY.md`: disclosure email, supported-versions table, embargo timeline.
- `build.sh`: sync bundle version from `Cargo.toml`, bundle integrity checks, rsync deploy to preserve TCC grants.

### Fixed
- Case-insensitive bundle collision (`Always` vs `always`) — daemon ships as `always-daemon`.
- Vocabulary false positives (`Zed` in `analyzed`) via word-boundary replacement.
- Paste-in-flight lock leak, atomic config/glossary writes, SQLite busy timeout, UDS client resilience.
- STT language persistence, model UX, overlay visibility, and listening latency regressions.

## [0.0.1] - 2026-06-06

First tagged build. No prior public releases.
