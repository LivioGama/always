# Always - Windows Build & Run Guide

## Overview

Always is a voice-to-text daemon that runs on Windows. The daemon CLI builds on Windows with the Rust MSVC toolchain.

**Note**: On Windows, keyboard listener and clipboard paste are stubs (not yet implemented). Voice transcription via Groq API works fully. PRs welcome for implementing Windows-specific keyboard/clipboard features.

## Prerequisites

### Required
1. **Rust** with MSVC toolchain
   - Download from https://rustup.rs/
   - During install, choose "Install" for the full toolchain including MSVC
   - Or add MSVC target after install: `rustup target add x86_64-pc-windows-msvc`

2. **Git** (optional but recommended)
   - Download from https://git-scm.com/
   - Or use Windows Package Manager: `winget install Git.Git`

3. **A free Groq API key**
   - Sign up at https://console.groq.com/keys

### Recommended
- **VS Code** or similar editor for configuration
- **Windows Terminal** for better command-line experience

## Quick Build (Windows PowerShell)

### 1. Clone the Repository

```powershell
git clone https://github.com/rtk-ai/always.git
cd always
```

### 2. Run the Build Script

```powershell
.\build-windows.ps1 -Profile release
```

Or for debug builds (larger but better for development):

```powershell
.\build-windows.ps1 -Profile debug
```

The script will:
- ✅ Check for Rust and MSVC toolchain
- ✅ Build the Windows daemon
- ✅ Verify the binary works
- ✅ Output the binary location

## Manual Build Steps

If the build script doesn't work, follow these manual steps:

### 1. Ensure Rust MSVC Target

```powershell
rustup target add x86_64-pc-windows-msvc
```

### 2. Build the Release Binary

```powershell
cargo build --no-default-features --features windows --target x86_64-pc-windows-msvc --release
```

The binary will be at:
```
target/x86_64-pc-windows-msvc/release/always.exe
```

### 3. Verify the Build

```powershell
.\target\x86_64-pc-windows-msvc\release\always.exe --version
```

## First-Run Setup

### 1. Configure Your Groq API Key

```powershell
.\target\x86_64-pc-windows-msvc\release\always.exe config set groq_api_key sk_your_key_here
```

Get a free key at https://console.groq.com/keys

### 2. (Optional) Tune Sensitivity

```powershell
# For normal office environments
.\always.exe config preset normal

# For quieter rooms
.\always.exe config preset high

# For noisy environments
.\always.exe config preset low
```

### 3. Start the Daemon

```powershell
.\always.exe start
```

### 4. Verify It's Running

```powershell
.\always.exe status
```

You should see the PID (process ID) and log file path.

## Using the Daemon

### Start the Daemon (Background)
```powershell
always.exe start
```

### Run in Foreground (for debugging)
```powershell
always.exe run --lang en --silence 2.0
```

### View Live Logs
```powershell
always.exe logs --pretty
```

### Stop the Daemon
```powershell
always.exe stop
```

### Check Configuration
```powershell
always.exe config show
```

## Daemon Arguments

```
FLAGS:
  -l, --lang <CODE>        Language code (default: en)
  -t, --timeout <SECS>     Max utterance duration (default: 30)
  -s, --silence <SECS>     Silence to end utterance (default: 2.0)
  --auto-enter             Press Return after every paste
```

Example with custom settings:
```powershell
always.exe start --silence 1.5 --auto-enter
```

## Troubleshooting

### Build Errors

#### Error: "could not find native static library"
- Ensure you have Rust with MSVC toolchain: `rustup toolchain list`
- Reinstall MSVC target: `rustup target add x86_64-pc-windows-msvc --force`

#### Error: "clang-cl not found" or "dlltool not found"
- This means the C/C++ build tools aren't properly set up
- Reinstall from https://rustup.rs/ and choose the full MSVC toolchain
- Or install Visual Studio Build Tools: https://visualstudio.microsoft.com/downloads/

#### Firewall/Network Errors
- Allow `always.exe` through Windows Defender Firewall
- The daemon makes HTTPS requests to Groq API (api.groq.com)

### Runtime Issues

#### Daemon won't start
- Check status: `always.exe status`
- Look at logs: `always.exe logs --pretty`
- Ensure Groq API key is set: `always.exe config show | findstr groq`

#### No transcription happening
- Check microphone permissions in Windows Settings
- Verify microphone works (test in Voice Recorder app)
- Check logs for VAD/audio errors: `always.exe logs --level debug`

#### Transcriptions very slow
- Check network latency to Groq API
- Try offline mode (when available): `cargo build --features local-stt`

## Production Deployment

### Install to System PATH

Copy the binary to a directory in your PATH:

```powershell
# Create a bin directory if needed
mkdir $env:USERPROFILE\.local\bin -ErrorAction SilentlyContinue

# Copy the binary
Copy-Item target\x86_64-pc-windows-msvc\release\always.exe $env:USERPROFILE\.local\bin\

# Add to PATH (one time)
$env:Path += ";$env:USERPROFILE\.local\bin"
[Environment]::SetEnvironmentVariable("Path", $env:Path, [EnvironmentVariableTarget]::User)
```

Then use from anywhere:
```powershell
always.exe start
```

### Auto-Start on Login

Create a startup shortcut:

1. Press `Win+R`, type `shell:startup`, press Enter
2. Create a new shortcut pointing to: 
   ```
   C:\Users\YourUsername\.local\bin\always.exe start
   ```

Or use Task Scheduler for more control:

```powershell
# Create a scheduled task that runs at login
$trigger = New-ScheduledTaskTrigger -AtLogon
$action = New-ScheduledTaskAction -Execute "C:\path\to\always.exe" -Argument "start"
Register-ScheduledTask -TaskName "Always Daemon" -Trigger $trigger -Action $action -RunLevel Highest
```

## Configuration Files

Settings are stored in Windows Registry (via `keyring` crate):
```
HKEY_CURRENT_USER\Software\rtk-ai\always\
```

Or view via CLI:
```powershell
always.exe config show
```

## Development Build vs Release Build

**Release Build** (Production):
```powershell
cargo build --no-default-features --features windows --target x86_64-pc-windows-msvc --release
```
- Optimized for speed (~5-10% faster)
- Smaller binary (~23 MB vs ~80 MB)
- Transcripts hidden in logs by default (privacy)
- Use for daily use

**Debug Build** (Development):
```powershell
cargo build --no-default-features --features windows --target x86_64-pc-windows-msvc
```
- Unoptimized, faster compilation
- Larger binary with symbols
- Transcripts visible in logs (helpful for debugging)
- Use for development

To see transcripts in release builds:
```powershell
$env:ALWAYS_LOG_TRANSCRIPTS = 1
always.exe start
```

## Feature Flags

The Windows build uses `--features windows` which:
- ✅ Enables Groq Whisper transcription
- ✅ Enables VAD (voice activity detection)
- ✅ Enables glossary biasing
- ❌ Disables GUI overlay (macOS only)
- ❌ Disables keyboard listener (keyboard paste) — stub only
- ❌ Disables clipboard paste — stub only

These keyboard/clipboard stubs exist but return `NotImplemented`. Implementing them requires Windows-specific code:
- Keyboard listener: `SetWindowsHookEx` or keyboard interception library
- Clipboard paste: Windows clipboard API + keyboard events

## Advanced: Building from Source with Local STT

To include offline Whisper (requires ~2 GB ONNX runtime):

```powershell
cargo build --no-default-features --features windows,local-stt --target x86_64-pc-windows-msvc --release
```

This lets you transcribe without internet (all-local processing).

## Contributing

PRs for Windows-specific features (keyboard, clipboard, GUI) are very welcome!

See [`CONTRIBUTING.md`](CONTRIBUTING.md) for details.

## Support

- **Issues**: https://github.com/rtk-ai/always/issues
- **Discussions**: https://github.com/rtk-ai/always/discussions
- **Documentation**: https://github.com/rtk-ai/always/tree/main/docs

---

**Built for Windows users who got tired of typing.**
