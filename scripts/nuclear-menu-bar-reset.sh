#!/usr/bin/env bash
# Nuclear menu bar recovery: purge uninstalled Ice + all Always menu-bar caches,
# then launch with a fresh bundle ID (default com.always.v2).
set -euo pipefail

BUNDLE_ID="${ALWAYS_BUNDLE_ID:-com.always.v2}"
HOME_DIR="${HOME}"
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

echo "== Nuclear menu bar reset (Ice cleanup + Always) =="

pkill -f "/Applications/Always.app" 2>/dev/null || true
pkill -f "Desktop/Always.app" 2>/dev/null || true
pkill -TERM -f "always-daemon run" 2>/dev/null || true
sleep 1
pkill -KILL -f "always-daemon run" 2>/dev/null || true

echo "→ Removing leftover Ice (app uninstalled but prefs often remain)…"
defaults delete com.jordanbaird.Ice 2>/dev/null || true
rm -f "$HOME_DIR/Library/Preferences/com.jordanbaird.Ice.plist"
rm -rf "$HOME_DIR/Library/Application Support/Ice" 2>/dev/null || true
rm -rf "$HOME_DIR/Library/Caches/com.jordanbaird.Ice" 2>/dev/null || true
rm -rf "$HOME_DIR/Library/HTTPStorages/com.jordanbaird.Ice" 2>/dev/null || true
rm -f "$HOME_DIR/Library/LaunchAgents/com.jordanbaird.Ice.plist" 2>/dev/null || true
rm -f "$HOME_DIR/Library/LaunchDaemons/com.jordanbaird.Ice.plist" 2>/dev/null || true
# Ice-specific keys only (never match *ice* — that hits identityservices, imservice, etc.).
for key in \
  "com.jordanbaird.Ice" \
  "IceMenuBar" \
  "IceMenuBarAppearance" \
  "MenuBarAppearanceConfigurationV2"; do
  defaults -currentHost delete "$key" 2>/dev/null && echo "  -currentHost delete $key" || true
done

echo "→ Removing Control Center menu-extra registry (ByHost)…"
shopt -s nullglob
for f in "$HOME_DIR/Library/Preferences/ByHost/com.apple.controlcenter.displayablemenuextras."*.plist; do
  rm -f "$f" && echo "  deleted $(basename "$f")"
done
shopt -u nullglob

echo "→ Purging Always preference domains (all known bundle IDs)…"
for domain in com.always com.always.v2 com.alwaysapp com.alwaysapp.daemon; do
  if [ -f "$HOME_DIR/Library/Preferences/${domain}.plist" ]; then
    if [ "$domain" = "com.always" ] || [ "$domain" = "com.always.v2" ]; then
      for key in $(defaults read "$domain" 2>/dev/null | grep -E '^\s+"NSStatusItem|Always_StatusItem|Always_MenuBar' | sed 's/ =.*//' | tr -d ' "' || true); do
        defaults delete "$domain" "$key" 2>/dev/null || true
      done
      defaults write "$domain" Always_MenuBar_LegacyCachePurged -bool false
      defaults write "$domain" Always_StatusItem_AutosaveVersion -int 1
      defaults write "$domain" Always_MenuBar_OSResetVersion -int 99
      defaults write "$domain" Always_ShowSettingsOnLaunch -bool true
      defaults write "$domain" "NSStatusItem Visible Always_Main_v1" -bool true
      defaults write "$domain" "NSStatusItem VisibleCC Always_Main_v1" -bool true
      echo "  scrubbed + re-seeded $domain"
    else
      rm -f "$HOME_DIR/Library/Preferences/${domain}.plist"
      echo "  removed ${domain}.plist"
    fi
  else
    if [ "$domain" = "com.always.v2" ]; then
      defaults write "$domain" Always_StatusItem_AutosaveVersion -int 1
      defaults write "$domain" Always_MenuBar_OSResetVersion -int 99
      defaults write "$domain" Always_ShowSettingsOnLaunch -bool true
      defaults write "$domain" "NSStatusItem Visible Always_Main_v1" -bool true
      defaults write "$domain" "NSStatusItem VisibleCC Always_Main_v1" -bool true
      echo "  created fresh $domain domain"
    fi
  fi
done

echo "→ Clearing current-host NSStatusItem keys (Always + legacy)…"
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
    for name in "Always_Main_v${v}" Item-0 Item-1; do
      defaults delete com.always "${prefix}${name}" 2>/dev/null || true
      defaults delete com.always.v2 "${prefix}${name}" 2>/dev/null || true
    done
  done
done

echo "→ Restarting Control Center…"
killall ControlCenter 2>/dev/null || true
sleep 1
if [ -d "/Applications/Always.app" ]; then
  touch /Applications/Always.app
fi
killall ControlCenter 2>/dev/null || true
sleep 0.5

if [ ! -d "/Applications/Always.app" ]; then
  echo "⚠️  /Applications/Always.app missing — run: cd $REPO_ROOT/Always && ./build.sh"
  exit 1
fi

echo "→ Launching Always (bundle $BUNDLE_ID) + Menu Bar settings…"
open /Applications/Always.app
sleep 2
open "always://settings" 2>/dev/null || true
open "x-apple.systempreferences:com.apple.ControlCenter-Settings.extension?MenuBar"

echo ""
echo "Done."
echo "  • Ice leftovers removed"
echo "  • Fresh app identity: $BUNDLE_ID (re-grant Microphone + Accessibility if prompted)"
echo "  • Enable Always in System Settings → Menu Bar (and « overflow)"
echo "  • On notched Macs also check LEFT of the notch for the Always label"
