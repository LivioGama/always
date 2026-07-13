#!/bin/bash

set -e  # Exit on any error

echo "🔧 Building Always - Unified Build & Run Script"
echo "================================================"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Function to print colored output
print_step() {
    echo -e "${BLUE}▶${NC} $1"
}

print_success() {
    echo -e "${GREEN}✓${NC} $1"
}

print_warning() {
    echo -e "${YELLOW}⚠${NC} $1"
}

print_error() {
    echo -e "${RED}✗${NC} $1"
}

daemon_pids() {
    ps -eo pid=,args= | awk '{
        pid=$1
        exe=$2
        sub(/^.*\//, "", exe)
        if ((exe == "always" || exe == "always-daemon") && $3 == "run") {
            print pid
        }
    }'
}

kill_daemons() {
    local pids
    pids="$(daemon_pids)"
    if [[ -z "$pids" ]]; then
        echo "  No always daemon running"
        return 1
    fi
    while read -r pid; do
        [[ -n "$pid" ]] && kill -TERM "$pid" 2>/dev/null || true
    done <<< "$pids"
    sleep 1
    while read -r pid; do
        if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
            kill -KILL "$pid" 2>/dev/null || true
        fi
    done <<< "$pids"
    print_success "Killed always daemon"
}

# Step 1: Kill existing processes
print_step "Stopping existing Always processes..."
pkill -f "Always.app" 2>/dev/null && print_success "Killed Always" || echo "  No Always running"
kill_daemons || true
sleep 1

# Step 2: Build Rust CLI binary
print_step "Building Always CLI binary (Rust)..."
if cargo build --release; then
    print_success "CLI binary built successfully"
else
    print_error "Failed to build CLI binary"
    exit 1
fi

# Step 3: Verify binary exists and is executable
BINARY_PATH="target/release/always"
if [[ -f "$BINARY_PATH" && -x "$BINARY_PATH" ]]; then
    print_success "CLI binary verified at $BINARY_PATH"
else
    print_error "CLI binary not found or not executable at $BINARY_PATH"
    exit 1
fi

# Step 4: Build macOS App
print_step "Building Always (Swift/SwiftUI)..."
cd Always
if ./build.sh; then
    print_success "Always built and installed successfully"
else
    print_error "Failed to build Always"
    exit 1
fi
cd ..

# Step 5: Verify app installation
if [[ -d "/Applications/Always.app" ]]; then
    print_success "Always installed at /Applications/Always.app"
else
    print_error "Always not found in Applications folder"
    exit 1
fi

# Step 6: Launch the integrated system
print_step "Launching integrated Always system..."
open -a Always

# Wait for app to start
sleep 3

# Step 7: Verify everything is running
print_step "Verifying system status..."

# Check if Always is running
if pgrep -f "Always.app" > /dev/null; then
    ALWAYS_PID=$(pgrep -f "Always.app")
    print_success "Always running (PID: $ALWAYS_PID)"
else
    print_error "Always failed to start"
    exit 1
fi

# Check if daemon is running (should be auto-started by Always)
DAEMON_PID=""
for _ in {1..60}; do
    DAEMON_PID=$(daemon_pids | paste -sd "," -)
    if [[ -n "$DAEMON_PID" ]]; then
        break
    fi
    sleep 1
done
if [[ -n "$DAEMON_PID" ]]; then
    print_success "Voice daemon running (PID: $DAEMON_PID)"
else
    print_warning "Voice daemon not detected after 60 seconds"
fi

# Step 8: Display system status
echo ""
echo -e "${GREEN}🎉 Always System Successfully Built & Launched!${NC}"
echo "================================================"
echo ""
echo -e "${BLUE}📱 What you should see:${NC}"
echo "  • Menu bar microphone icon (🎤) in top-right corner"
echo "  • Colored overlay indicator (blue pill shape)"
echo "  • Voice detection active and ready"
echo ""
echo -e "${BLUE}🎙️  How to use:${NC}"
echo "  • Speak normally - voice will be auto-transcribed"
echo "  • Click menu bar icon for controls and settings"
echo "  • Ctrl+Alt+P to pause/resume"
echo "  • Ctrl+Alt+A to toggle auto-enter"
echo ""
echo -e "${BLUE}📊 Current processes:${NC}"
ps -eo pid=,args= | awk '{
    exe=$2
    sub(/^.*\//, "", exe)
    if ($0 ~ /Always.app\/Contents\/MacOS\/Always/ || ((exe == "always" || exe == "always-daemon") && $3 == "run")) {
        print
    }
}' | while read line; do
    echo "  $line"
done
echo ""

# Step 9: Show recent logs
if [[ -f "$HOME/Library/Application Support/always/always.log" ]]; then
    echo -e "${BLUE}📋 Recent activity:${NC}"
    tail -3 "$HOME/Library/Application Support/always/always.log" | sed 's/^/  /'
    echo ""
fi

echo -e "${GREEN}✓ Always is ready for voice detection!${NC}"
echo ""
echo "To stop: Click menu bar icon → Quit, or run 'always stop && pkill -f Always.app'"
