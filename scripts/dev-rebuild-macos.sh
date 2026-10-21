#!/usr/bin/env bash
# Local dev loop: kill → build → bundle → launch the DEV app only.
#
# Builds "Always Dev.app" (bundle id com.always.v3.dev) and deploys it to
# /Applications/Always Dev.app — a separate identity with its own config
# dir, socket, pid file, and prefs. The production /Applications/Always.app
# and its running GUI/daemon are NEVER touched by this script.
#
# To ship a build as the production app, run scripts/promote.sh manually.
#
# Usage:
#   scripts/dev-rebuild-macos.sh                # debug build (default — transcripts visible)
#   scripts/dev-rebuild-macos.sh release        # release build (transcripts hidden)
#   scripts/dev-rebuild-macos.sh --no-daemon    # skip daemon restart (for Swift-only changes)
#   scripts/dev-rebuild-macos.sh --force-daemon # force daemon restart even if Rust unchanged
#   scripts/dev-rebuild-macos.sh --no-tauri      # skip always-gui + always-tauri rebuild (daemon-only change)
#   ALWAYS_REBUILD_SILENT=1 scripts/dev-rebuild-macos.sh   # mute sounds

set -euo pipefail

# Parse arguments
SKIP_DAEMON=false
FORCE_DAEMON=false
SKIP_TAURI=false
PROFILE="debug"
for arg in "$@"; do
    case "$arg" in
        --no-daemon)
            SKIP_DAEMON=true
            ;;
        --force-daemon)
            FORCE_DAEMON=true
            ;;
        --no-tauri)
            SKIP_TAURI=true
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

DEV_APP_NAME="Always Dev"
DEV_APP="/Applications/${DEV_APP_NAME}.app"
DEV_SUPPORT_DIR="$HOME/Library/Application Support/always-dev"
PROD_SUPPORT_DIR="$HOME/Library/Application Support/always"

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

# ALWAYS kill the dev app, even for a Swift-only change (a stale running
# dev GUI makes `open` re-focus the old process and the new build never
# executes — same failure this script exists to prevent).
#
# These patterns are scoped to "Always Dev.app" on purpose: the string
# "Always Dev.app" cannot match the production path "Always.app", and
# broad patterns like `pkill -f "always-daemon run"` would kill the
# PRODUCTION daemon — the exact failure this split exists to prevent.
if true; then
    echo "▶ killing Always Dev (production Always is untouched)..."
    play "$SOUND_KILL"
    pkill -9 -f "Always Dev.app" 2>/dev/null || true

    # A settings app launched from the crate's cargo target dir is the
    # Swift menu bar's dev fallback (MenuBarView.tauriBinaryPath), so the
    # menu action would happily focus it again — stale code. That launch
    # path is not covered by the "Always Dev.app" pattern above. This
    # pattern CANNOT match production: /Applications/Always.app/Contents/
    # MacOS/always-tauri contains no "always_tauri/target" component.
    pkill -9 -f "always_tauri/target/[^/]+/always-tauri" 2>/dev/null || true

    if [ "$SKIP_DAEMON" = false ]; then
        # Dev daemon via its own pid file (covers bundled daemon AND any
        # manual `ALWAYS_INSTANCE=dev always run` from target/).
        if [ -f "$DEV_SUPPORT_DIR/always.pid" ]; then
            _dev_pid="$(cat "$DEV_SUPPORT_DIR/always.pid" 2>/dev/null || true)"
            if [ -n "${_dev_pid:-}" ]; then
                kill -TERM "$_dev_pid" 2>/dev/null || true
                sleep 0.5
                kill -KILL "$_dev_pid" 2>/dev/null || true
            fi
            rm -f "$DEV_SUPPORT_DIR/always.pid"
        fi
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

# ── Settings app (always-gui frontend + always-tauri binary) ────────────────
# The menu bar's “Settings…” action spawns the bundled `always-tauri`
# binary, which serves the React settings UI (Snippets, Vocabulary,
# Update, …) and talks to the dev daemon over its socket. Built and
# installed here so the dev bundle always carries the current settings
# frontend. Skip with --no-tauri.
GUI_DIR="$REPO_ROOT/always-gui"
TAURI_CRATE_DIR="$REPO_ROOT/crates/always_tauri"
TAURI_CONF="$TAURI_CRATE_DIR/tauri.conf.json"
TAURI_BIN="$TAURI_CRATE_DIR/target/$PROFILE/always-tauri"
BUNDLED_TAURI_BIN="$DEV_APP/Contents/MacOS/always-tauri"

# tauri.conf.json sanity check for the macOS layout. The Tauri app must NOT
# reuse the Swift app's bundle identifier: LaunchServices and TCC key mic /
# accessibility grants to the identifier, so a collision would let the
# settings app steal — or silently void — the GUI's permission grants. And
# the settings window the menu action opens must exist and be centered.
check_tauri_conf() {
    if [ ! -f "$TAURI_CONF" ]; then
        echo "✗ tauri.conf.json not found at $TAURI_CONF"
        return 1
    fi
    if ! command -v python3 >/dev/null 2>&1; then
        echo "⚠️  python3 not found — skipping the tauri.conf.json check"
        return 0
    fi
    python3 - "$TAURI_CONF" <<'PY'
import json
import sys

with open(sys.argv[1]) as fh:
    conf = json.load(fh)

problems = []
identifier = conf.get("identifier", "")
if identifier in ("com.always.v3", "com.always.v3.dev"):
    problems.append(
        "identifier %r collides with the Swift app's bundle id "
        "(LaunchServices/TCC would merge the two apps)" % identifier
    )

windows = (conf.get("app") or {}).get("windows") or []
settings = next((w for w in windows if w.get("label") == "settings"), None)
if settings is None:
    problems.append(
        "no window labelled 'settings' — the menu bar action would open nothing"
    )
else:
    if settings.get("center") is not True:
        problems.append('settings window is not centered (set "center": true)')
    if settings.get("visible") is not True:
        print(
            "  (settings window starts hidden — always-gui/App.tsx shows it after "
            "the onboarding check; first-run instances show Setup instead)"
        )

for line in problems:
    print("✗ " + line)
sys.exit(1 if problems else 0)
PY
}

if [ "$SKIP_TAURI" = true ]; then
    echo "▶ skipping settings frontend + always-tauri (--no-tauri)"
else
    echo "▶ checking tauri.conf.json (macOS)…"
    check_tauri_conf || {
        echo "✗ tauri.conf.json failed the macOS sanity check — fix it before rebuilding."
        exit 1
    }

    echo "▶ building settings frontend (always-gui)…"
    (
        cd "$GUI_DIR"
        if command -v pnpm >/dev/null 2>&1; then
            pnpm install --no-frozen-lockfile
            pnpm run build
        else
            npm install
            npm run build
        fi
    )
    if [ ! -f "$GUI_DIR/dist/index.html" ]; then
        echo "✗ always-gui build produced no dist/index.html"
        exit 1
    fi

    # The frontend is EMBEDDED into the always-tauri binary by
    # `tauri::generate_context!`, so a frontend-only change still requires
    # recompiling the crate. Cargo usually notices via include_bytes!
    # rerun-if-changed tracking, but a stale settings UI is exactly the class
    # of silent failure this script exists to prevent — force it when dist
    # is newer than the last binary.
    if [ -x "$TAURI_BIN" ] && [ "$GUI_DIR/dist/index.html" -nt "$TAURI_BIN" ]; then
        echo "  (frontend changed — forcing always-tauri recompile)"
        touch "$TAURI_CRATE_DIR/src/main.rs"
    fi

    echo "▶ building settings app (always-tauri, $PROFILE)…"
    case "$PROFILE" in
        debug)   env -u CARGO_INCREMENTAL cargo build --manifest-path "$TAURI_CRATE_DIR/Cargo.toml" --bin always-tauri ;;
        release) env -u CARGO_INCREMENTAL cargo build --release --manifest-path "$TAURI_CRATE_DIR/Cargo.toml" --bin always-tauri ;;
    esac
    if [ ! -x "$TAURI_BIN" ]; then
        echo "✗ always-tauri binary not found at $TAURI_BIN"
        exit 1
    fi
fi

echo "▶ Swift bundle + deploy (Always Dev)..."
(
    cd Always
    ALWAYS_BUILD_PROFILE="$PROFILE" \
    ALWAYS_APP_NAME="$DEV_APP_NAME" \
    ALWAYS_BUNDLE_ID="com.always.v3.dev" \
    ALWAYS_DEPLOY_PATH="$DEV_APP" \
    ALWAYS_URL_SCHEME="always-dev" \
    ./build.sh
)

# CRITICAL: remove the intermediate project-dir bundle. If it lives on,
# LaunchServices re-discovers it on every seed-rescan and `open` may
# resolve to it instead of /Applications.
echo "▶ removing intermediate bundle..."
rm -rf "$REPO_ROOT/Always/${DEV_APP_NAME}.app"

# Install the settings app binary into the DEPLOYED dev bundle. build.sh
# rsyncs in place (rsync --delete), so the copy happens AFTER the deploy,
# which would otherwise strip a binary added to the project-dir bundle.
# This is what the menu bar's “Settings…” action launches: MenuBarView
# looks for Contents/MacOS/always-tauri inside its own bundle.
# Scoped to "Always Dev.app" — production is never written to here.
if [ "$SKIP_TAURI" = true ]; then
    if [ ! -x "$BUNDLED_TAURI_BIN" ] && [ -x "$TAURI_BIN" ]; then
        echo "  (--no-tauri: installing the previously built always-tauri)"
        cp "$TAURI_BIN" "$BUNDLED_TAURI_BIN"
    fi
else
    echo "▶ installing always-tauri into Always Dev.app..."
    cp "$TAURI_BIN" "$BUNDLED_TAURI_BIN"
fi
if [ ! -x "$BUNDLED_TAURI_BIN" ]; then
    echo "✗ always-tauri is missing from $BUNDLED_TAURI_BIN"
    echo "  The menu bar's Settings action will log 'always-tauri binary not"
    echo "  found' and open nothing. Rebuild without --no-tauri."
    exit 1
fi

# build.sh already codesigned the bundle, so swapping in the new
# always-tauri binary invalidated its seal. Re-sign in place — macOS 15+
# refuses to launch an app whose seal no longer matches, and TCC keys mic
# and accessibility grants to the signature, which must stay stable. This
# mirrors build.sh's identity resolution: same cert survives grants across
# rebuilds; ad-hoc is the accepted last resort.
resign_identity="${ALWAYS_CODESIGN_IDENTITY:-}"
if [ -z "$resign_identity" ]; then
    resign_identity="$(security find-identity -v -p codesigning 2>/dev/null | sed -n 's/.*"\(Always Local Signing\)".*/\1/p' | head -1)"
fi
if [ -z "$resign_identity" ]; then
    resign_identity="$(security find-identity -v -p codesigning 2>/dev/null | sed -n 's/.*"\(Developer ID Application: [^"]*\)".*/\1/p' | head -1)"
fi
if [ -z "$resign_identity" ]; then
    resign_identity="$(security find-identity -v -p codesigning 2>/dev/null | sed -n 's/.*"\(Apple Development: [^"]*\)".*/\1/p' | head -1)"
fi
[ -z "$resign_identity" ] && resign_identity="-"
if codesign --force --sign "$resign_identity" \
        --identifier "com.always.v3.dev" \
        --entitlements "$REPO_ROOT/Always/Always.entitlements" \
        "$DEV_APP" >/dev/null 2>&1; then
    echo "  ✓ re-signed $DEV_APP (bundle seal restored after always-tauri swap)"
else
    echo "⚠️  could not re-sign $DEV_APP after adding always-tauri — the launch"
    echo "    below may be rejected by Gatekeeper. Fix with:"
    echo "    codesign --force --deep --sign '$resign_identity' '$DEV_APP'"
fi


# First-run seed: copy production state into the dev instance so the dev
# app boots with the user's real prefs, Groq key, voiceprint, and
# vocabulary instead of a cold onboarding flow. Runs once — afterwards the
# dev dir is independent and prod edits never propagate.
if [ ! -d "$DEV_SUPPORT_DIR" ]; then
    echo "▶ seeding dev instance from production state..."
    mkdir -p "$DEV_SUPPORT_DIR"
    for f in always.db always.db-wal always.db-shm voiceprint.json vocabulary.json; do
        [ -f "$PROD_SUPPORT_DIR/$f" ] && cp "$PROD_SUPPORT_DIR/$f" "$DEV_SUPPORT_DIR/$f" || true
    done
fi

echo "▶ launching Always Dev..."
open -g "$DEV_APP"

# LaunchServices starts the GUI before the app has finished spawning the
# bundled daemon. A fixed three-second pause can race that asynchronous
# startup; wait for the expected process instead, while retaining a hard
# timeout so a broken launch still fails visibly.
wait_for_process() {
    local pattern="$1"
    local attempt
    for attempt in {1..15}; do
        if pgrep -f "$pattern" >/dev/null; then
            return 0
        fi
        sleep 1
    done
    return 1
}

wait_for_process "Always Dev.app/Contents/MacOS/Always$" || true
if [ "$SKIP_DAEMON" = false ]; then
    wait_for_process "Always Dev.app/Contents/MacOS/always-daemon run" || true
fi

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
verify_running "dev GUI" "Always Dev.app/Contents/MacOS/Always$" \
    "$DEV_APP/Contents/MacOS/Always" || VERIFY_OK=false
if [ "$SKIP_DAEMON" = false ]; then
    verify_running "dev daemon" "Always Dev.app/Contents/MacOS/always-daemon run" \
        "$DEV_APP/Contents/MacOS/always-daemon" || VERIFY_OK=false
fi

# The settings app is launched on demand by the menu bar, so this can only
# verify the BINARY the menu action will find — not a running process.
# Deployed and runnable are different claims, and a binary that is missing
# or older than the one just built means the menu bar would launch a stale
# settings UI. (Dev-only paths: the bundle path is "$DEV_APP".)
if [ "$SKIP_TAURI" = false ] || [ -x "$BUNDLED_TAURI_BIN" ]; then
    echo "  ▶ verifying the bundled always-tauri binary..."
    bundled_mtime=$(stat -f %m "$BUNDLED_TAURI_BIN")
    if [ -f "$TAURI_BIN" ] && [ "$bundled_mtime" -lt "$(stat -f %m "$TAURI_BIN")" ]; then
        if [ "$SKIP_TAURI" = true ]; then
            # Explicit opt-out: the cargo target is newer, but the user chose
            # not to rebuild it this run. Say so — never silently ship stale.
            echo "  ⚠️  bundled always-tauri is older than $TAURI_BIN (--no-tauri"
            echo "     skipped the rebuild; run without the flag to refresh it)"
        else
            echo "✗ bundled always-tauri ($BUNDLED_TAURI_BIN) is older than the"
            echo "  build at $TAURI_BIN — the menu bar Settings action would launch"
            echo "  a stale settings app; the copy above did not take effect."
            VERIFY_OK=false
        fi
    else
        echo "  ✓ always-tauri installed in Always Dev.app (menu bar Settings will use it)"
    fi
fi

if [ "$VERIFY_OK" != true ]; then
    play "$SOUND_FAIL"
    echo "✗ REBUILD NOT LIVE — do not test, and do not report this as shipped."
    exit 1
fi

# Belt-and-suspenders audit: prove production is still alive. Match its
# absolute executable path case-insensitively because the signed bundle's
# executable is `always` on disk, and may be invoked with arguments.
if ! pgrep -if '^/Applications/Always\.app/Contents/MacOS/always( |$)' >/dev/null; then
    echo "⚠️  production Always GUI is not running — nothing was killed by"
    echo "    this script, but prod won't come back on its own."
fi

play "$SOUND_UP"
echo "✓ done — Always Dev verified running (production untouched)"
