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

# Step 1: Kill existing processes
print_step "Stopping existing Always processes..."
pkill -f "AlwaysApp" 2>/dev/null && print_success "Killed AlwaysApp" || echo "  No AlwaysApp running"
pkill -f "always run" 2>/dev/null && print_success "Killed always daemon" || echo "  No always daemon running"
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
print_step "Building AlwaysApp (Swift/SwiftUI)..."
cd AlwaysApp
if ./build.sh; then
    print_success "AlwaysApp built and installed successfully"
else
    print_error "Failed to build AlwaysApp"
    exit 1
fi
cd ..

# Step 5: Verify app installation
if [[ -d "/Applications/AlwaysApp.app" ]]; then
    print_success "AlwaysApp installed at /Applications/AlwaysApp.app"
else
    print_error "AlwaysApp not found in Applications folder"
    exit 1
fi

# Step 6: Launch the integrated system
print_step "Launching integrated Always system..."
open -a AlwaysApp

# Wait for app to start
sleep 3

# Step 7: Verify everything is running
print_step "Verifying system status..."

# Check if AlwaysApp is running
if pgrep -f "AlwaysApp" > /dev/null; then
    ALWAYSAPP_PID=$(pgrep -f "AlwaysApp")
    print_success "AlwaysApp running (PID: $ALWAYSAPP_PID)"
else
    print_error "AlwaysApp failed to start"
    exit 1
fi

# Check if daemon is running (should be auto-started by AlwaysApp)
sleep 2  # Give daemon time to start
if pgrep -f "always run" > /dev/null; then
    DAEMON_PID=$(pgrep -f "always run")
    print_success "Voice daemon running (PID: $DAEMON_PID)"
else
    print_warning "Voice daemon not detected yet (may still be starting...)"
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
echo "  • Ctrl+Shift+P to pause/resume"
echo "  • Ctrl+Shift+A to toggle auto-enter"
echo ""
echo -e "${BLUE}📊 Current processes:${NC}"
ps aux | grep -E "(AlwaysApp|always run)" | grep -v grep | while read line; do
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
echo "To stop: Click menu bar icon → Quit, or run 'pkill -f AlwaysApp'"