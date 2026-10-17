#!/usr/bin/env bash
# Build and launch the Linux daemon and Tauri settings app from this checkout.
# This script manages only the dev GUI and daemon from this checkout.

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
DAEMON_PID_FILE="$STATE_DIR/daemon.pid"
DAEMON_LOG_FILE="$STATE_DIR/daemon.log"
DEV_GROQ_API_KEY="${GROQ_API_KEY:-}"
unset GROQ_API_KEY
if [[ -n "${XDG_RUNTIME_DIR:-}" ]]; then
    SOCKET_FILE="$XDG_RUNTIME_DIR/always-dev.sock"
else
    SOCKET_FILE="/tmp/always-dev.sock"
fi

mkdir -p "$STATE_DIR"

stop_recorded_dev_daemon() {
    [[ -f "$DAEMON_PID_FILE" ]] || return 0
    old_pid="$(cat "$DAEMON_PID_FILE" 2>/dev/null || true)"
    if [[ "$old_pid" =~ ^[0-9]+$ && -e "/proc/$old_pid/exe" ]]; then
        old_exe="$(readlink -f "/proc/$old_pid/exe" 2>/dev/null || true)"
        old_instance="$(tr '\0' '\n' <"/proc/$old_pid/environ" 2>/dev/null | sed -n 's/^ALWAYS_INSTANCE=//p' | head -1)"
        if [[ "$old_exe" == "$DAEMON_BIN" && "$old_instance" == "dev" ]]; then
            kill -TERM "$old_pid" 2>/dev/null || true
            for _ in {1..50}; do
                kill -0 "$old_pid" 2>/dev/null || break
                sleep 0.1
            done
        fi
    fi
    rm -f "$DAEMON_PID_FILE"
}

cleanup() {
    if [[ -n "${daemon_pid:-}" ]] && kill -0 "$daemon_pid" 2>/dev/null; then
        daemon_exe="$(readlink -f "/proc/$daemon_pid/exe" 2>/dev/null || true)"
        daemon_instance="$(tr '\0' '\n' <"/proc/$daemon_pid/environ" 2>/dev/null | sed -n 's/^ALWAYS_INSTANCE=//p' | head -1)"
        if [[ "$daemon_exe" == "$DAEMON_BIN" && "$daemon_instance" == "dev" ]]; then
            kill -TERM "$daemon_pid" 2>/dev/null || true
        fi
    fi
    rm -f "$PID_FILE" "$DAEMON_PID_FILE"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

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
stop_recorded_dev_daemon

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

echo "▶ launching dev daemon..."
if [[ -n "$DEV_GROQ_API_KEY" ]]; then
    ALWAYS_INSTANCE=dev GROQ_API_KEY="$DEV_GROQ_API_KEY" "$DAEMON_BIN" run >"$DAEMON_LOG_FILE" 2>&1 &
else
    ALWAYS_INSTANCE=dev "$DAEMON_BIN" run >"$DAEMON_LOG_FILE" 2>&1 &
fi
daemon_pid=$!
unset DEV_GROQ_API_KEY
printf '%s\n' "$daemon_pid" > "$DAEMON_PID_FILE"

daemon_ready=false
for _ in {1..150}; do
    if ! kill -0 "$daemon_pid" 2>/dev/null; then
        echo "✗ dev daemon exited during startup; log: $DAEMON_LOG_FILE" >&2
        cat "$DAEMON_LOG_FILE" >&2
        exit 1
    fi
    if rg -q "uds_server_listening, socket_path: $SOCKET_FILE" "$DAEMON_LOG_FILE"; then
        daemon_ready=true
        break
    fi
    sleep 0.1
done
if [[ "$daemon_ready" != true ]]; then
    echo "✗ dev daemon did not create its socket; log: $DAEMON_LOG_FILE" >&2
    cat "$DAEMON_LOG_FILE" >&2
    exit 1
fi

echo "▶ launching settings app..."
ALWAYS_INSTANCE=dev ALWAYS_CLI="$DAEMON_BIN" "$GUI_BIN" >"$LOG_FILE" 2>&1 &
gui_pid=$!
printf '%s\n' "$gui_pid" > "$PID_FILE"

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

gui_connected=false
for _ in {1..150}; do
    if ! kill -0 "$gui_pid" 2>/dev/null; then
        echo "✗ settings app exited before connecting to the daemon; log: $LOG_FILE" >&2
        cat "$LOG_FILE" >&2
        exit 1
    fi
    if rg -q "uds_connected path=$SOCKET_FILE" "$LOG_FILE"; then
        gui_connected=true
        break
    fi
    sleep 0.1
done
if [[ "$gui_connected" != true ]]; then
    echo "✗ settings app did not connect to the dev daemon; log: $LOG_FILE" >&2
    cat "$LOG_FILE" >&2
    exit 1
fi

if ! tr '\0' '\n' <"/proc/$gui_pid/environ" | rg -qx 'ALWAYS_INSTANCE=dev'; then
    echo "✗ settings app is not isolated as the dev instance; log: $LOG_FILE" >&2
    exit 1
fi

if ! kill -0 "$daemon_pid" 2>/dev/null || [[ "$(readlink -f "/proc/$daemon_pid/exe" 2>/dev/null || true)" != "$DAEMON_BIN" ]]; then
    echo "✗ dev daemon process verification failed; log: $DAEMON_LOG_FILE" >&2
    exit 1
fi


echo "✓ Linux settings app running (pid $gui_pid)"
echo "  Models panel: open Settings → Models"
echo "  daemon: $DAEMON_BIN"
echo "  daemon log: $DAEMON_LOG_FILE"
echo "  GUI log: $LOG_FILE"
echo "  close the settings window to finish this command"
if wait "$gui_pid"; then
    exit 0
else
    status=$?
    echo "✗ settings app exited with status $status; log: $LOG_FILE" >&2
    cat "$LOG_FILE" >&2
    exit "$status"
fi
