#!/bin/bash
# Launch smoke test: the real signed bundle's daemon + helpers, run from a
# scratch bundle as an isolated instance (ALWAYS_INSTANCE=smoke: own socket,
# state dir, logs; no peer handoff; ALWAYS_NO_PASTE=1). Prod is master-paused
# for the window (one mic) and ALWAYS resumed by the trap.
# usage: smoke.sh <built Always.app> [secs]
set -u
D="$(cd "$(dirname "$0")" && pwd)"; SRC="$1"; SECS="${2:-30}"
APP="$D/AlwaysSmoke.app"; PROD_SOCK="$HOME/Library/Caches/Always/always.sock"
SMOKE_SOCK="$HOME/Library/Caches/Always/always-smoke.sock"; LOGDIR="$HOME/Library/Logs/Always-smoke"
PATTERN="AlwaysSmoke.app/Contents/MacOS/"
rm -rf "$APP" "$LOGDIR" "$D/events.jsonl" "$D/daemon.out"; mkdir -p "$APP/Contents/MacOS"
# The daemon is renamed: the production daemon (correctly) reaps any other
# `always-daemon run` it sees as a duplicate of itself. The renamed copy is
# byte-identical, and it also exercises the bundle-path / parent-pid
# self-exclusion instead of the name list.
cp "$SRC/Contents/MacOS/always-daemon" "$APP/Contents/MacOS/always-smoked"
cp "$SRC/Contents/MacOS/always-capture" "$SRC/Contents/MacOS/always-preview-asr" "$APP/Contents/MacOS/"
cp "$SRC/Contents/Info.plist" "$APP/Contents/Info.plist"; P="$APP/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleExecutable always-smoked" -c "Set :CFBundleName AlwaysSmoke" "$P"
/usr/libexec/PlistBuddy -c "Add :LSUIElement bool true" "$P" 2>/dev/null || /usr/libexec/PlistBuddy -c "Set :LSUIElement true" "$P"
for k in SUFeedURL SUPublicEDKey SUEnableAutomaticChecks SUScheduledCheckInterval CFBundleURLTypes; do /usr/libexec/PlistBuddy -c "Delete :$k" "$P" 2>/dev/null; done
codesign --force --deep --sign "Always Local Signing" --identifier com.always.v3 "$APP" >/dev/null 2>&1 || { echo "sign failed"; exit 1; }
stop_smoke() {
  # Only processes inside the scratch AlwaysSmoke.app — can never match prod.
  pgrep -f "$PATTERN" | while read -r pid; do /bin/kill -TERM "$pid" 2>/dev/null; done
  sleep 1
  pgrep -f "$PATTERN" | while read -r pid; do /bin/kill -KILL "$pid" 2>/dev/null; done
}
cleanup() {
  stop_smoke
  echo "prod resume:"; python3 -I "$D/uds.py" "$PROD_SOCK" send '{"type":"SetPaused","data":{"paused":false,"reason":"smoke-test-end"}}'
}
trap cleanup EXIT
# NEVER touches system volume or plays audibly: the sandbox runs with
# ALWAYS_CUE_DRY_RUN=1, so every cue is decoded, gained and rendered OFFLINE
# to a buffer (no output device) and logged — see status_sound.rs.
# Seed the sandbox with COPIES of the user's prefs + voiceprint + focus state
# (read-only on prod), so it runs the real configuration (Apple backend,
# My Voice, per-app allowlist) instead of first-run defaults.
SMOKE_CFG="$HOME/Library/Application Support/always-smoke"; rm -rf "$SMOKE_CFG"; mkdir -p "$SMOKE_CFG"
cp "$HOME/Library/Application Support/always/always.db" "$HOME/Library/Application Support/always/voiceprint.json" "$SMOKE_CFG/"
cp "$HOME/Library/Caches/Always/focus-state.json" "$HOME/Library/Caches/Always/focus-state-smoke.json" 2>/dev/null
AEC_ENV=(); [ -n "${SMOKE_AEC:-}" ] && AEC_ENV=(--env ALWAYS_AEC=$SMOKE_AEC)
echo "prod pause:"; python3 -I "$D/uds.py" "$PROD_SOCK" send '{"type":"SetPaused","data":{"paused":true,"reason":"smoke-test"}}'
open -n --stdout "$D/daemon.out" --stderr "$D/daemon.out" \
  --env ALWAYS_INSTANCE=smoke --env ALWAYS_SKIP_ORPHAN_KILL=1 --env ALWAYS_NO_PASTE=1 --env ALWAYS_CUE_DRY_RUN=1 --env ALWAYS_TRANSCRIPT_STREAM=0 --env ALWAYS_CAPTURE_HEARTBEAT=1 ${AEC_ENV[@]+"${AEC_ENV[@]}"} \
  "$APP" --args run --lang auto --timeout 30
for i in $(seq 1 150); do [ -S "$SMOKE_SOCK" ] && break; sleep 0.1; done
[ -S "$SMOKE_SOCK" ] || { echo "FAIL: smoke socket never appeared"; exit 1; }
python3 -I "$D/uds.py" "$SMOKE_SOCK" listen "$SECS" "$D/events.jsonl" &
LISTENER=$!
sleep 8
# Every status cue through the real bundle path (UDS PlayStatusCue → cue()
# → worker → AVAudioPlayer); check.py asserts a status_sound_played line each.
for cue in listening transcribing done error cancel command; do
  python3 -I "$D/uds.py" "$SMOKE_SOCK" send "{\"type\":\"PlayStatusCue\",\"data\":{\"cue\":\"$cue\"}}" >/dev/null
done
echo "processes during run:"; pgrep -fl "$PATTERN" | sed 's#.*/MacOS/#  #'
wait $LISTENER
