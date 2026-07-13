#!/bin/bash
# Integration test script for Linux overlay

set -e

echo "=== Linux Overlay Integration Test ==="
echo ""

# Check if overlay binary exists
if [ ! -f "./target/debug/always-overlay" ]; then
    echo "Building overlay binary..."
    cargo build --no-default-features --features overlay --bin always-overlay
fi

echo "✓ Overlay binary built"
echo ""

# Test 1: Check overlay binary exists and is executable
echo "Test 1: Binary exists"
if [ -x "./target/debug/always-overlay" ]; then
    echo "✓ Overlay binary is executable"
else
    echo "✗ Overlay binary is not executable"
    exit 1
fi
echo ""

# Test 2: Run unit tests
echo "Test 2: Unit tests"
cargo test --no-default-features --features overlay --bin always-overlay
echo "✓ All unit tests passed"
echo ""

# Test 3: Check overlay startup. Requires a usable X11 display.
echo "Test 3: Overlay startup without daemon"
startup_output="$(timeout 5 ./target/debug/always-overlay 2>&1 || true)"
printf '%s\n' "$startup_output"
if printf '%s\n' "$startup_output" | grep -q "Failed to open X11 display"; then
    echo "⚠ Skipped reconnect smoke test: no usable X11 display in this shell"
elif printf '%s\n' "$startup_output" | grep -q "Retrying in 5 seconds"; then
    echo "✓ Overlay opens X11 display and retries while daemon is unavailable"
else
    echo "✗ Overlay did not show expected startup/retry behavior"
    exit 1
fi
echo ""

# Test 4: Verify systemd units are present
echo "Test 4: Systemd units"
if [ -f "./systemd/always-daemon.service" ] && [ -f "./systemd/always-overlay.service" ]; then
    echo "✓ Systemd unit files present"
    echo "  - always-daemon.service"
    echo "  - always-overlay.service"
else
    echo "✗ Systemd unit files missing"
    exit 1
fi
echo ""

# Test 5: Verify documentation
echo "Test 5: Documentation"
if [ -f "./docs/linux-overlay.md" ]; then
    echo "✓ Documentation present"
else
    echo "✗ Documentation missing"
    exit 1
fi
echo ""

echo "=== All Integration Tests Passed ==="
echo ""
echo "Manual Testing Instructions:"
echo "1. Start daemon: always run"
echo "2. Start overlay: ./target/debug/always-overlay"
echo "3. Trigger voice activity and verify the X11 HUD appears near the bottom center"
echo "4. Test pause/resume: always toggle-pause"
echo "5. Verify overlay updates and hides when state clears"
echo ""
echo "Note: Current implementation uses an X11 renderer. Wayland layer-shell is future work."
