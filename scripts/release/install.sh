#!/usr/bin/env bash
# ──────────────────────────────────────────────────────────────────────────────
# Always — installer for macOS and Linux.
#
#   curl -fsSL https://github.com/LivioGama/always/releases/latest/download/install.sh | bash
#   curl -fsSL https://raw.githubusercontent.com/LivioGama/always/main/scripts/install-always.sh | bash
#
# (install.sh is published as an asset of every release; the second form is a
# thin wrapper that fetches it.)
#
# Resolves the latest release tag through the releases/latest redirect (no API
# rate limit), downloads always-<tag>-<target>.* and its .sha256 sibling,
# verifies the checksum, then installs:
#   macOS  Always.app -> /Applications (or ~/Applications if not writable), opens it
#   Linux  .deb (apt) / .rpm (dnf, yum) / else tar.gz -> ~/.local/bin
#
# Environment overrides:
#   ALWAYS_VERSION     pin a release tag (e.g. v0.1.0); default: latest release
#   ALWAYS_BASE_URL    fetch assets from here instead of GitHub (http(s):// or
#                      file:///dir) -- for testing against local files; needs
#                      ALWAYS_VERSION
#   ALWAYS_METHOD      Linux only: auto (default) | deb | rpm | tar
#   ALWAYS_APP_DIR     macOS only: directory to install Always.app into
#   ALWAYS_PREFIX      Linux tar install prefix (default: ~/.local)
#   ALWAYS_NO_LAUNCH   macOS: set to 1 to skip opening the app
#   ALWAYS_SYSTEMD     Linux: 1 = enable the user service, 0 = skip; default: ask
#
# Self-update mode (`always update`, or the app's "Install update"). The
# updater has ALREADY downloaded the release files and verified their minisign
# signatures against the public key built into the app; this script then must
# not touch the network:
#   ALWAYS_ARCHIVE       path of the already-downloaded archive/package. Needs
#                        ALWAYS_VERSION; its file name must be the one this
#                        platform/method installs; <path>.sha256 must sit next
#                        to it and is still checked. Changes: macOS relaunches
#                        the app only if it was running; Linux restarts an
#                        active user service instead of asking about one.
#   ALWAYS_UPDATER_PID   pid of an `always update` process that must survive
#                        this script quitting the running copy (macOS)
# ──────────────────────────────────────────────────────────────────────────────

set -euo pipefail

REPO="LivioGama/always"
RELEASES_URL="https://github.com/${REPO}/releases"

log()  { printf '>>> %s\n' "$*"; }
ok()   { printf 'OK  %s\n' "$*"; }
warn() { printf 'WARN %s\n' "$*" >&2; }
fail() { printf 'ERROR %s\n' "$*" >&2; exit 1; }

TMP="$(mktemp -d "${TMPDIR:-/tmp}/always-install.XXXXXX")"
cleanup() { rm -rf "$TMP"; }
trap cleanup EXIT
trap 'exit 130' INT TERM
# Readable by apt's sandbox user (_apt) so `apt-get install ./file.deb` is quiet.
chmod 755 "$TMP"

# ── Detect OS / arch ─────────────────────────────────────────────────────────
case "$(uname -s)" in
  Darwin) OS="macos" ;;
  Linux)  OS="linux" ;;
  *)      fail "Unsupported OS: $(uname -s). On Windows use: irm https://raw.githubusercontent.com/${REPO}/main/scripts/install.ps1 | iex" ;;
esac

case "$(uname -m)" in
  arm64|aarch64) ARCH="aarch64" ;;
  x86_64|amd64)  ARCH="x86_64" ;;
  *)             fail "Unsupported architecture: $(uname -m)" ;;
esac

# An x86_64 shell under Rosetta still runs on Apple Silicon hardware.
if [[ "$OS" == "macos" && "$ARCH" == "x86_64" ]] \
    && [[ "$(sysctl -n hw.optional.arm64 2>/dev/null || echo 0)" == "1" ]]; then
  ARCH="aarch64"
fi

if [[ "$OS" == "macos" ]]; then
  TARGET="${ARCH}-apple-darwin"
else
  TARGET="${ARCH}-unknown-linux-gnu"
fi

# ── Resolve the tag + base URL ───────────────────────────────────────────────
no_release() {
  fail "$1
      No release with these assets exists yet (or the version is wrong).
      See ${RELEASES_URL}"
}

ARCHIVE_OVERRIDE="${ALWAYS_ARCHIVE:-}"
if [[ -n "$ARCHIVE_OVERRIDE" ]]; then
  [[ -n "${ALWAYS_VERSION:-}" ]] \
    || fail "ALWAYS_ARCHIVE needs ALWAYS_VERSION (the tag the archive belongs to)."
  [[ -f "$ARCHIVE_OVERRIDE" ]] || fail "ALWAYS_ARCHIVE does not exist: ${ARCHIVE_OVERRIDE}"
fi

TAG="${ALWAYS_VERSION:-}"
if [[ -n "$TAG" ]]; then
  [[ "$TAG" == v* ]] || TAG="v${TAG}"
elif [[ -n "${ALWAYS_BASE_URL:-}" ]]; then
  fail "ALWAYS_BASE_URL needs ALWAYS_VERSION (the tag the local assets are named after)."
else
  # The releases/latest page redirects to .../releases/tag/<tag>, or to
  # .../releases when there is no release. Not api.github.com: its anonymous
  # limit (60/h per IP) is exhausted by shared addresses.
  log "Resolving the latest release"
  LATEST_URL="$(curl -fsSLI -o /dev/null -w '%{url_effective}' "${RELEASES_URL}/latest")" \
    || fail "Could not reach ${RELEASES_URL}/latest"
  case "$LATEST_URL" in
    */releases/tag/v?*) TAG="${LATEST_URL##*/releases/tag/}" ;;
    *) no_release "No published release found for ${REPO}." ;;
  esac
fi

if [[ -n "${ALWAYS_BASE_URL:-}" ]]; then
  BASE_URL="${ALWAYS_BASE_URL%/}"
else
  BASE_URL="${RELEASES_URL}/download/${TAG}"
fi
PREFIX_NAME="always-${TAG}-${TARGET}"
log "Always ${TAG} (${TARGET})"

# ── Download + verify helpers ────────────────────────────────────────────────
fetch() { # fetch <asset-name> <dest>
  curl -fsSL --retry 2 -o "$2" "${BASE_URL}/$1"
}

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    fail "Neither sha256sum nor shasum found; cannot verify the download."
  fi
}

# fetch_verified <asset-name> -> sets VERIFIED (local path of the checked file)
VERIFIED=""
fetch_verified() {
  local name="$1" dest="$TMP/$1" want got
  if [[ -n "$ARCHIVE_OVERRIDE" ]]; then
    # Self-update: the updater downloaded and signature-verified this file.
    [[ "$(basename "$ARCHIVE_OVERRIDE")" == "$name" ]] \
      || fail "ALWAYS_ARCHIVE is $(basename "$ARCHIVE_OVERRIDE"), but this install needs ${name}."
    dest="$ARCHIVE_OVERRIDE"
    [[ -f "${dest}.sha256" ]] \
      || fail "Missing ${dest}.sha256; refusing to install without its checksum."
    log "Using the already-verified ${name}"
  else
    log "Downloading ${name}"
    fetch "$name" "$dest" 2>/dev/null || no_release "Could not download ${BASE_URL}/${name}"
    fetch "${name}.sha256" "${dest}.sha256" 2>/dev/null \
      || fail "Could not download ${BASE_URL}/${name}.sha256; refusing to install without its checksum."
  fi
  want="$(awk 'NR == 1 {print $1}' "${dest}.sha256")"
  [[ "$want" =~ ^[0-9a-f]{64}$ ]] || fail "Could not read a SHA-256 digest from ${name}.sha256"
  got="$(sha256_of "$dest")"
  [[ "$want" == "$got" ]] || fail "Checksum mismatch for ${name}
      expected ${want}
      got      ${got}"
  ok "SHA256 verified: ${name}"
  VERIFIED="$dest"
}

# ── macOS ────────────────────────────────────────────────────────────────────
install_macos() {
  [[ "$ARCH" == "aarch64" ]] \
    || fail "Always for macOS requires Apple Silicon (arm64); this Mac is $(uname -m)."

  fetch_verified "${PREFIX_NAME}.tar.gz"
  local archive="$VERIFIED"

  local app_dir="${ALWAYS_APP_DIR:-}"
  if [[ -z "$app_dir" ]]; then
    if [[ -w /Applications ]]; then app_dir="/Applications"; else app_dir="$HOME/Applications"; fi
  fi
  mkdir -p "$app_dir"
  local dest="$app_dir/Always.app"

  mkdir "$TMP/extract"
  tar -xzf "$archive" -C "$TMP/extract"
  local src="$TMP/extract/${PREFIX_NAME}/Always.app"
  [[ -d "$src" ]] || fail "Archive does not contain ${PREFIX_NAME}/Always.app"

  # Quit a running copy that lives in the target location before replacing it.
  local was_running=0 pid
  if [[ -d "$dest" ]] && pgrep -f "${dest}/Contents/MacOS/Always" >/dev/null 2>&1; then
    was_running=1
    log "Quitting the running Always"
    osascript -e 'tell application "Always" to quit' >/dev/null 2>&1 || true
    for _ in 1 2 3 4 5 6 7 8 9 10; do
      pgrep -f "${dest}/Contents/MacOS/Always" >/dev/null 2>&1 || break
      sleep 0.5
    done
    # What is left of that bundle (the daemon, helpers). Never this script,
    # nor the `always update` process that started it: ending that one would
    # cut off the output the user is reading.
    for pid in $(pgrep -f "${dest}/Contents/MacOS/" 2>/dev/null || true); do
      [[ "$pid" == "$$" || "$pid" == "${ALWAYS_UPDATER_PID:-}" ]] && continue
      kill "$pid" >/dev/null 2>&1 || true
    done
  fi

  rm -rf "$dest"
  # ditto preserves the code signature, symlinks and resource forks.
  ditto "$src" "$dest"
  # curl does not quarantine, but a browser-downloaded archive would.
  xattr -dr com.apple.quarantine "$dest" 2>/dev/null || true
  codesign --verify --deep --strict "$dest" 2>/dev/null \
    || warn "Code signature check failed for ${dest}"
  ok "Installed ${dest}"

  # A self-update relaunches only what it quit; a fresh install opens the app.
  if [[ "${ALWAYS_NO_LAUNCH:-0}" != "1" ]] && { [[ -z "$ARCHIVE_OVERRIDE" ]] || [[ "$was_running" == 1 ]]; }; then
    log "Opening Always"
    open "$dest"
  fi
  log "macOS will ask for Microphone and Accessibility access on first use."
}

# ── Linux ────────────────────────────────────────────────────────────────────
SUDO=""
need_root() {
  if [[ "$(id -u)" -eq 0 ]]; then SUDO=""; return 0; fi
  if command -v sudo >/dev/null 2>&1; then SUDO="sudo"; return 0; fi
  return 1
}

install_deb() {
  fetch_verified "${PREFIX_NAME}.deb"
  local pkg="$VERIFIED"
  log "Installing with apt (needs root)"
  if ! $SUDO apt-get install -y "$pkg"; then
    log "Retrying after apt-get update"
    $SUDO apt-get update
    $SUDO apt-get install -y "$pkg"
  fi
  ok "Installed via .deb: /usr/bin/always"
  UNIT_KIND="system"
}

install_rpm() {
  fetch_verified "${PREFIX_NAME}.rpm"
  local mgr pkg="$VERIFIED"
  if command -v dnf >/dev/null 2>&1; then mgr="dnf"; else mgr="yum"; fi
  log "Installing with ${mgr} (needs root)"
  $SUDO "$mgr" install -y "$pkg"
  ok "Installed via .rpm: /usr/bin/always"
  UNIT_KIND="system"
}

install_tar() {
  fetch_verified "${PREFIX_NAME}.tar.gz"
  local archive="$VERIFIED" prefix bindir
  prefix="${ALWAYS_PREFIX:-$HOME/.local}"
  bindir="$prefix/bin"
  mkdir "$TMP/extract"
  tar -xzf "$archive" -C "$TMP/extract"
  local src="$TMP/extract/${PREFIX_NAME}"
  [[ -x "$src/bin/always" ]] || fail "Archive does not contain ${PREFIX_NAME}/bin/always"
  mkdir -p "$bindir"
  install -m 755 "$src/bin/always" "$bindir/always"
  ok "Installed ${bindir}/always"
  # The .deb/.rpm declare their dependencies; a tarball install cannot.
  if command -v ldd >/dev/null 2>&1 && ldd "$bindir/always" 2>&1 | grep -q 'not found'; then
    warn "Missing shared libraries for ${bindir}/always:"
    ldd "$bindir/always" 2>&1 | grep 'not found' >&2 || true
    warn "Debian/Ubuntu runtime libraries: 'sudo apt install libasound2t64 libgcc-s1 libstdc++6 libx11-6 libxtst6'."
    warn "Fedora runtime libraries: 'sudo dnf install alsa-lib libgcc libstdc++ libX11 libXtst'."
  elif ! "$bindir/always" --version </dev/null >/dev/null 2>&1; then
    warn "${bindir}/always did not start. It needs glibc 2.39 or newer (Ubuntu 24.04+, Fedora 40+, Debian 13+)."
  fi
  case ":$PATH:" in
    *":$bindir:"*) ;;
    *) warn "${bindir} is not on your PATH. Add: export PATH=\"${bindir}:\$PATH\"" ;;
  esac
  if [[ -f "$src/share/always-daemon.service" ]]; then
    local unit_dir="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
    mkdir -p "$unit_dir"
    sed "s|^ExecStart=.*|ExecStart=${bindir}/always run|" \
      "$src/share/always-daemon.service" > "$unit_dir/always-daemon.service"
    UNIT_KIND="user"
  fi
}

offer_systemd() {
  [[ -n "$UNIT_KIND" ]] || return 0
  command -v systemctl >/dev/null 2>&1 || return 0
  if ! systemctl --user show-environment >/dev/null 2>&1; then
    log "No systemd user session detected; start manually with: always run"
    return 0
  fi
  systemctl --user daemon-reload >/dev/null 2>&1 || true

  local answer="${ALWAYS_SYSTEMD:-ask}"
  if [[ "$answer" == "ask" ]]; then
    if { true </dev/tty; } 2>/dev/null; then
      printf 'Start Always automatically at login (systemctl --user)? [y/N] ' >/dev/tty
      read -r answer </dev/tty || answer="n"
    else
      answer="n"
    fi
  fi
  case "$answer" in
    1|y|Y|yes|YES)
      systemctl --user enable --now always-daemon.service
      ok "Enabled always-daemon.service (logs: journalctl --user -u always-daemon -f)"
      ;;
    *)
      log "To start at login later: systemctl --user enable --now always-daemon.service"
      ;;
  esac
}

# Self-update on Linux: the new binary only runs after a restart, so restart
# the user service when (and only when) it is already active.
restart_service_if_active() {
  command -v systemctl >/dev/null 2>&1 || return 0
  systemctl --user is-active --quiet always-daemon.service >/dev/null 2>&1 || return 0
  systemctl --user daemon-reload >/dev/null 2>&1 || true
  log "Restarting always-daemon.service"
  systemctl --user restart always-daemon.service \
    || warn "Could not restart always-daemon.service; restart it to use the new version."
}

install_linux() {
  if ldd --version 2>&1 | grep -qi musl; then
    fail "musl libc detected (Alpine). The Linux builds target glibc 2.39+."
  fi

  local method="${ALWAYS_METHOD:-auto}"
  if [[ "$method" == "auto" ]]; then
    if command -v apt-get >/dev/null 2>&1 && command -v dpkg >/dev/null 2>&1 && need_root; then
      method="deb"
    elif { command -v dnf >/dev/null 2>&1 || command -v yum >/dev/null 2>&1; } && need_root; then
      method="rpm"
    else
      method="tar"
    fi
  elif [[ "$method" == "deb" || "$method" == "rpm" ]]; then
    need_root || fail "ALWAYS_METHOD=${method} needs root or sudo."
  fi

  UNIT_KIND=""
  case "$method" in
    deb) install_deb ;;
    rpm) install_rpm ;;
    tar) install_tar ;;
    *)   fail "Unknown ALWAYS_METHOD: ${method} (use auto, deb, rpm or tar)" ;;
  esac
  if [[ -n "$ARCHIVE_OVERRIDE" ]]; then restart_service_if_active; else offer_systemd; fi
  log "Run 'always --help' to get started. Docs: https://liviogama.github.io/always/"
}

UNIT_KIND=""
if [[ "$OS" == "macos" ]]; then install_macos; else install_linux; fi
