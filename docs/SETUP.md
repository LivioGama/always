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

Always ships as a **signed and notarized DMG** for macOS.

#### Homebrew (Recommended)

```bash
# Add the tap
brew tap liviogama/also

# Install
brew install --cask also
```

#### Manual Install

1. Download the latest `.dmg` from [GitHub Releases](https://github.com/LivioGama/also/releases/latest).
2. Open the DMG file.
3. Drag `Always.app` into your `Applications` folder.
4. Launch from `Applications` or Spotlight (`⌘ + Space`, type "Always").

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
xattr -d com.apple.quarantine /Applications/Also.app

# Re-notarize check (should pass automatically)
codesign --verify --deep --strict /Applications/Also.app

# If already notarized, you should see:
# /Applications/Also.app: valid on disk
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
| "Always" is damaged and can't be opened | Run `xattr -d com.apple.quarantine /Applications/Also.app` in Terminal |
| Microphone access denied | Re-check permissions in System Settings → Privacy & Security |
| Overlay doesn't appear | Grant Accessibility permission; check System Settings → Privacy & Security → Screen Recording |
| App quits immediately | Check `~/Library/Logs/Also/` for crash logs |

---

## Linux

### Installation

Always ships as a **DEB package** for Debian-based distributions and an **RPM** for Fedora/RHEL.

#### Debian / Ubuntu

```bash
# Download the latest release
wget https://github.com/LivioGama/also/releases/latest/download/also_0.0.1_amd64.deb

# Install (this places the daemon binary, desktop file, and systemd service)
sudo apt install ./also_0.0.1_amd64.deb
```

#### Fedora / RHEL

```bash
# Download the latest release
wget https://github.com/LivioGama/also/releases/latest/download/also-0.0.1-x86_64.rpm

# Install
sudo dnf install ./also-0.0.1-x86_64.rpm
```

### systemd Service

The DEB/RPM packages install a systemd service for the Always daemon:

```bash
# Start the daemon now
sudo systemctl start also-daemon

# Enable auto-start on boot
sudo systemctl enable also-daemon

# Check status
systemctl --user status also-daemon

# View logs
journalctl --user -u also-daemon -f
```

The daemon runs as the **current user** (via `--user` scope), so no root privileges are needed at runtime. The `sudo` is only required for the initial service enable/start.

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
# The package installs /etc/udev/rules.d/99-also-microphone.rules automatically.
# If missing, create it manually:

sudo tee /etc/udev/rules.d/99-also-microphone.rules > /dev/null << 'EOF'
# Always daemon — allow the current user to access USB/midi audio interfaces
SUBSYSTEM=="snd", GROUP="audio", MODE="0660"
SUBSYSTEM=="sound", GROUP="audio", MODE="0660"
EOF

# Reload rules
sudo udevadm control --reload-rules
sudo udevadm trigger
```

**Note:** The `also` user must be in the `audio` group:

```bash
sudo usermod -aG audio $USER
# Log out and back in for the group change to take effect
```

### First Launch Workflow

1. **Install the DEB/RPM package.**
2. **Start the daemon:** `sudo systemctl --user enable --now also-daemon`
3. **Configure your audio input** — verify with `arecord -l`.
4. **Set a Groq API key** in the CLI or settings:

   ```bash
   also config set groq_api_key sk-your-key-here
   ```

5. **Start dictating** — the daemon runs in the background and types at your cursor.

### Troubleshooting

| Issue | Fix |
|---|---|
| Daemon fails to start | Check `journalctl --user -u also-daemon -n 50` for errors |
| No audio input detected | Verify with `arecord -l`; ensure PipeWire/PulseAudio is running |
| Permission denied on mic | Add user to `audio` group; reload udev rules |
| Paste doesn't work | Install `xclip` or `wl-clipboard` (for Wayland) |
| Overlay not visible | Set `ALSO_X11=true` for X11 sessions; for Wayland, use `ALSO_WAYLAND=true` |

---

## Windows

### Installation

Always ships as an **MSI installer** for Windows 10 1909+ and Windows 11.

#### Manual Install

1. Download the latest `.msi` from [GitHub Releases](https://github.com/LivioGama/also/releases/latest).
2. **Right-click → Run as Administrator** (required for microphone and input access).
3. Follow the installation wizard.
4. Launch Always from the Start menu.

#### PowerShell (One-liner)

```powershell
# Download and install silently
$msi = "https://github.com/LivioGama/also/releases/latest/download/also-0.0.1-x64.msi"
Invoke-WebRequest -Uri $msi -OutFile "$env:TEMP/also.msi"
Start-Process msiexec.exe -ArgumentList "/i `"$env:TEMP/also.msi`" /quiet /norestart" -Wait
```

### Admin Requirements

Running the MSI with Administrator privileges is required to:

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

1. **Install** the MSI as Administrator.
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
| Antivirus blocks the app | Add Always to your AV exclusions; the MSI is signed with a trusted cert |

---

## Common Issues

### API Key Configuration

Always stores API keys in the OS keychain, never in plaintext config files.

```bash
# Set your Groq API key (stored securely)
also config set groq_api_key sk-groq-your-key-here

# Verify it's set
also config show groq_api_key

# Reset it
also config reset groq_api_key
```

### Local Model Download

For offline transcription, download a model:

```bash
# List available models
also models list

# Download a model (e.g., Parakeet)
also models download parakeet-ctc-1.1b

# Switch to local mode
also config set stt_backend local
also config set stt_model parakeet-ctc-1.1b
```

### Log Files

- **macOS:** `~/Library/Logs/Also/`
- **Linux:** `~/.local/state/also/` or `journalctl --user -u also-daemon`
- **Windows:** `%LOCALAPPDATA%/Also/logs/`

### Verbose Logging

```bash
# Enable verbose output for debugging
also config set log_level debug

# Reset to default
also config set log_level info
```