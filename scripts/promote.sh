#!/usr/bin/env bash
# MANUAL production promotion: kill → build → deploy → launch the REAL
# /Applications/Always.app. This is the ONLY script allowed to touch the
# production install — agents and the dev loop must never run it.
#
# Run this yourself when you want the current code to become your daily
# driver. It replaces the app you dictate with, so expect a brief
# dictation outage while it restarts.
#
# Usage:
#   scripts/promote.sh                # debug build (default)
#   scripts/promote.sh release        # release build (transcripts hidden)

set -euo pipefail

PROFILE="debug"
for arg in "$@"; do
    case "$arg" in
        release) PROFILE="release" ;;
        debug)   PROFILE="debug" ;;
        *) echo "unknown argument: $arg (use 'debug' or 'release')"; exit 2 ;;
    esac
done

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

echo "▶ PROMOTING to production — Always will restart and dictation will"
echo "  pause briefly. Cancel now (Ctrl-C) if this was not intentional."
sleep 2

echo "▶ killing production Always..."
pkill -9 -f "Always.app/Contents/MacOS/Always" 2>/dev/null || true
for _pat in "Always.app/Contents/MacOS/always-daemon run" "always run"; do
    pkill -TERM -f "$_pat" 2>/dev/null || true
done
sleep 0.5
for _pat in "Always.app/Contents/MacOS/always-daemon run" "always run"; do
    pkill -KILL -f "$_pat" 2>/dev/null || true
done
sleep 0.2

echo "▶ cargo build ($PROFILE)..."
case "$PROFILE" in
    debug)   env -u CARGO_INCREMENTAL GGML_CCACHE=OFF cargo build --lib --bin always --features local-stt ;;
    release) env -u CARGO_INCREMENTAL GGML_CCACHE=OFF cargo build --release --lib --bin always --features local-stt ;;
esac
touch "$REPO_ROOT/.rust_build_timestamp"

echo "▶ Swift bundle + deploy (production Always.app)..."
(
    cd Always
    ALWAYS_BUILD_PROFILE="$PROFILE" ./build.sh
)

# Remove the intermediate project-dir bundle — a stray copy steals
# LaunchServices resolution for `open -a Always`.
rm -rf "$REPO_ROOT/Always/Always.app"

echo "▶ launching Always..."
open /Applications/Always.app
sleep 3

verify_running() {
    local label="$1" pattern="$2" binary="$3"
    local pid
    pid=$(pgrep -f "$pattern" | head -1)
    if [ -z "$pid" ]; then
        echo "✗ $label is NOT running after launch"
        return 1
    fi
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
        echo "  → you are running STALE code; the promotion did not replace the old process"
        return 1
    fi
    echo "  ✓ $label (pid $pid) is running the promoted build"
    return 0
}

echo "▶ verifying the running processes match the promoted build..."
VERIFY_OK=true
verify_running "GUI" "Always.app/Contents/MacOS/Always$" \
    "/Applications/Always.app/Contents/MacOS/Always" || VERIFY_OK=false
verify_running "daemon" "Always.app/Contents/MacOS/always-daemon run" \
    "/Applications/Always.app/Contents/MacOS/always-daemon" || VERIFY_OK=false

if [ "$VERIFY_OK" != true ]; then
    echo "✗ PROMOTION NOT LIVE — production may be down; check and relaunch."
    exit 1
fi

echo "✓ done — production Always is running the promoted build"
