#!/usr/bin/env bash
# Build and launch the Linux daemon and Tauri settings app from this checkout.
# This script only manages a GUI process recorded in its own PID file.

set -euo pipefail

if [[ "$(uname -s)" != Linux ]]; then
    echo "This script builds the Linux app and must run on Linux." >&2
    exit 2
fi

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

GUI_DIR="$REPO_ROOT/always-gui"
GUI_BIN="$REPO_ROOT/crates/always_tauri/target/debug/always-tauri"
DAEMON_BIN="$REPO_ROOT/target/debug/always"
STATE_DIR="${XDG_STATE_HOME:-$HOME/.local/state}/always-dev"
PID_FILE="$STATE_DIR/gui.pid"
LOG_FILE="$STATE_DIR/gui.log"

mkdir -p "$STATE_DIR"

# Stop only processes whose executable is this checkout's Linux GUI binary.
for proc_exe in /proc/[0-9]*/exe; do
    [[ -e "$proc_exe" ]] || continue
    old_exe="$(readlink -f "$proc_exe" 2>/dev/null || true)"
    [[ "$old_exe" == "$GUI_BIN" ]] || continue
    old_pid="${proc_exe#/proc/}"
    old_pid="${old_pid%/exe}"
    [[ "$old_pid" =~ ^[0-9]+$ ]] || continue
    kill -TERM "$old_pid" 2>/dev/null || true
    for _ in {1..30}; do
        kill -0 "$old_pid" 2>/dev/null || break
        sleep 0.1
    done
done
rm -f "$PID_FILE"

echo "▶ building Linux daemon..."
cargo build --locked --no-default-features --features linux --bin always

echo "▶ building settings frontend..."
(
    cd "$GUI_DIR"
    npm run build
)

echo "▶ building Linux settings app..."
cargo build --locked --manifest-path crates/always_tauri/Cargo.toml --bin always-tauri

if [[ ! -x "$DAEMON_BIN" || ! -x "$GUI_BIN" ]]; then
    echo "✗ expected Linux binaries were not produced" >&2
    exit 1
fi

echo "▶ launching settings app..."
ALWAYS_CLI="$DAEMON_BIN" "$GUI_BIN" >"$LOG_FILE" 2>&1 &
gui_pid=$!
printf '%s\n' "$gui_pid" > "$PID_FILE"
trap 'rm -f "$PID_FILE"' EXIT

for _ in {1..30}; do
    if ! kill -0 "$gui_pid" 2>/dev/null; then
        echo "✗ settings app exited during startup; log: $LOG_FILE" >&2
        cat "$LOG_FILE" >&2
        rm -f "$PID_FILE"
        exit 1
    fi
    sleep 0.1
done

if [[ "$(readlink -f "/proc/$gui_pid/exe" 2>/dev/null || true)" != "$GUI_BIN" ]]; then
    echo "✗ could not verify the settings app process; log: $LOG_FILE" >&2
    exit 1
fi

echo "✓ Linux settings app running (pid $gui_pid)"
echo "  Models panel: open Settings → Models"
echo "  daemon: $DAEMON_BIN"
echo "  log: $LOG_FILE"
echo "  close the settings window to finish this command"
if wait "$gui_pid"; then
    exit 0
else
    status=$?
    echo "✗ settings app exited with status $status; log: $LOG_FILE" >&2
    cat "$LOG_FILE" >&2
    exit "$status"
fi
