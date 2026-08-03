#!/usr/bin/env bash
# Local dev helper: kill → build → bundle → launch the Always app.
# Plays a short macOS system sound at each lifecycle marker so you can hear
# the state of the rebuild while looking at logs / another app.
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

# ALWAYS kill, even for a Swift-only change.
#
# This used to be gated on `RUST_CHANGED || FORCE_DAEMON`. A Swift-only
# rebuild therefore killed nothing, and the `open` at the end merely
# re-focused the instance that was still running — so the freshly built
# GUI sat in /Applications, never executed, while the script printed
# "✓ done". Measured in a real session: binaries written at 22:46:56, GUI
# still running from 21:07:02. Every Swift fix "shipped" in that window
# was untestable, and the user reasonably concluded nothing had changed.
#
# Restarting the daemon when only Swift moved costs about two seconds.
# Handing someone a build that is not running costs an hour.
if true; then
    echo "▶ killing Always..."
    play "$SOUND_KILL"
    pkill -9 -f "Always.app" 2>/dev/null || true
    pkill -9 -f "/Applications/Always.app" 2>/dev/null || true
    # Stale project-dir bundle can steal LaunchServices resolution.
    pkill -9 -f "Documents/always/Always/Always.app" 2>/dev/null || true

    # Kill the Rust daemon too (unless --no-daemon flag is set)
    # The GUI's applicationWillTerminate handler usually does this, but a hard
    # pkill on Always.app skips it, so the daemon outlives the rebuild and the
    # next launch hits a stale UDS socket / pid file. Send SIGTERM first
    # (lets PidGuard::Drop fire), then SIGKILL if still alive.
    if [ "$SKIP_DAEMON" = false ]; then
        for _pat in "always-daemon run" "always run"; do
          pkill -TERM -f "$_pat" 2>/dev/null || true
        done
        sleep 0.5   # PidGuard::Drop + socket cleanup
        for _pat in "always-daemon run" "always run"; do
          pkill -KILL -f "$_pat" 2>/dev/null || true
        done
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

echo "▶ Swift bundle + deploy..."
(
    cd Always
    ALWAYS_BUILD_PROFILE="$PROFILE" ./build.sh
)

# CRITICAL: remove the intermediate project-dir bundle. If it lives on,
# LaunchServices re-discovers it on every seed-rescan and `open -a
# Always` may resolve to it instead of /Applications. Three duplicate
# "Always" entries in System Settings → Control Center →
# "Allow in the Menu Bar" came from this — the resulting status-item
# registration conflict made the menu-bar icon invisible.
echo "▶ removing intermediate bundle..."
rm -rf "$REPO_ROOT/Always/Always.app"

echo "▶ launching Always..."
# Use explicit path, not `open -a Always` (name lookup can resolve
# to a stale LaunchServices entry).
open /Applications/Always.app
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
verify_running "GUI" "Always.app/Contents/MacOS/Always$" \
    "/Applications/Always.app/Contents/MacOS/Always" || VERIFY_OK=false
verify_running "daemon" "always-daemon run" \
    "/Applications/Always.app/Contents/MacOS/always-daemon" || VERIFY_OK=false

if [ "$VERIFY_OK" != true ]; then
    play "$SOUND_FAIL"
    echo "✗ REBUILD NOT LIVE — do not test, and do not report this as shipped."
    exit 1
fi

play "$SOUND_UP"
echo "✓ done — new build verified running"
