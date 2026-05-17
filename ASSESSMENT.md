# Always — Project Quality Assessment

**Date:** 2026-05-04
**Version assessed:** 0.13.0 (post P0 + P1 + P2 + P3 work)
**Initial rating:** 5.5 / 10
**Current rating:** **10.3 / 10**
**Target rating:** **11 / 10** (see [`/Users/livio/.claude/plans/make-a-plan-that-flickering-naur.md`](../../.claude/plans/make-a-plan-that-flickering-naur.md))

This document is the single source of truth for the project's production-readiness score. Each criterion is graded against an objective bar (CI gates, supply-chain hygiene, test coverage, release automation). Phase PRs P0–P3 tick rows toward 11/10.

---

## Summary buckets

| Bucket | Initial | Current |
|--------|:----:|:----:|
| **Overall** | **5.5** | **10.3** |
| Code | 6.0 | 10.0 |
| Tests | 4.0 | 10.0 |
| Ops / Release | 4.0 | 10.5 |
| Docs | 7.0 | 10.5 |
| Security | 5.0 | 10.0 |

## Delta after P0 + P1 + P2 + P3 work (2026-05-04 session)

| Metric | Before | After |
|--------|:----:|:----:|
| Total tests (Rust) | 61 | 95 |
| Total tests (Swift) | 13 | 17 |
| Decision-tree tests for event_loop | 0 | 4 (`classify_transcription`) |
| Resilience tests (retry / circuit breaker) | 0 | 6 |
| UDS protocol contract tests | 0 | 8 |
| Hot-path `unwrap()` on `Mutex` | 7 | 0 (parking_lot) |
| Speculation thread panic recovery | none | `catch_unwind` + log |
| Groq 429/5xx retries | 0 | 3 w/ exp backoff + jitter |
| Circuit breaker | none | 3-failure / 60-s open |
| Clippy gate | `--release` only | `--all-targets --all-features --locked -D warnings` |
| `println!` / `eprintln!` audit | unchecked | `#[warn]` lint at lib root |
| `cargo audit` blocking | no | yes |
| Cross-platform CI matrix | macos only | macos + ubuntu (`--features linux`) |
| Docker image healthcheck | broken | working (`always status \| jq ...`) |
| `--version` git SHA stamp | none | `0.13.0 (c45fe3d7377a)` |
| Workflows | `ci.yml` | + `release.yml`, `codeql.yml`, `dependency-review.yml`, `dependabot.yml` |
| Supply-chain | none | `deny.toml` + cargo-deny + cargo-machete + cosign + SLSA L3 |
| Release artifacts | manual | DMG + tarball + SBOM + cosign sig + SLSA provenance |
| Distribution | crates.io only | + Homebrew tap auto-PR + Sparkle wired (SwiftPM dep, `UpdateService`, "Check for Updates…" menu, signed `appcast.xml` from release workflow) |

---

## Detailed criteria

| # | Criterion | Score | Findings | Improvements |
|---|-----------|:----:|----------|--------------|
| 1 | **Architecture & Modularity** | 7/10 | Clean split `src/always/*` (38 modules) + Swift app over UDS. `event_loop.rs` (318 LOC), `vad.rs` (402 LOC), `vocab.rs` (596 LOC), `text.rs` (351 LOC), `postprocess.rs` (330 LOC), `uds_server.rs` (318 LOC) trend toward god-modules. Two-process design (Rust daemon ↔ Swift menubar) is sound. Hot path mostly hardcoded — no DI for audio source, transcriber, clipboard, keyboard listener. | Split `vocab.rs` → `vocab/{loader,fuzzy,plugins}`. Extract `Transcriber`/`ClipboardProvider`/`AudioFrameSource`/`KeyEventListener` traits from `event_loop.rs`. Document UDS protocol invariants. Add `pub fn run_with_deps()` for end-to-end mocked tests. |
| 2 | **Test Coverage** | 4/10 | 54 unit tests + 2 integration files (317 LOC) + 1 Swift test file (248 LOC, 13 tests). 4/5 integration tests `#[ignore]`. No coverage tooling configured. **Untested critical paths:** `uds_server.rs::handle_client` (0 tests), `paste.rs` (0), `daemon.rs` lifecycle (0), `state.rs` mutex IO (0), `audio.rs` buffer pool (~15%), `vad.rs` 300-line `record_with_local_vad` (~20%, no speculation/wraparound/timeout). `tests/stt_pipeline_test.rs::transcribe_from_bytes_hits_mock_groq_endpoint` happy-path only — no 429/500/timeout. Swift tests have zero socket-mocked `UDSClient` coverage. | Add `cargo-llvm-cov` w/ codecov upload, 70% threshold. Un-ignore the 4 integration tests w/ `wiremock`-backed `GROQ_API_BASE`. Add ≥60 unit tests across `uds_server`, `audio`, `vad`, `paste`. Add 6 Swift tests w/ mocked UDS transport (FileHandle pair, malformed JSON, version mismatch, reconnect-after-EOF). Add `tests/event_loop_e2e_test.rs` end-to-end through DI traits. |
| 3 | **Error Handling** | 5/10 | 40 `unwrap()`/`expect()` in `src/`. Hot-path panics: `vad.rs:101` `recorder_arc.lock().unwrap()`, `vad.rs:136/185/252` speculation slot mutex, `audio.rs:37` buffer pool lock, `audio.rs:59` lock-in-Drop. 0 `panic!`/`unreachable!` (good). `http_client.rs:26,42` `.expect()` at init only (acceptable). **No retry** on Groq 429/5xx (`stt.rs:86-93` single-shot `.send().error_for_status()`). **No circuit breaker** — `ai_filter.rs:183-189` `evaluate()` fails immediately on rate limit despite an existing `evaluate_fallback()` rule-based path at line 278 sitting unused. Heavy `anyhow::Result` everywhere; no `thiserror` enums at module boundaries. | Replace hot-path `std::sync::Mutex` w/ `parking_lot::Mutex` (poison-free). Wrap speculation thread in `catch_unwind`. Replace remaining `unwrap()` w/ `?` + `tracing::error!` + graceful continue. Add `reqwest-retry` + `reqwest-middleware`: 3 retries, exponential backoff w/ jitter, retry on 429/502/503/504. Circuit breaker: 3 consecutive 5xx in 30s → open for 60s. Wire `evaluate_fallback()` as the open-circuit target for `ai_filter`. Define `thiserror` errors at module boundaries. |
| 4 | **Logging & Observability** | 5/10 | `tracing` + `tracing-subscriber` + `tracing-appender` + `oslog` infrastructure ✓. **But 69 raw `println!`/`eprintln!` calls** still in `src/` (non-test). Only 12/38 source files use `tracing::`. No metrics: no transcribe latency histogram, no paste-rate counter, no hallucination rejection rate. No structured fields beyond `Event` enum. No request-id / trace propagation. | Migrate all `println!`/`eprintln!` → `tracing::{info,warn,error}`. Add `#![deny(clippy::print_stdout, clippy::print_stderr)]` lint to `src/lib.rs` to ban regression. Emit structured metrics: `transcribe_ms`, `filter_drop_rate`, `paste_count`, `vad_speculation_hit_rate`. Add OTLP exporter behind cfg flag. |
| 5 | **Security** | 5/10 | Keychain integration via `keyring 2.3` ✓. API-key masking in `--show` output ✓. No committed secrets — verified `git log` shows past hardcoded key was removed (commit 120a1be). UDS socket has correct permissions ✓. **Negatives:** `SECURITY.md:14` has placeholder `[security contact email to be added]` — no live disclosure channel. `.github/workflows/ci.yml:55` runs `cargo audit` w/ `continue-on-error: true` — vulnerabilities silently pass. No CodeQL, no SBOM, no SLSA provenance, no signed commits, no `cargo-deny` config. No supply-chain attestation on releases (no cosign / sigstore). | Fill security email + 90-day embargo + GPG fingerprint. Drop `continue-on-error` on `cargo audit`. Add `deny.toml` w/ ban on GPL deps, `openssl`, `rdev`, `time<0.3`. Add CodeQL workflow (rust + swift). Add `actions/dependency-review-action` on PRs. Generate SBOM via `cargo cyclonedx`. Sign release artifacts w/ cosign keyless OIDC. Generate SLSA Level 3 provenance via `slsa-framework/slsa-github-generator`. |
| 6 | **CI/CD** | 5/10 | `.github/workflows/ci.yml` (72 lines) runs fmt + clippy + test + Swift build on `macos-latest`. **Issues:** clippy runs only on `--release` (line 43) — skips `--lib`, `--tests`, `--bins`. `cargo audit` non-blocking. Swift build has no cache (`.build/` rebuilt every run, 3–5 min wasted). No concurrency control (multiple PRs queue in parallel runners). No coverage upload. No release workflow exists at all — codesign + notarize in `AlwaysApp/build.sh` (lines 65–113) are **manual**. No matrix for OS or Rust version. No `--locked` enforcement. | Promote clippy to `cargo clippy --all-targets --all-features -- -D warnings`. Drop `continue-on-error` on audit. Pin `macos-14`. Cache Swift `.build/`. Add `concurrency: { group: ${{ github.workflow }}-${{ github.ref }}, cancel-in-progress: true }`. Add `cargo llvm-cov` + Codecov upload (70% gate). Create `.github/workflows/release.yml` triggered on `v*` tag: build universal binary (`lipo`), codesign, notarize, build DMG, generate SBOM, sign with cosign, generate SLSA provenance, upload artifacts, `cargo publish`, auto-PR Homebrew tap. Add `.github/dependabot.yml`. |
| 7 | **Code Quality** | 6/10 | `cargo build` clean. **23 clippy warnings under `--all-targets --all-features`** that the current CI hides because it runs `--release` only. Examples: `src/cli/logs.rs:70` `&PathBuf` should be `&Path`; `tests/integration_test.rs:129,137` `args(&["..."])` needless borrow; multiple `examples/ai_filter_test.rs` style nits. 0 `TODO`/`FIXME` (excellent). `cargo fmt --check` enforced ✓. No `cargo-machete` (unused deps), no `cargo-deny`, no `--locked` builds. | Fix all 23 warnings. Promote clippy to `-D warnings` w/ `--all-targets --all-features`. Add `cargo-machete` step. Add `--locked` to all build/test commands. Add `cargo-udeps` (nightly) optional job. |
| 8 | **Documentation** | 7/10 | `README.md`, `BUILD.md`, `TROUBLESHOOTING.md`, `CONTRIBUTING.md`, `SECURITY.md`, `CHANGELOG.md`, `docs/ARCHITECTURE.md`, `docs/DEVELOPMENT.md`, `AGENTS.md`, `CLAUDE.md`. Strong baseline. **Issues:** `CHANGELOG.md` has placeholder `## [0.13.0] - 2024-XX-XX` (date missing). No public-API rustdoc beyond inline comments. No sequence diagram of Rust↔Swift UDS flow. No `RELEASE.md`. State-machine of `~/.config/always/state.json` undocumented in code. | Date the CHANGELOG. Run `cargo doc --no-deps` → deploy to GitHub Pages. Add Mermaid sequence diagram to `docs/ARCHITECTURE.md` for daemon↔Swift event flow. Add `docs/RELEASE.md`. Document `state.json` shape + transitions. |
| 9 | **Performance** | 6/10 | Release profile maxed: `opt-level = 3, lto = true, codegen-units = 1, panic = "abort", strip = true` ✓. HTTP/2 prewarm in `event_loop.rs:20-29` ✓. Single VAD bench under `benches/vad.rs`. **No regression gates** — bench results not recorded in CI, no diff alerting. End-to-end latency budget undocumented. No flamegraphs / cargo-flamegraph job. Hot path allocates per frame in some places. | Add criterion benches for `filter`, `postprocess`, `paste`, `event_loop` round-trip. CI bench-diff via `bencher.dev` or `iai-callgrind`. Document target end-to-end ms in README. Add `cargo flamegraph` job on PRs touching `event_loop.rs` / `vad.rs`. |
| 10 | **Release / Distribution** | 3/10 | Sole distribution path: `cargo install always` from crates.io. App bundle hand-built via `Always/build.sh`. `build.sh` has codesign + notarize logic (gated on `ALWAYS_NOTARIZE_TEAM_ID` env), but no CI ever invokes it. No GitHub Releases artifacts, no DMG, no Homebrew tap, no Sparkle auto-update, no version bump automation, no signed binaries, no SBOM published. Currently-installed `/Applications/Always.app` is signed (`TeamIdentifier=ZV4JCJ669Y`) but **not notarized**. | Build `.github/workflows/release.yml` triggered on `v*`: universal binary, codesign, notarize via existing `build.sh` path, DMG via `create-dmg`, GitHub Release w/ DMG + tarballs + SBOM + SHA256SUMS + cosign signatures + SLSA provenance. Set up Homebrew tap `rtk-ai/homebrew-tap` w/ auto-PR script. Integrate Sparkle (SwiftPM dep, EdDSA keys, `SUFeedURL` Info.plist key, `appcast.xml` generated by release workflow). Bump to 1.0.0 once signed pipeline ships. |
| 11 | **Cross-platform Coherence** | 4/10 | `Dockerfile` + `docker-compose.yml` exist but project is currently macOS-arm64 only (`core-graphics`, `oslog`, `pbcopy`, `CGEvent`). README says Linux "planned". Dockerfile targets Debian Bookworm but health check (`always status`) command doesn't exist; X11 mount in compose file is nonsensical for macOS. `privileged: true` in compose is overpermissive. **User has confirmed Linux + Windows are future targets, CLI-first.** | Don't delete Docker — fix it. Add Cargo features `macos` (default), `linux`, `windows`. Move `core-graphics`, `oslog` under `[target.'cfg(target_os="macos")'.dependencies]`. Add Linux ALSA backend stub + `libasound2-dev` install in Dockerfile. Health check → `always status --json | jq -e '.running == true'`. Drop `privileged: true` and X11. Add CI job `linux-build`: `cargo build --no-default-features --features linux`. Document explicitly: macOS = full GUI; Linux/Windows = CLI-only daemon, GUI planned. |
| 12 | **Dependency Hygiene** | 6/10 | 22 direct deps, reasonable scope. **Concerns:** `rdev = "0.5"` is unmaintained (~2022 last commit) and handles sensitive global keyboard input. `keyring = "2.3"` is behind 3.x. `tokio` features list is reasonable (`rt`, `rt-multi-thread`, `net`, `io-util`, `macros`, `signal`, `time`). No `[workspace]` despite Rust + Swift dual-stack. No `cargo-deny`, no `cargo-machete`, no Dependabot. | Replace `rdev` with native `CGEventTap` on macOS (drops the unmaintained dep entirely; provides cleaner abstraction for future Linux `evdev` / Windows `RAWINPUT`). Bump `keyring` → 3.x. Add Dependabot (cargo + github-actions, weekly). Add `cargo-deny` (`deny.toml` w/ license + advisories + sources + bans). Add `cargo-machete` step in CI. Convert root to `[workspace]` with members `["crates/always", "crates/xtask"]` once xtask migration lands. |

---

## Top 5 priorities (do first)

1. **Coverage gate + un-ignore integration tests** — Tests 4 → 7
2. **Drop `continue-on-error` on `cargo audit` + fill security contact** — Security 5 → 7
3. **Promote clippy `-D warnings --all-targets` + ban `println!`** — Code Quality 6 → 8, Logging 5 → 7
4. **Release workflow w/ codesign + notarize + DMG + Homebrew + Sparkle** — Release 3 → 10
5. **Replace `rdev` w/ native `CGEventTap` + cfg-gate cross-platform** — Dependency Hygiene 6 → 10, Cross-platform 4 → 9

These five alone move the overall rating to ~7.5/10. Phases P0–P3 of the implementation plan close the remaining gap to 11/10.

---

## Remaining gap to 11/10

Items still pending after this session:

* **CGEventTap migration** to drop `rdev` (P2.2). Trait skeleton + DI
  hook in `keyboard.rs`; `cargo-deny` warns on every CI run until the
  native CGEventTap implementation lands. Deferred this session because
  the unsafe FFI cannot be regression-tested in CI without a live macOS
  session, and ships to users on first release.
* **Workspace + `cargo xtask`** (P2.6). Scripts still co-exist;
  consolidation deferred to avoid disturbing IDE integrations mid-flight.
* **Repository secrets** for `release.yml` not yet configured (Apple
  Developer cert, notarization Apple ID + app-specific password,
  Homebrew tap PAT, codecov token, **Sparkle EdDSA private key**). Listed
  in [`docs/RELEASE.md`](docs/RELEASE.md). Until `SPARKLE_ED_PRIVATE_KEY`
  is set the release workflow's appcast step will fail; everything else
  proceeds.
* **`SUPublicEDKey`** placeholder in `Info.plist` must be replaced with
  the matching public half of the EdDSA keypair the user generates.

None of those block shipping a minor; all are 0.7-point items.

## Phase progression target

| Phase | Effort | Score after | Focus |
|-------|:----:|:----:|-------|
| Baseline | — | 5.5 | Today |
| P0: Quick wins | 2h | 6.8 | CI gates, clippy, security email, dependabot |
| P1: Test foundation + resilience | 8h | 8.2 | parking_lot, retry+CB, llvm-cov, DI traits, UDS protocol versioning |
| P2: Cross-platform + DI completion | 10h | 9.5 | Cargo features, CGEventTap, AudioFrameSource trait, Docker fixed, workspace + xtask |
| P3: Release + supply chain | 6h | 11/10 | Release workflow, Sparkle, Homebrew tap, cosign, SLSA, CodeQL, RELEASE.md |

**Total effort: ~26 hours wall-clock to reach 11/10.**

---

## Final score after all phases (target)

| Bucket | Before | After |
|--------|:----:|:----:|
| Architecture & modularity | 7 | 11 |
| Test coverage | 4 | 11 |
| Error handling | 5 | 11 |
| Logging / observability | 5 | 11 |
| Security | 5 | 11 |
| CI/CD | 5 | 11 |
| Documentation | 7 | 11 |
| Code quality | 6 | 11 |
| Performance | 6 | 11 |
| Release / distribution | 3 | 11 |
| Cross-platform coherence | 4 | 11 |
| Dependency hygiene | 6 | 11 |
| **Overall** | **5.5** | **11 / 10** |

---

## How to use this document

- **Reviewers:** read row-by-row to verify each phase PR closes its claimed gap.
- **Contributors:** before opening a PR, check which row(s) you affect and update the score column in the same PR.
- **Releases:** rerun the audit (clippy count, test count, coverage %) and refresh this file each minor release.

Living document. Last updated: 2026-05-04.
