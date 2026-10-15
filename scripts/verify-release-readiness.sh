#!/usr/bin/env bash
set -euo pipefail

MODE="${1:-local}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

run() {
  printf '\n==> %s\n' "$*"
  "$@"
}

require_command() {
  if ! command -v "$1" >/dev/null 2>&1; then
    printf 'Missing required command: %s\n' "$1" >&2
    exit 1
  fi
}

local_gates() {
  require_command cargo
  require_command swift
  require_command actionlint

  run actionlint \
    .github/workflows/ci.yml \
    .github/workflows/release.yml \
    .github/workflows/release-build.yml \
    .github/workflows/codeql.yml \
    .github/workflows/dependency-review.yml
  run cargo fmt --check
  run cargo clippy --all-targets --all-features --locked -- -D warnings
  run cargo test --locked --all-targets
  (cd Always && run swift test)
  run cargo audit --deny warnings --ignore RUSTSEC-2024-0384 --ignore RUSTSEC-2024-0388
  run cargo deny --all-features check
  run cargo machete
  RUSTDOCFLAGS="-D warnings" run cargo doc --locked --no-deps
  run cargo build --no-default-features --features linux --locked
  run cargo build --release --locked --target aarch64-apple-darwin
  ALWAYS_BUILD_PROFILE=release \
    ALWAYS_SWIFT_CONFIGURATION=release \
    ALWAYS_DAEMON_PATH="$ROOT/target/aarch64-apple-darwin/release/always" \
    run ./Always/build.sh
  run codesign --verify --deep --strict --verbose=2 /Applications/Always.app
  run /Applications/Always.app/Contents/MacOS/always-daemon --version
}

case "$MODE" in
  local)
    local_gates
    ;;
  *)
    printf 'Usage: %s [local]\n' "$0" >&2
    exit 2
    ;;
esac

printf '\nRelease readiness check (%s) passed.\n' "$MODE"
