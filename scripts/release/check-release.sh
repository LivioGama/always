#!/bin/sh
# Release consistency check, shared by prepare.sh and the Release workflow's
# `verify` job.
#
#   scripts/release/check-release.sh v0.1.0     # enforce: refuse on any mismatch
#   scripts/release/check-release.sh --report   # print the same findings, exit 0
#
# Enforced for vX.Y.Z:
#   1. every version file below declares X.Y.Z (lockstep);
#   2. CHANGELOG.md has a `## [X.Y.Z]` heading and `## [Unreleased]` holds no
#      bullet (entries live in .github/changelog.d/ until the cut);
#   3. the root Cargo.lock is fresh for its manifest (`cargo update --locked`).
set -eu

MODE=enforce
TAG=""
case "${1:-}" in
    --report) MODE=report ;;
    v[0-9]*) TAG="$1" ;;
    refs/tags/v[0-9]*) TAG="${1#refs/tags/}" ;;
    *) echo "usage: check-release.sh vX.Y.Z | --report" >&2; exit 2 ;;
esac

cd "$(git rev-parse --show-toplevel)"

# The version files that move in lockstep. Keep in sync with prepare.sh.
CARGO_MANIFESTS="Cargo.toml crates/always_tauri/Cargo.toml src-tauri/Cargo.toml"
JSON_FILES="tauri.conf.json always-gui/package.json"
PLIST="Always/Info.plist"

cargo_version() { sed -n 's/^version *= *"\([^"]*\)".*/\1/p' "$1" | head -n 1; }
json_version() { sed -n 's/^ *"version" *: *"\([^"]*\)".*/\1/p' "$1" | head -n 1; }
plist_version() { # plist_version <file> <key>
    awk -v k="$2" '$0 ~ "<key>" k "</key>" { f = 1; next }
        f && /<string>/ { gsub(/.*<string>|<\/string>.*/, ""); print; exit }' "$1"
}

FOUND=""
add() { FOUND="${FOUND}${FOUND:+
}$1 $2"; }
for f in $CARGO_MANIFESTS; do [ -f "$f" ] && add "$f" "$(cargo_version "$f")"; done
for f in $JSON_FILES; do [ -f "$f" ] && add "$f" "$(json_version "$f")"; done
if [ -f "$PLIST" ]; then
    add "$PLIST#CFBundleShortVersionString" "$(plist_version "$PLIST" CFBundleShortVersionString)"
    add "$PLIST#CFBundleVersion" "$(plist_version "$PLIST" CFBundleVersion)"
fi

echo "version files:"
printf '%s\n' "$FOUND" | awk '{ printf "  %-48s %s\n", $1, $2 }'

if [ "$MODE" = report ]; then
    distinct="$(printf '%s\n' "$FOUND" | awk '{print $2}' | sort -u | wc -l | tr -d ' ')"
    if [ "$distinct" -gt 1 ]; then
        echo "check-release: WARNING version files disagree ($distinct distinct values); prepare.sh sets them all at release time"
    fi
    exit 0
fi

WANT="${TAG#v}"
FAIL=0
bad="$(printf '%s\n' "$FOUND" | awk -v w="$WANT" '$2 != w { printf "  %s is %s, tag is %s\n", $1, $2, w }')"
if [ -n "$bad" ]; then
    echo "check-release: version mismatch:" >&2
    printf '%s\n' "$bad" >&2
    FAIL=1
fi

if ! grep -Fq "## [$WANT]" CHANGELOG.md; then
    echo "check-release: CHANGELOG.md has no '## [$WANT]' heading" >&2
    FAIL=1
fi
stray="$(awk '
    /^## / { inside = index($0, "## [Unreleased]") == 1; next }
    inside && /^[[:space:]]*- / { n++ }
    END { print n + 0 }
' CHANGELOG.md)"
if [ "$stray" -ne 0 ]; then
    echo "check-release: $stray bullet(s) still under '## [Unreleased]' in CHANGELOG.md" >&2
    FAIL=1
fi

LOCKLOG="$(mktemp)"
trap 'rm -f "$LOCKLOG"' EXIT
# Only the root lockfile is built `--locked` by CI and the release; the Tauri
# crates' lockfiles are not maintained by cargo here (see prepare.sh).
# A throwaway target dir keeps cargo's scratch files out of the tracked tree.
CARGO_TARGET_DIR="$(mktemp -d)"
export CARGO_TARGET_DIR
if ! cargo update --workspace --locked --quiet --manifest-path Cargo.toml 2>"$LOCKLOG"; then
    echo "check-release: Cargo.lock is stale for Cargo.toml:" >&2
    cat "$LOCKLOG" >&2
    FAIL=1
fi
rm -rf "$CARGO_TARGET_DIR"

if [ "$FAIL" -ne 0 ]; then exit 1; fi
echo "check-release: all checks passed for $TAG"
