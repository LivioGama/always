# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- `ASSESSMENT.md` at repo root — production-readiness scorecard with per-criterion ratings
- `.github/dependabot.yml` — weekly Cargo, GitHub Actions, and Swift Package Manager updates
- Concurrency control on CI workflow (auto-cancel superseded runs)
- Swift Package Manager build cache in CI

### Changed
- `cargo clippy` in CI now runs `--all-targets --all-features --locked -- -D warnings` (was `--release` only, hiding test/example lints)
- `cargo audit` in CI is now blocking (was `continue-on-error: true`, allowing silent vulnerabilities)
- CI runner pinned to `macos-14` (was `macos-latest`)
- `SECURITY.md`: real disclosure email, supported-versions table, embargo timeline (48h ack, 30/60/90-day patch by severity)
- Lint `clippy::print_stdout` / `clippy::print_stderr` warned at `lib.rs` root; allowed only on intentional CLI surfaces (`cli/logs.rs`, `daemon.rs`, `json_listener.rs`)

### Fixed
- 23 latent clippy warnings under `--all-targets` (mostly `&PathBuf` → `&Path`, needless `&[…]` borrow in `args(…)`, dead `total_rejected` in example)
- Stray `eprintln!` in `smart_filter.rs` and `hud.rs` migrated to `tracing::{warn,info}`

## [0.13.0] - 2026-04-16

### Added
- Silero VAD integration for improved voice activity detection
- Hallucination detection module
- UDS event streaming for real-time state communication
- SwiftUI menu bar application with status overlay

### Changed
- Migrated from file-based state to UDS event streaming
- Improved filtering and post-processing pipeline
- Enhanced vocabulary biasing for Whisper transcription

### Security
- API keys now masked in config show command
- Removed API key prefix logging from stderr

## [0.12.0] - 2026-04-06

### Added
- Initial release
- Voice activity detection
- Groq Whisper API integration
- Automatic paste functionality
- Vocabulary biasing support
