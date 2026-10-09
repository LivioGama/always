#!/usr/bin/env bash
# ──────────────────────────────────────────────────────────────────────────────
# Always — One-line installer for macOS and Linux
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/LivioGama/always/main/scripts/install-always.sh | bash
#
# Or just copy-paste the commands below for your platform:
#
# macOS:
#   brew install --cask LivioGama/tap/always
#
# Linux:
#   sudo apt install libwebkit2gtk-4.1-dev libayatana-appindicator3-dev
#   curl -LO https://github.com/LivioGama/always/releases/latest/download/always-*.deb
#   sudo dpkg -i always-*.deb
#
# Windows:
#   winget install LivioGama.Always   (or download .msi/.exe from releases)
# ──────────────────────────────────────────────────────────────────────────────

set -euo pipefail

REPO="LivioGama/always"

log()   { echo ">>> $*"; }
ok()    { echo "✓ $*"; }
warn()  { echo "⚠ $*"; }
fail()  { echo "✗ $*"; exit 1; }

# ── Detect OS ────────────────────────────────────────────────────────────────
case "$(uname -s)" in
  Darwin)  OS="macos" ;;
  Linux)   OS="linux" ;;
  *)       fail "Unsupported OS: $(uname -s). Install manually from releases." ;;
esac

case "$(uname -m)" in
  arm64|aarch64) ARCH="aarch64" ;;
  x86_64)        ARCH="x86_64" ;;
  *)             fail "Unsupported arch: $(uname -m)" ;;
esac

# ── macOS ────────────────────────────────────────────────────────────────────
if [[ "$OS" == "macos" ]]; then
  log "Installing Always on macOS ($ARCH)…"

  if command -v brew &>/dev/null; then
    brew install --cask LivioGama/tap/always
    ok "Installed via Homebrew. Open /Applications/Always.app"
  else
    # Fallback: download DMG from latest release by listing assets
    GH_JSON=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" 2>/dev/null) || fail "Failed to fetch release info"
    # Match DMG for the current arch (arm64 or x86_64)
    DMG_URL=$(echo "$GH_JSON" | python3 -c "
import sys, json, re
assets = json.load(sys.stdin)['assets']
arch_pat = 'arm64' if '$ARCH' == 'aarch64' else 'x86_64|x64'
for a in assets:
    if re.search(r'\.dmg$', a['name'], re.I):
        print(a['browser_download_url'])
        sys.exit(0)
" 2>/dev/null) || true

    if [[ -z "$DMG_URL" ]]; then
      fail "No DMG found in release. Install via: brew install --cask LivioGama/tap/always"
    fi

    log "Downloading DMG from $DMG_URL"
    curl -fsSL -o /tmp/always.dmg "$DMG_URL" || fail "Download failed. Install via: brew install --cask LivioGama/tap/always"

    # Remove quarantine
    xattr -dr com.apple.quarantine /tmp/always.dmg 2>/dev/null || true
    ok "Quarantine removed"

    # Mount and copy
    MOUNT="/tmp/always-mount"
    hdiutil attach /tmp/always.dmg -quiet -nobrowse -mountpoint "$MOUNT"
    cp -R "$MOUNT/Always.app" /Applications/
    hdiutil detach "$MOUNT" -quiet
    rm -f /tmp/always.dmg
    ok "Installed to /Applications/Always.app"
  fi

  log ""
  log "  Quick start: Open /Applications/Always.app"
  log "  Docs: https://liviogama.github.io/always/"

# ── Linux ────────────────────────────────────────────────────────────────────
elif [[ "$OS" == "linux" ]]; then
  log "Installing Always on Linux ($ARCH)…"

  # Check GUI deps
  if ! dpkg -s libwebkit2gtk-4.1-dev &>/dev/null; then
    warn "GUI dependency missing: libwebkit2gtk-4.1-dev"
    warn "Install: sudo apt install libwebkit2gtk-4.1-dev"
  fi

  # Try DEB first
  if dpkg -l debhelper &>/dev/null || [[ -f /etc/debian_version ]]; then
    GH_JSON=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" 2>/dev/null) || fail "Failed to fetch release info"
    DEB_URL=$(echo "$GH_JSON" | python3 -c "
import sys, json
assets = json.load(sys.stdin)['assets']
for a in assets:
    if a['name'].endswith('.deb'):
        print(a['browser_download_url'])
        sys.exit(0)
" 2>/dev/null) || true

    if [[ -z "$DEB_URL" ]]; then
      fail "No DEB found in release. Try the AppImage path below."
    fi

    log "Downloading DEB from $DEB_URL"
    curl -fsSL -o /tmp/always.deb "$DEB_URL" || fail "DEB download failed"

    sudo dpkg -i /tmp/always.deb 2>/dev/null || sudo apt-get install -f -y -qq 2>/dev/null || true
    rm -f /tmp/always.deb
    ok "Installed via DEB"
    log "  Quick start: Run 'always' from your app menu or terminal"
  else
    # AppImage fallback
    GH_JSON=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" 2>/dev/null) || fail "Failed to fetch release info"
    APPIMAGE_URL=$(echo "$GH_JSON" | python3 -c "
import sys, json
assets = json.load(sys.stdin)['assets']
for a in assets:
    if a['name'].endswith('.AppImage'):
        print(a['browser_download_url'])
        sys.exit(0)
" 2>/dev/null) || true

    if [[ -z "$APPIMAGE_URL" ]]; then
      fail "No AppImage found in release."
    fi

    log "Downloading AppImage from $APPIMAGE_URL"
    curl -fsSL -o /tmp/always.AppImage "$APPIMAGE_URL" || fail "AppImage download failed"

    chmod +x /tmp/always.AppImage
    sudo cp /tmp/always.AppImage /usr/local/bin/always
    rm -f /tmp/always.AppImage
    ok "Installed AppImage to /usr/local/bin/always"
    warn "For best experience, install the DEB: sudo apt install libwebkit2gtk-4.1-dev && sudo dpkg -i always-*.deb"
  fi

  log "  Docs: https://liviogama.github.io/always/"
fi
