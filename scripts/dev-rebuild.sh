#!/usr/bin/env bash
# Local dev loop: kill → build → bundle → launch the DEV app only.
#
# Builds "Always Dev.app" (bundle id com.always.v3.dev) and deploys it to
# /Applications/Always Dev.app — a separate identity with its own config
# dir, socket, pid file, and prefs. The production /Applications/Always.app
# and its running GUI/daemon are NEVER touched by this script.
#
# To ship a build as the production app, run scripts/promote.sh manually.
#
# Usage:
#   scripts/dev-rebuild.sh                # debug build (default — transcripts visible)
#   scripts/dev-rebuild.sh release        # release build (transcripts hidden)
#   scripts/dev-rebuild.sh --no-daemon    # skip daemon restart (for Swift-only changes)
#   scripts/dev-rebuild.sh --force-daemon # force daemon restart even if Rust unchanged
#   ALWAYS_REBUILD_SILENT=1 scripts/dev-rebuild.sh   # mute sounds

set -euo pipefail

# Parse arguments
SKIP_DAEMON=false
FORCE_DAEMON=false
PROFILE="debug"
for arg in "$@"; do
    case "$arg" in
        --no-daemon)
            SKIP_DAEMON=true
            ;;
        --force-daemon)
            FORCE_DAEMON=true
            ;;
        release)
            PROFILE="release"
            ;;
        debug)
            PROFILE="debug"
            ;;
    esac
done
SOUND_DIR="/System/Library/Sounds"
SOUND_KILL="$SOUND_DIR/Pop.aiff"
SOUND_COMPILED="$SOUND_DIR/Frog.aiff"
SOUND_UP="$SOUND_DIR/Funk.aiff"
SOUND_FAIL="$SOUND_DIR/Sosumi.aiff"

play() {
    [ "${ALWAYS_REBUILD_SILENT:-0}" = "1" ] && return 0
    [ -f "$1" ] || return 0
    afplay "$1" >/dev/null 2>&1 &
}

trap 'play "$SOUND_FAIL"; echo "✗ rebuild failed"' ERR

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

DEV_APP_NAME="Always Dev"
DEV_APP="/Applications/${DEV_APP_NAME}.app"
DEV_SUPPORT_DIR="$HOME/Library/Application Support/always-dev"
PROD_SUPPORT_DIR="$HOME/Library/Application Support/always"

# Check if Rust source files changed (to avoid unnecessary daemon restarts)
RUST_CHANGED=false
if [ "$FORCE_DAEMON" = false ] && [ "$SKIP_DAEMON" = false ]; then
    # Check if there are uncommitted changes in src/ directory
    if git diff --quiet src/ 2>/dev/null; then
        # No uncommitted changes, check if HEAD changed since last build
        # We use a timestamp file to track the last Rust build
        RUST_BUILD_MARKER="$REPO_ROOT/.rust_build_timestamp"
        CARGO_LOCK="$REPO_ROOT/Cargo.lock"
        if [ -f "$RUST_BUILD_MARKER" ] && [ -f "$CARGO_LOCK" ]; then
            # If Cargo.lock is newer than the marker, Rust dependencies changed
            if [ "$CARGO_LOCK" -nt "$RUST_BUILD_MARKER" ]; then
                RUST_CHANGED=true
                echo "  (Cargo.lock updated - Rust rebuild required)"
            fi
        else
            # Marker doesn't exist, assume first build
            RUST_CHANGED=true
        fi
    else
        # Uncommitted changes in src/, need rebuild
        RUST_CHANGED=true
        echo "  (Rust source changed - daemon restart required)"
    fi
fi

# ALWAYS kill the dev app, even for a Swift-only change (a stale running
# dev GUI makes `open` re-focus the old process and the new build never
# executes — same failure this script exists to prevent).
#
# These patterns are scoped to "Always Dev.app" on purpose: the string
# "Always Dev.app" cannot match the production path "Always.app", and
# broad patterns like `pkill -f "always-daemon run"` would kill the
# PRODUCTION daemon — the exact failure this split exists to prevent.
if true; then
    echo "▶ killing Always Dev (production Always is untouched)..."
    play "$SOUND_KILL"
    pkill -9 -f "Always Dev.app" 2>/dev/null || true

    if [ "$SKIP_DAEMON" = false ]; then
        # Dev daemon via its own pid file (covers bundled daemon AND any
        # manual `ALWAYS_INSTANCE=dev always run` from target/).
        if [ -f "$DEV_SUPPORT_DIR/always.pid" ]; then
            _dev_pid="$(cat "$DEV_SUPPORT_DIR/always.pid" 2>/dev/null || true)"
            if [ -n "${_dev_pid:-}" ]; then
                kill -TERM "$_dev_pid" 2>/dev/null || true
                sleep 0.5
                kill -KILL "$_dev_pid" 2>/dev/null || true
            fi
            rm -f "$DEV_SUPPORT_DIR/always.pid"
        fi
        sleep 0.2   # let processes actually die before rebuild
    fi
else
    echo "▶ skipping daemon restart (Rust unchanged)"
    SKIP_DAEMON=true
fi

echo "▶ cargo build ($PROFILE)..."
if [ "$SKIP_DAEMON" = true ]; then
    echo "  (skipped - --no-daemon flag set)"
else
    # local-stt enables the Parakeet/Whisper/Canary/etc local backends
    # via transcribe-rs. Without it the daemon silently falls back to
    # Groq when the user picks a local model — including ones already
    # cached by Handy. Always-on for the deploy script; CI can still
    # build without the feature for Linux/Windows.
    case "$PROFILE" in
        debug)   env -u CARGO_INCREMENTAL GGML_CCACHE=OFF cargo build --lib --bin always --features local-stt ;;
        release) env -u CARGO_INCREMENTAL GGML_CCACHE=OFF cargo build --release --lib --bin always --features local-stt ;;
        *) echo "unknown profile: $PROFILE (use 'debug' or 'release')"; exit 2 ;;
    esac
    # Update timestamp marker after successful Rust build
    touch "$REPO_ROOT/.rust_build_timestamp"
fi
play "$SOUND_COMPILED"

echo "▶ Swift bundle + deploy (Always Dev)..."
(
    cd Always
    ALWAYS_BUILD_PROFILE="$PROFILE" \
    ALWAYS_APP_NAME="$DEV_APP_NAME" \
    ALWAYS_BUNDLE_ID="com.always.v3.dev" \
    ALWAYS_DEPLOY_PATH="$DEV_APP" \
    ALWAYS_URL_SCHEME="always-dev" \
    ALWAYS_SPARKLE=0 \
    ./build.sh
)

# CRITICAL: remove the intermediate project-dir bundle. If it lives on,
# LaunchServices re-discovers it on every seed-rescan and `open` may
# resolve to it instead of /Applications.
echo "▶ removing intermediate bundle..."
rm -rf "$REPO_ROOT/Always/${DEV_APP_NAME}.app"

# First-run seed: copy production state into the dev instance so the dev
# app boots with the user's real prefs, Groq key, voiceprint, and
# vocabulary instead of a cold onboarding flow. Runs once — afterwards the
# dev dir is independent and prod edits never propagate.
if [ ! -d "$DEV_SUPPORT_DIR" ]; then
    echo "▶ seeding dev instance from production state..."
    mkdir -p "$DEV_SUPPORT_DIR"
    for f in always.db always.db-wal always.db-shm voiceprint.json vocabulary.json; do
        [ -f "$PROD_SUPPORT_DIR/$f" ] && cp "$PROD_SUPPORT_DIR/$f" "$DEV_SUPPORT_DIR/$f" || true
    done
fi

echo "▶ launching Always Dev..."
open "$DEV_APP"
sleep 3

# Prove the new build is the one running.
#
# "Deployed" and "running" are different claims, and this script used to
# only ever make the first one while printing a checkmark that read like
# the second. A process whose start time predates the binary it was
# built from is running STALE code, and saying "done" in that state
# sends someone off to test a build that does not exist on their machine.
verify_running() {
    local label="$1" pattern="$2" binary="$3"
    local pid
    pid=$(pgrep -f "$pattern" | head -1)
    if [ -z "$pid" ]; then
        echo "✗ $label is NOT running after launch"
        return 1
    fi
    # `etimes` is a Linux-only ps keyword; macOS ps rejects it and this
    # check silently passed on garbage. `lstart` is the portable-on-macOS
    # answer: an absolute start time, converted with BSD `date -j -f`.
    local lstart started mtime
    lstart=$(ps -o lstart= -p "$pid")
    started=$(date -j -f "%a %b %e %T %Y" "$lstart" +%s 2>/dev/null || echo "")
    mtime=$(stat -f %m "$binary")
    if [ -z "$started" ] || [ -z "$mtime" ]; then
        echo "✗ $label (pid $pid): could not read start time or binary mtime — treating as unverified"
        return 1
    fi
    if [ "$started" -lt "$mtime" ]; then
        echo "✗ $label (pid $pid) started $(( mtime - started ))s BEFORE its binary was built"
        echo "  → you are running STALE code; the launch did not replace the old process"
        return 1
    fi
    echo "  ✓ $label (pid $pid) is running the build just made"
    return 0
}

echo "▶ verifying the running processes match the build..."
VERIFY_OK=true
verify_running "dev GUI" "Always Dev.app/Contents/MacOS/Always$" \
    "$DEV_APP/Contents/MacOS/Always" || VERIFY_OK=false
if [ "$SKIP_DAEMON" = false ]; then
    verify_running "dev daemon" "Always Dev.app/Contents/MacOS/always-daemon run" \
        "$DEV_APP/Contents/MacOS/always-daemon" || VERIFY_OK=false
fi

if [ "$VERIFY_OK" != true ]; then
    play "$SOUND_FAIL"
    echo "✗ REBUILD NOT LIVE — do not test, and do not report this as shipped."
    exit 1
fi

# Belt-and-suspenders audit: prove production is still alive. Cheap check,
# catches any future regression where a dev path accidentally kills prod.
if ! pgrep -f "Always.app/Contents/MacOS/Always$" >/dev/null; then
    echo "⚠️  production Always GUI is not running — nothing was killed by"
    echo "    this script, but prod won't come back on its own."
fi

play "$SOUND_UP"
echo "✓ done — Always Dev verified running (production untouched)"
