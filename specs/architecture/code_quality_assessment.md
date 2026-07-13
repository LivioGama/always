# Code Quality Assessment: Always

Date: 2026-05-22
Mode: Full Assessment
Primary focus: Stabilization and safe high-impact improvements

## Metrics Dashboard

| Metric | Current |
| --- | ---: |
| Rust production LOC (`src/`) | 15,895 |
| Rust + Swift test LOC (`tests/`, `Always/Tests`) | 1,282 |
| Swift production LOC (`Always/Sources`) | 6,123 |
| Test assertions/cases detected | 170+ |
| Direct Cargo dependencies | 27 |
| Largest Rust file | `src/managers/model_registry.rs` — 1,208 LOC |
| Largest Swift file | `Always/Sources/Always/Views/SettingsWindow.swift` — 795 LOC |

Verification snapshot after stabilization:

- `cargo fmt --check`: passing
- `cargo clippy --all-targets --all-features --locked -- -D warnings`: passing
- `cargo test --locked --all-targets`: passing
- `cargo test --locked --all-targets --all-features`: passing
- `cargo test --locked --no-default-features --features linux --lib --bins`: passing
- `swift test --package-path Always`: passing
- `./run.sh`: builds, deploys, launches `/Applications/Always.app`, and starts one bundled daemon

## Subsystem Ratings

### Rust Daemon Core (`src/always/`) ★★★★☆

Strengths:
- The daemon has clear runtime modules for audio, VAD, event broadcasting, UDS control, configuration, paste, and telemetry.
- Critical text filtering and hallucination logic have meaningful unit coverage.
- UDS socket permissions are restricted to owner access, reducing local command-injection risk.

Concerns:
- Several mature modules now exceed the healthy Rust file-size range: `model_registry.rs` at 1,208 LOC, `uds_server.rs` at 893 LOC, `correction.rs` at 800 LOC, `vocab/plugins.rs` at 746 LOC, `event_loop.rs` at 737 LOC, and `vad.rs` at 687 LOC.
- `vad.rs` and `event_loop.rs` remain high-risk because they mix latency-sensitive capture, runtime configuration, speculative transcription, duplicate suppression, and event emission.

### CLI and Configuration (`src/main.rs`, `src/cli/`, `src/db.rs`) ★★★☆☆

Strengths:
- CLI commands are explicit and are covered by integration tests for config/status/socket-path behavior.
- Sensitive API-key storage has moved toward keychain-backed paths.

Concerns:
- CLI code still has mixed command dispatch and operational behavior in `src/main.rs` at 692 LOC.
- CLI/docs drift remains a realistic risk; this pass fixed stale `toggle pause` / `toggle auto-enter` references and restored help text for `toggle-pause`.

### Swift App (`Always/Sources/Always/`) ★★★☆☆

Strengths:
- The app has tests for event decoding, UDS defaults, overlay flash behavior, state monitor reactions, shortcut formatting, key persistence decisions, and Groq validation mapping.
- The UI now has pure helper seams for network-free validation and masked API-key persistence checks.

Concerns:
- `SettingsWindow.swift` at 795 LOC, `StatusOverlay.swift` at 781 LOC, and `UDSClient.swift` at 780 LOC now exceed the 500 LOC threshold and mix presentation, state orchestration, protocol decoding, and side effects.
- Sparkle startup is now defensive in local/dev builds: the app skips Sparkle when `SUPublicEDKey` is still the placeholder instead of logging an invalid EdDSA key failure. Public release still requires replacing the placeholder key.

### CI and Tooling (`.github/workflows/ci.yml`) ★★★★☆

Strengths:
- CI runs Rust format, all-target/all-feature clippy, Rust tests, Swift build/test, audit/deny/machete, Linux no-defaults build, dependency review, and CodeQL.
- The release workflow builds/signs/notarizes the macOS app, generates a DMG/appcast/SBOM/checksums, publishes crates/Homebrew artifacts, and expects Sparkle signing secrets.

Concerns:
- Release remains blocked until `SUPublicEDKey` is replaced with the public half matching `SPARKLE_ED_PRIVATE_KEY`.
- The manual microphone/permissions/dictation matrix still requires human-visible macOS interaction before tagging.

### Documentation (`README.md`, `docs/`) ★★★☆☆

Strengths:
- README explains usage, overlay rules, vocabulary, and known optimization opportunities.
- Architecture and development docs now point at UDS streaming, Swift app responsibilities, configurable shortcuts, and current verification commands.

Concerns:
- Documentation is still compact and lacks deeper operational runbooks for release-mode Swift verification, permissions, and mock STT testing.

## Key Findings

1. **Quality gates were not green before this pass.** Format, clippy, all-target Rust tests, Swift tests, all-feature local-STT tests, and Linux smoke now pass.
2. **All-feature testing exposed shared global-state races.** Pause/per-app tests now serialize their global mutations.
3. **Linux release smoke caught macOS-only import drift.** Hotkey handlers are now gated to the macOS feature path.
4. **Sparkle local startup was noisy with the checked-in placeholder.** UpdateService now disables Sparkle until a real public key is configured; release builds still fail fast if notarization is attempted with the placeholder.
5. **Large files are now the main maintainability risk.** The safest next step is targeted decomposition after v1.0 rather than broad pre-release refactors.

## Recommendations

### P1: Keep Stabilization Gates Mandatory

- **What**: Keep `cargo fmt --check`, `cargo clippy --all-targets --all-features --locked -- -D warnings`, `cargo test --locked --all-targets`, `cargo test --locked --all-targets --all-features`, Linux no-defaults smoke, and `swift test --package-path Always` as required pre-merge checks.
- **Risk**: Low — already passing locally after this pass.
- **Impact**: Prevents recurrence of formatting, namespace, and warning regressions.

### P1: Finish Sparkle Release Key Setup

- **What**: Generate the Sparkle EdDSA keypair, set `SPARKLE_ED_PRIVATE_KEY` in GitHub secrets, replace `SUPublicEDKey` in `Always/Info.plist`, and run the release workflow dry-run/tag flow.
- **Risk**: Medium — key mismatch breaks auto-update.
- **Impact**: Removes the last known release-packaging blocker.

### P2: Split Large Model/Protocol Modules

- **What**: Split `model_registry.rs`, `uds_server.rs`, and `correction.rs` along catalog/download/protocol/queue boundaries after v1.0.
- **Risk**: Medium — model and UDS behavior touch release-critical flows.
- **Impact**: Reduces the largest Rust maintenance hotspots without changing external behavior.

### P2: Decompose Swift Settings and Overlay

- **What**: Move settings persistence, UDS event decoding helpers, and overlay state calculation into smaller testable units.
- **Risk**: Medium — UI behavior must be visually verified.
- **Impact**: Reduces near-threshold Swift files and improves testability.

### P3: Add Coverage and Operational Runbooks

- **What**: Add coverage reporting and expand docs for permissions, mock STT testing, and release verification.
- **Risk**: Low.
- **Impact**: Makes future quality work measurable instead of anecdotal.

## Assumptions

- Existing uncommitted changes are preserved as user work.
- The first quality target is green gates plus targeted safety improvements, not broad refactoring.
- Live Groq credentials are not required for tests; network-facing behavior should use mocks or pure status mapping where possible.
