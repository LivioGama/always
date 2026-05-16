#!/usr/bin/env bash
# Local dev helper: kill → build → bundle → launch the Always app.
# Plays a short macOS system sound at each lifecycle marker so you can hear
# the state of the rebuild while looking at logs / another app.
#
# Usage:
#   scripts/dev-rebuild.sh                # debug build (default — transcripts visible)
#   scripts/dev-rebuild.sh release        # release build (transcripts hidden)
#   ALWAYS_REBUILD_SILENT=1 scripts/dev-rebuild.sh   # mute sounds

set -euo pipefail

PROFILE="${1:-debug}"
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

echo "▶ killing AlwaysApp..."
play "$SOUND_KILL"
pkill -f AlwaysApp 2>/dev/null || true
sleep 0.3   # let processes actually die before rebuild

echo "▶ cargo build ($PROFILE)..."
case "$PROFILE" in
    debug)   cargo build --lib --bin always ;;
    release) cargo build --release --lib --bin always ;;
    *) echo "unknown profile: $PROFILE (use 'debug' or 'release')"; exit 2 ;;
esac
play "$SOUND_COMPILED"

echo "▶ Swift bundle + deploy..."
(
    cd AlwaysApp
    ALWAYS_BUILD_PROFILE="$PROFILE" ./build.sh
)

# CRITICAL: remove the intermediate project-dir bundle. If it lives on,
# LaunchServices re-discovers it on every seed-rescan and `open -a
# AlwaysApp` may resolve to it instead of /Applications. Three duplicate
# "AlwaysApp" entries in System Settings → Control Center →
# "Allow in the Menu Bar" came from this — the resulting status-item
# registration conflict made the menu-bar icon invisible.
echo "▶ removing intermediate bundle..."
rm -rf "$REPO_ROOT/AlwaysApp/AlwaysApp.app"

echo "▶ launching AlwaysApp..."
# Use explicit path, not `open -a AlwaysApp` (name lookup can resolve
# to a stale LaunchServices entry).
open /Applications/AlwaysApp.app
sleep 1.5
play "$SOUND_UP"
echo "✓ done"
