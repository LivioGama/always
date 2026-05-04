# Code Quality Assessment: Always

Date: 2026-05-03
Mode: Full Assessment
Primary focus: Stabilization and safe high-impact improvements

## Metrics Dashboard

| Metric | Current |
| --- | ---: |
| Rust production LOC (`src/`) | 8,176 |
| Rust integration test LOC (`tests/`) | 317 |
| Swift production LOC (`AlwaysApp/Sources`) | 2,557 |
| Swift test LOC (`AlwaysApp/Tests`) | 248 |
| Test assertions/cases detected | 109 |
| Direct Cargo dependencies | 27 |
| Largest Rust file | `src/always/vocab/plugins.rs` — 705 LOC |
| Largest Swift file | `AlwaysApp/Sources/AlwaysApp/Views/SettingsWindow.swift` — 493 LOC |

Verification snapshot after stabilization:

- `cargo fmt --check`: passing
- `cargo clippy --release -- -D warnings`: passing
- `cargo test keyboard`: passing
- `swift test`: passing after adding pure helper coverage

## Subsystem Ratings

### Rust Daemon Core (`src/always/`) ★★★★☆

Strengths:
- The daemon has clear runtime modules for audio, VAD, event broadcasting, UDS control, configuration, paste, and telemetry.
- Critical text filtering and hallucination logic have meaningful unit coverage.
- UDS socket permissions are restricted to owner access, reducing local command-injection risk.

Concerns:
- Several mature modules exceed the healthy Rust file-size range: `vocab/plugins.rs` at 705 LOC, `vocab.rs` at 596 LOC, `ai_filter.rs` at 552 LOC, and `hallucination.rs` at 531 LOC.
- `vad.rs` mixes recording, VAD state transitions, speculative transcription, energy checks, and event emission in one 402 LOC file. It is below the god-module threshold but is high-risk because it is latency-sensitive.

### CLI and Configuration (`src/main.rs`, `src/cli/`, `src/db.rs`) ★★★☆☆

Strengths:
- CLI commands are explicit and are covered by integration tests for config/status/socket-path behavior.
- Sensitive API-key storage has moved toward keychain-backed paths.

Concerns:
- CLI code still has mixed command dispatch and operational behavior in `src/main.rs` at 371 LOC.
- The logs command had a stale namespace path that only surfaced when compiling the binary test target, showing that CLI module coverage needs to stay part of routine verification.

### Swift App (`AlwaysApp/Sources/AlwaysApp/`) ★★★☆☆

Strengths:
- The app has tests for event decoding, UDS defaults, overlay flash behavior, state monitor reactions, shortcut formatting, key persistence decisions, and Groq validation mapping.
- The UI now has pure helper seams for network-free validation and masked API-key persistence checks.

Concerns:
- `SettingsWindow.swift` at 493 LOC and `StatusOverlay.swift` at 477 LOC are near the 500 LOC threshold and mix presentation, state orchestration, and side effects.
- GitNexus could not index Swift symbols because the Swift tree-sitter parser is unavailable, so Rust graph tooling does not currently protect Swift refactors.

### CI and Tooling (`.github/workflows/ci.yml`) ★★★★☆

Strengths:
- CI runs Rust format, clippy, tests, Swift build, and Swift tests on macOS.
- `cargo-audit` installation now fails loudly if the tool cannot install.

Concerns:
- `cargo audit` remains advisory-only while transitive advisories are reviewed.
- There is no coverage threshold or release-mode Swift verification gate yet.

### Documentation (`README.md`, `docs/`) ★★★☆☆

Strengths:
- README explains usage, overlay rules, vocabulary, and known optimization opportunities.
- Architecture and development docs now point at UDS streaming, Swift app responsibilities, configurable shortcuts, and current verification commands.

Concerns:
- Documentation is still compact and lacks deeper operational runbooks for release-mode Swift verification, permissions, and mock STT testing.

## Key Findings

1. **Quality gates were not green before stabilization.** `cargo fmt --check` and clippy both failed at the start of the pass. These now pass.
2. **Shortcut configuration needed focused safety tests.** Parsing/matching now has tests for valid combos, invalid shortcuts, unsupported keys, and fallback defaults.
3. **Swift UI work needed testable seams.** Groq validation status mapping and masked API-key persistence are now pure helpers with tests, reducing reliance on live network/UI behavior.
4. **Audit tooling policy needed clarity.** CI no longer hides `cargo-audit` installation failure; advisory results remain non-blocking by documented choice.
5. **Large files are the next maintainability bottleneck.** The safest next step is targeted decomposition after all gates stay green.

## Recommendations

### P1: Keep Stabilization Gates Mandatory

- **What**: Keep `cargo fmt --check`, `cargo clippy --release -- -D warnings`, `cargo test`, `swift build`, and `swift test` as required pre-merge checks.
- **Risk**: Low — already passing locally after this pass.
- **Impact**: Prevents recurrence of formatting, namespace, and warning regressions.

### P1: Add Release-Mode Swift Verification

- **What**: Add a documented local release build check for `AlwaysApp`, then promote it to CI once the overlay release-mode regression is resolved.
- **Risk**: Medium — may expose the known overlay issue.
- **Impact**: Closes the most visible confidence gap for shipping the macOS app.

### P2: Split Large Vocabulary Plugin Module

- **What**: Split `src/always/vocab/plugins.rs` by plugin and shared extraction helpers.
- **Risk**: Medium — import behavior touches user vocabulary quality.
- **Impact**: Reduces the largest Rust maintenance hotspot without changing external behavior.

### P2: Decompose Swift Settings and Overlay

- **What**: Move settings persistence helpers and overlay state calculation into smaller testable units.
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
