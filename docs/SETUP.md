# 📋 Platform-Specific Setup Guide

This guide covers platform-specific installation, permissions, and troubleshooting for Always on macOS, Linux, and Windows.

---

## Table of Contents

- [macOS](#macos)
- [Linux](#linux)
- [Windows](#windows)
- [Common Issues](#common-issues)

---

## macOS

### Installation

Install with one command (Apple Silicon):

```bash
curl -fsSL https://raw.githubusercontent.com/LivioGama/always/main/scripts/install-always.sh | bash
```

The script downloads the latest release from [GitHub Releases](https://github.com/LivioGama/always/releases/latest) and installs `Always.app`. To install by hand, download `always-vX.Y.Z-aarch64-apple-darwin.tar.gz` and its `.sha256` from the same page, check it with `shasum -a 256 -c`, and unpack it.

Launch from `Applications` or Spotlight (`⌘ + Space`, type "Always").

### Permissions

macOS requires explicit permission for microphone and automation access. After first launch:

#### 1. Microphone Access

```
System Settings → Privacy & Security → Microphone → Enable Always
```

#### 2. Input Monitoring

```
System Settings → Privacy & Security → Input Monitoring → Enable Always
```

#### 3. Accessibility (for keyboard events)

```
System Settings → Privacy & Security → Accessibility → Enable Always
```

#### If permissions are denied

macOS may show: *"Always.app" needs to be opened to continue.*

```bash
# Gatekeeper override — only if you downloaded from the web
xattr -d com.apple.quarantine /Applications/Always.app

# Signature check (the app is ad-hoc signed, not notarized)
codesign --verify --deep --strict /Applications/Always.app
# Prints nothing on success; a failure names the broken file
```

### First Launch Workflow

1. **Launch Always** from Applications.
2. **Grant permissions** when prompted by macOS.
3. **Enroll your voice** — record three short samples in **Settings → My Voice**.
4. **Configure STT backend** — set a Groq API key in **Settings → Models**, or download a local model.
5. **Start dictating** — just talk.

### Troubleshooting

| Issue | Fix |
|---|---|
| "Always" is damaged and can't be opened | Run `xattr -d com.apple.quarantine /Applications/Always.app` in Terminal |
| Microphone access denied | Re-check permissions in System Settings → Privacy & Security |
| Overlay doesn't appear | Grant Accessibility permission; check System Settings → Privacy & Security → Screen Recording |
| App quits immediately | Check `~/Library/Logs/Always/` for crash logs |

---

## Linux

### Installation

Install with one command (x86_64 and aarch64):

```bash
curl -fsSL https://raw.githubusercontent.com/LivioGama/always/main/scripts/install-always.sh | bash
```

Or install a package from [GitHub Releases](https://github.com/LivioGama/always/releases/latest). Packages are published as `.tar.gz`, `.deb` and `.rpm` for `x86_64` and `aarch64`.

#### Debian / Ubuntu

```bash
# Download a release (replace vX.Y.Z with the release tag)
wget https://github.com/LivioGama/always/releases/latest/download/always-vX.Y.Z-x86_64-unknown-linux-gnu.deb

# Install (this places the daemon binary and the systemd user service)
sudo apt install ./always-vX.Y.Z-x86_64-unknown-linux-gnu.deb
```

#### Fedora / RHEL

```bash
# Download a release (replace vX.Y.Z with the release tag)
wget https://github.com/LivioGama/always/releases/latest/download/always-vX.Y.Z-x86_64-unknown-linux-gnu.rpm

# Install
sudo dnf install ./always-vX.Y.Z-x86_64-unknown-linux-gnu.rpm
```

### systemd Service

The DEB/RPM packages install a systemd **user** service for the Always daemon
(`/usr/lib/systemd/user/always-daemon.service`, source
`scripts/release/always-daemon.service`, runs `always run`). The one-line
installer does the same for a tarball install under `~/.config/systemd/user/`.

```bash
# Start the daemon now and enable auto-start on login
systemctl --user enable --now always-daemon.service

# Check status
systemctl --user status always-daemon

# View logs
journalctl --user -u always-daemon -f
```

The daemon runs as the **current user** (via `--user` scope), so no root privileges are needed at runtime. Installing the package needs `sudo`; enabling the service does not.

### Audio Configuration

Always uses **ALSA** via the `cpal` crate for audio capture. On most modern Linux systems, PipeWire or PulseAudio provides an ALSA compatibility layer automatically.

```bash
# Check available audio devices
arecord -l

# Test microphone recording
arecord -D plughw:1,0 -f S16_LE -r 16000 -c 1 /tmp/test.wav
aplay /tmp/test.wav

# If no devices show, ensure your audio server is running
# For PipeWire:
systemctl --user status pipewire pipewire-pulse
# For PulseAudio:
systemctl --user status pulseaudio
```

### udev Rules

For direct microphone access (required on some minimal setups without PulseAudio/PipeWire):

```bash
# The package installs /etc/udev/rules.d/99-always-microphone.rules automatically.
# If missing, create it manually:

sudo tee /etc/udev/rules.d/99-always-microphone.rules > /dev/null << 'EOF'
# Always daemon — allow the current user to access USB/midi audio interfaces
SUBSYSTEM=="snd", GROUP="audio", MODE="0660"
SUBSYSTEM=="sound", GROUP="audio", MODE="0660"
EOF

# Reload rules
sudo udevadm control --reload-rules
sudo udevadm trigger
```

**Note:** The `always` user must be in the `audio` group:

```bash
sudo usermod -aG audio $USER
# Log out and back in for the group change to take effect
```

### First Launch Workflow

1. **Install** with the one-liner or the DEB/RPM package.
2. **Start the daemon:** `systemctl --user enable --now always-daemon.service`
3. **Configure your audio input** — verify with `arecord -l`.
4. **Set a Groq API key** in the CLI or settings:

   ```bash
   always config set groq_api_key sk-your-key-here
   ```

5. **Start dictating** — the daemon runs in the background and types at your cursor.

### Troubleshooting

| Issue | Fix |
|---|---|
| Daemon fails to start | Check `journalctl --user -u always-daemon -n 50` for errors |
| No audio input detected | Verify with `arecord -l`; ensure PipeWire/PulseAudio is running |
| Permission denied on mic | Add user to `audio` group; reload udev rules |
| Paste doesn't work | Install `xclip` or `wl-clipboard` (for Wayland) |
| Overlay not visible | Set `ALSO_X11=true` for X11 sessions; for Wayland, use `ALSO_WAYLAND=true` |

---

## Windows

### Installation

Install from PowerShell (Windows 10 1909+ and Windows 11):

```powershell
irm https://raw.githubusercontent.com/LivioGama/always/main/scripts/install.ps1 | iex
```

To install by hand, download `always-vX.Y.Z-x86_64-pc-windows-msvc.zip` and its `.sha256` from [GitHub Releases](https://github.com/LivioGama/always/releases/latest), check the hash with `Get-FileHash`, and unzip it.

### Admin Requirements

Running Always with Administrator privileges is required to:

- Grant the Always service access to the microphone device.
- Install the global hotkey registration (requires kernel-level keyboard hook).
- Register the tray icon for per-user auto-start.

### Permissions

After installation, Windows may prompt for additional permissions:

#### 1. Microphone Access

```
Settings → Privacy & Security → Microphone → Allow apps to access your microphone
Ensure Always is toggled ON
```

#### 2. Desktop App Access

```
Settings → Privacy → Microphone → Let desktop apps access your microphone
Ensure Always.exe is toggled ON
```

### First Launch Workflow

1. **Install** with the PowerShell one-liner above.
2. **Launch Always** from the Start menu.
3. **Grant permissions** when prompted by Windows.
4. **Enroll your voice** — record three short samples in **Settings → My Voice**.
5. **Configure STT backend** — set a Groq API key in **Settings → Models**.
6. **Start dictating** — just talk.

### Troubleshooting

| Issue | Fix |
|---|---|
| Installer says "insufficient privileges" | Right-click → Run as Administrator |
| Microphone access denied | Check Settings → Privacy → Microphone |
| Hotkeys don't work | Ensure Always is running as Administrator |
| Paste doesn't work | Install a clipboard manager or check Windows clipboard history |
| Antivirus blocks the app | Add Always to your AV exclusions; builds are not code-signed yet, so SmartScreen or your AV may flag them |

---

## Common Issues

### API Key Configuration

Always stores API keys in the OS keychain, never in plaintext config files.

```bash
# Set your Groq API key (stored securely)
always config set groq_api_key sk-groq-your-key-here

# Verify it's set
always config show groq_api_key

# Reset it
always config reset groq_api_key
```

### Local Model Download

For offline transcription, download a model:

```bash
# List available models
always models list

# Download a model (e.g., Parakeet)
always models download parakeet-ctc-1.1b

# Switch to local mode
always config set stt_backend local
always config set stt_model parakeet-ctc-1.1b
```

### Log Files

- **macOS:** `~/Library/Logs/Always/`
- **Linux:** `~/.local/state/always/` or `journalctl --user -u always-daemon`
- **Windows:** `%LOCALAPPDATA%/Always/logs/`

### Verbose Logging

```bash
# Enable verbose output for debugging
always config set log_level debug

# Reset to default
always config set log_level info
```