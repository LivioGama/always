#!/usr/bin/env bash
# Purge stale macOS menu-bar cache for Always and relaunch.
set -euo pipefail

echo "Stopping Always…"
pkill -f "/Applications/Always.app/Contents/MacOS/Always" 2>/dev/null || true
pkill -TERM -f "always-daemon run" 2>/dev/null || true
pkill -TERM -f "always run" 2>/dev/null || true
sleep 1
pkill -KILL -f "always-daemon run" 2>/dev/null || true
pkill -KILL -f "always run" 2>/dev/null || true

echo "Purging legacy menu-bar preferences…"
for key in \
  "NSStatusItem Visible Item-0" \
  "NSStatusItem VisibleCC Item-0" \
  "NSStatusItem Preferred Position Item-0" \
  "NSStatusItem Visible Item-1" \
  "NSStatusItem VisibleCC Item-1" \
  "NSStatusItem Preferred Position Item-1"; do
  defaults delete com.always "$key" 2>/dev/null || true
done
for v in $(seq 1 20); do
  for prefix in "NSStatusItem Visible " "NSStatusItem VisibleCC " "NSStatusItem Preferred Position "; do
    defaults delete com.always "${prefix}Always_Main_v${v}" 2>/dev/null || true
  done
done
defaults write com.always Always_MenuBar_LegacyCachePurged -bool true
defaults write com.always Always_ShowSettingsOnLaunch -bool true
rm -f ~/Library/Preferences/com.alwaysapp.plist 2>/dev/null || true

killall ControlCenter 2>/dev/null || true
sleep 1

if [[ -d /Applications/Always.app ]]; then
  echo "Opening Always Settings (always://settings)…"
  open -a Always
  sleep 2
  open "always://settings" 2>/dev/null || true
  open "x-apple.systempreferences:com.apple.ControlCenter-Settings.extension?MenuBar"
else
  echo "Always.app not in /Applications — run: cd Always && ./build.sh"
  exit 1
fi

echo "Done. Check the menu bar (and the << overflow). Enable Always in Menu Bar settings."
