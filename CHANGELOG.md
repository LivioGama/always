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
- Groq grammar correction on gpt-oss models requests `reasoning_effort: low` and omits reasoning text from the response.
- CI: `cargo clippy --all-targets --all-features --locked -D warnings`, blocking `cargo audit`, runner pinned to `macos-14`.
- `SECURITY.md`: disclosure email, supported-versions table, embargo timeline.
- `build.sh`: sync bundle version from `Cargo.toml`, bundle integrity checks, rsync deploy to preserve TCC grants.

### Fixed
- Daemon memory leak (0.7 GB at launch → 4.2 GB within an hour of dictation, tens of GB per working day): ONNX Runtime 2.0.0-rc.12 on current macOS retains a slice of every inference's working buffers at the process level for the daemon's lifetime (measured; `heap` shows ~12 retained `std::shared_ptr` blocks per run; session recycling and the arena/memory-pattern options do not bound it). The daemon now checks its own physical footprint every 30 s and, past a 3 GiB ceiling while it is not actively listening (paused or ≥60 s silent), exits gracefully so the GUI respawns a fresh daemon — never mid-utterance (SPEC §7.3).
- The 1 Hz mic-conflict probe also retained memory while any non-excluded app ran audio input: the Info.plist value CFType from `CFBundleGetValueForInfoDictionaryKey` was never `CFRelease`d and the LaunchServices/CFBundle display-name resolution re-ran every poll. Display names are now resolved once per bundle id and cached, and the plist value is released.
- Dictation latency: SoX's 4.1 s output buffer delivered microphone audio in 4-second bursts, delaying the listening badge, the end-of-speech cut and the paste by up to 4 s. The recorder now streams in 128 ms blocks through a dedicated reader thread; latency logs measure from capture time.
- Long dictations no longer end, paste (and auto-Enter) at the first ~270 ms pause after the 6 s chunk mark: the short-utterance window was judged on the audio since the last chunk instead of the whole utterance.
- Grammar pre-warm keys now match the paste path (local cleanup applied inside the request builder; chunk joins warmed as they will be pasted), so the LLM call is usually already done at paste time.
- The overlay hides within ~0.15 s of the final text instead of ~1 s, shows live preview text as it arrives instead of one update late, and returns to the live badge when a confirmation flash ends mid-utterance.
- Case-insensitive bundle collision (`Always` vs `always`) — daemon ships as `always-daemon`.
- Vocabulary false positives (`Zed` in `analyzed`) via word-boundary replacement.
- Paste-in-flight lock leak, atomic config/glossary writes, SQLite busy timeout, UDS client resilience.
- STT language persistence, model UX, overlay visibility, and listening latency regressions.

## [0.0.1] - 2026-06-06

First tagged build. No prior public releases.
