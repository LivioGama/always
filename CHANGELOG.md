# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- GitHub Actions CI workflow with formatting, clippy, and test checks
- CHANGELOG.md, CONTRIBUTING.md, and SECURITY.md documentation
- Standardized log locations following platform conventions

### Changed
- README updated to honestly reflect macOS Apple Silicon-only support
- Moved ARCHITECTURE.md and DEVELOPMENT.md to docs/ directory
- Moved debug binaries from src/bin/ to examples/ directory
- Removed API key prefix logging for improved security

### Fixed
- Fixed broken test build by adding missing `silero_threshold` field to test fixture
- Removed dev session junk files from repository

## [0.13.0] - 2024-XX-XX

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

## [0.12.0] - 2024-XX-XX

### Added
- Initial release
- Voice activity detection
- Groq Whisper API integration
- Automatic paste functionality
- Vocabulary biasing support
