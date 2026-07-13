# Always Voice Detection - Build & Run Guide

## 🚀 One-Command Build & Run

You now have multiple ways to build and run the complete Always system:

### Option 1: Unified Script (Recommended)
```bash
./build-and-run.sh
```

### Option 2: Simple Wrapper
```bash
./run.sh
```

### Option 3: Makefile (Most Convenient)
```bash
make          # Build and run (default)
make run      # Build and run  
make start    # Build and run
make stop     # Stop system
make clean    # Clean builds
make help     # Show help
```

## ⚙️ What the Build Script Does

1. **Stops** any existing Always processes
2. **Builds** Rust CLI binary (`cargo build --release`)
3. **Builds** macOS app (Swift/SwiftUI) 
4. **Installs** Always to `/Applications/`
5. **Launches** Always which auto-starts the voice daemon
6. **Verifies** both processes are running
7. **Shows** status and usage instructions

## 🎯 Auto-Start Integration

The Always is configured to:
- ✅ **Auto-start daemon** when the app launches
- ✅ **Auto-stop daemon** when the app quits  
- ✅ **Manage lifecycle** of the voice detection system
- ✅ **Show live status** in menu bar and overlay

## 📱 What You'll See

After running the build script:

1. **Menu Bar Icon**: 🎤 microphone icon (top-right)
2. **Visual Overlay**: Blue pill-shaped indicator that changes colors
3. **Voice Detection**: Automatic speech-to-text transcription
4. **Real-time Feedback**: Overlay shows listening/transcribing states

## 🎙️ Usage

- **Speak normally** → automatic transcription
- **Menu bar controls** → click 🎤 icon for settings
- **Keyboard shortcuts**:
  - `Ctrl+Shift+P` → pause/resume
  - `Ctrl+Shift+A` → toggle auto-enter
  - `Ctrl+C` → stop daemon

## 🛑 Stopping

```bash
make stop
# or
pkill -f Always
# or 
Click menu bar icon → Quit
```

The unified system ensures perfect coordination between the macOS app and voice detection daemon!