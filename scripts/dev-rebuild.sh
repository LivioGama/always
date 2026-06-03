#!/usr/bin/env bash
# Local dev helper: kill → build → bundle → launch the Always app.
# Plays a short macOS system sound at each lifecycle marker so you can hear
# the state of the rebuild while looking at logs / another app.
#
# Usage:
#   scripts/dev-rebuild.sh                # debug build (default — transcripts visible)
#   scripts/dev-rebuild.sh release        # release build (transcripts hidden)
#   scripts/dev-rebuild.sh --no-daemon    # skip daemon restart (for Swift-only changes)
#   ALWAYS_REBUILD_SILENT=1 scripts/dev-rebuild.sh   # mute sounds

set -euo pipefail

# Parse arguments
SKIP_DAEMON=false
PROFILE="debug"
for arg in "$@"; do
    case "$arg" in
        --no-daemon)
            SKIP_DAEMON=true
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

echo "▶ cargo build ($PROFILE)..."
if [ "$SKIP_DAEMON" = true ]; then
    echo "  (skipped - --no-daemon flag set)"
else
    case "$PROFILE" in
        debug)   cargo build --lib --bin always ;;
        release) cargo build --release --lib --bin always ;;
        *) echo "unknown profile: $PROFILE (use 'debug' or 'release')"; exit 2 ;;
    esac
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
sleep 1.5
play "$SOUND_UP"
echo "✓ done"
