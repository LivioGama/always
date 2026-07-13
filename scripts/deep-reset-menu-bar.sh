#!/usr/bin/env bash
# OS-level menu bar cache reset for Always on macOS 26 (Tahoe).
# Fixes empty Control Center displayableInfos and stale NSStatusItem keys.
set -euo pipefail

BUNDLE_ID="${ALWAYS_BUNDLE_ID:-com.always.v2}"
HOME_DIR="${HOME}"

echo "== Always deep menu bar reset =="

pkill -f "/Applications/Always.app/Contents/MacOS/Always" 2>/dev/null || true
pkill -TERM -f "always-daemon run" 2>/dev/null || true
sleep 1
pkill -KILL -f "always-daemon run" 2>/dev/null || true

echo "→ Removing Control Center menu-extra registry (ByHost)…"
shopt -s nullglob
for f in "$HOME_DIR/Library/Preferences/ByHost/com.apple.controlcenter.displayablemenuextras."*.plist; do
  rm -f "$f" && echo "  deleted $(basename "$f")"
done
shopt -u nullglob

echo "→ Purging app preference domains…"
for domain in "$BUNDLE_ID" com.always com.alwaysapp com.alwaysapp.daemon; do
    if [ -f "$HOME_DIR/Library/Preferences/${domain}.plist" ]; then
        # Strip only menu-bar keys; if domain is legacy, delete whole file.
        if [ "$domain" != "$BUNDLE_ID" ] && [ "$domain" != "com.always" ]; then
      rm -f "$HOME_DIR/Library/Preferences/${domain}.plist"
      echo "  removed ${domain}.plist"
    else
      for key in $(defaults read "$domain" 2>/dev/null | grep -E '^\s+"NSStatusItem|Always_StatusItem|Always_MenuBar' | sed 's/ =.*//' | tr -d ' "' || true); do
        defaults delete "$domain" "$key" 2>/dev/null || true
      done
      defaults write "$domain" Always_MenuBar_LegacyCachePurged -bool false
      defaults write "$domain" Always_StatusItem_AutosaveVersion -int 1
      defaults write "$domain" Always_ShowSettingsOnLaunch -bool true
      defaults write "$domain" Always_RunOSMenuBarDeepReset -bool true
      echo "  scrubbed $domain menu-bar keys"
    fi
  fi
done

echo "→ Clearing current-host NSGlobalDomain status-item keys…"
CURRENT_HOST_KEYS=$(defaults -currentHost read 2>/dev/null | grep -E '^\s+"NSStatusItem' | sed 's/ =.*//' | tr -d ' "' || true)
while IFS= read -r key; do
  [ -z "$key" ] && continue
  case "$key" in
    *Always*|*always*|*Item-0*|*Item-1*)
      defaults -currentHost delete "$key" 2>/dev/null && echo "  -currentHost delete $key" || true
      ;;
  esac
done <<< "$CURRENT_HOST_KEYS"

for v in $(seq 1 20); do
  for prefix in "NSStatusItem Visible " "NSStatusItem VisibleCC " "NSStatusItem Preferred Position "; do
    defaults delete "$BUNDLE_ID" "${prefix}Always_Main_v${v}" 2>/dev/null || true
    defaults delete "$BUNDLE_ID" "${prefix}Item-0" 2>/dev/null || true
    defaults delete "$BUNDLE_ID" "${prefix}Item-1" 2>/dev/null || true
  done
done

killall ControlCenter 2>/dev/null || true
sleep 1
touch /Applications/Always.app 2>/dev/null || true
killall ControlCenter 2>/dev/null || true
sleep 0.5

echo "→ Launching Always + Menu Bar settings…"
open -a Always
sleep 2
open "always://settings" 2>/dev/null || true
open "x-apple.systempreferences:com.apple.ControlCenter-Settings.extension?MenuBar"

echo "Done. In System Settings → Menu Bar, turn Always ON (check the « overflow too)."
