<div align="center">

# Always

### **Speak. It types. Anywhere.**

The always-on dictation app. No push-to-talk, no window to open — just talk.

[![Release](https://img.shields.io/github/v/release/LivioGama/always)](https://github.com/LivioGama/always/releases/latest)
[![License](https://img.shields.io/github/license/LivioGama/always)](LICENSE)
[![macOS](https://img.shields.io/badge/macOS-14%2B-000000?style=flat&logo=apple)](https://developer.apple.com/macos/)
[![Linux](https://img.shields.io/badge/Linux-Ubuntu_22.04%2B%2C_Debian_12%2B%2C_Fedora_38%2B-000000?style=flat&logo=linux)](https://www.linux.org/)
[![Windows](https://img.shields.io/badge/Windows_10_1909%2B%2C_Windows_11-0078D6?style=flat&logo=windows)](https://www.windows.com/)

</div>

---

## ✨ What It Is

You speak. Your words appear at your cursor — in any app, any text field, anywhere. There's nothing to press, nothing to click, nothing to remember. Always lives in your menu bar, listens to your voice, and types what you say.

But Always is more than a speech-to-text tool — it's a philosophy. The idea is that your hands should be free. That speaking should be as natural as thinking, and that the technology should disappear so completely you forget it's there. It's hard to explain in words. You have to try it to understand — and once you do, you get used to it faster than you'd expect.

Other dictation tools want to be the center of attention. Always wants to disappear.

---

## 🎯 Key Features

🗣️ **Always-on dictation** — No push-to-talk. The microphone is live; speech becomes text automatically.

⌨️ **Types anywhere** — Words land at your cursor in any app — Xcode, Slack, Notion, Terminal, browser, anything.

✅ **Auto-enter, on by default** — Text is typed and sent automatically. Hold **⌘ (Cmd)** while speaking to prevent the send — your words appear but nothing gets submitted.

🎤 **My Voice** — Teach Always your voice with three quick samples. Once enrolled, it only listens to you — ignoring other people, videos, music, and calls, even while media plays at full volume.

☁️ **Cloud or local** — Use Groq's fast cloud API, or run models entirely on-device for offline privacy. Local models can also back up the cloud automatically.

🧠 **Self-improving vocabulary** — Correct a mistranscription once and Always learns the fix forever. Add your project's jargon and it stops turning "Kubernetes" into "cuber netties".

🛡️ **Smart filtering** — Catches the hallucinations speech models invent from silence — "thanks for watching", "[Music]", repeated tokens — before they reach your cursor.

🔀 **Microphone conflict handling** — Auto-pauses when another app needs the mic, auto-resumes when it's free. No fighting with Superwhisper, Zoom, or anything else.

---

## ⌨️ Keyboard Shortcuts

All shortcuts are customizable in **Settings → Shortcuts**.

| Shortcut | What it does |
|---|---|
| **Hold ⌘** while speaking | Prevent auto-enter — text appears but doesn't send |
| **⌃⌥V** | Paste raw unfiltered text (bypasses profanity/filler filtering) |
| **⌃⌥W** | Correct last transcript and teach your vocabulary |
| **⌃⌥P** | Pause / resume dictation |
| **⌃⌥A** | Toggle auto-enter on/off |
| **⌃⌥⇧P** | Master pause — survives across all pause sources |

> **Linux:** Replace **⌘** with **Super/Win** and **⌃⌥** with **Ctrl+Alt**.
> **Windows:** Replace **⌘** with **Win/Cmd** and **⌃⌥** with **Ctrl+Alt**.

---

## 🖥️ Installation

### macOS (Apple Silicon)

**Install** (Apple Silicon, one command):

```bash
curl -fsSL https://raw.githubusercontent.com/LivioGama/always/main/scripts/install-always.sh | bash
```

Or grab `always-macos-arm64.tar.gz` (with `SHA256SUMS`) from [Releases](https://github.com/LivioGama/always/releases/latest).

**Or build it** (no Homebrew, no SoX — everything Always needs ships inside the app):

```bash
# needs Xcode 26 (App Store) for the macOS 26 SDK
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh  # Rust
git clone https://github.com/LivioGama/always.git && cd always
cargo build --release --bin always
ALWAYS_BUILD_PROFILE=release ALWAYS_SWIFT_CONFIGURATION=release ./Always/build.sh   # installs /Applications/Always.app
```

**First launch** walks you through everything — nothing to set up beforehand:

1. **Microphone**, **Accessibility** (to type into other apps) and, on macOS 26+, **Speech Recognition** — each with a button that opens the right prompt or System Settings pane.
2. On macOS 26+ the on-device speech model downloads in the background, with progress. Transcription runs on your Mac with Apple's engine: no account, no API key. (A Groq key in Settings → Models switches to the cloud.)
3. **My Voice** (optional): three short samples.
4. **Pick the apps Always types into.** Every other app stays paused until you turn it on (menu bar → *Resume Always for …*).

**System Requirements:** Apple Silicon, macOS 14 (Sonoma) or later; macOS 26 (Tahoe) or later for on-device transcription without an API key. 4 GB RAM minimum.

---

### Linux

```bash
# Any distro (x86_64 or aarch64)
curl -fsSL https://raw.githubusercontent.com/LivioGama/always/main/scripts/install-always.sh | bash
```

Or install a package from [Releases](https://github.com/LivioGama/always/releases/latest):

```bash
# Debian / Ubuntu
wget https://github.com/LivioGama/always/releases/latest/download/always-linux-x86_64.deb
sudo apt install ./always-linux-x86_64.deb

# Fedora / RHEL
wget https://github.com/LivioGama/always/releases/latest/download/always-linux-x86_64.rpm
sudo dnf install ./always-linux-x86_64.rpm
```

(`aarch64` builds: replace `x86_64` with `aarch64`. A `.tar.gz` is also published.)

**After first install (Linux):**

1. Enable the microphone for your desktop environment.
2. Start the daemon with `sudo systemctl enable --now always-daemon` (systemd service provided).
3. Configure udev rules for microphone access if needed — see [docs/SETUP.md](docs/SETUP.md#linux).

**System Requirements:** Ubuntu 22.04+ / Debian 12+ / Fedora 38+, ALSA or PulseAudio/PipeWire, 4 GB RAM minimum.

---

### Windows

```powershell
irm https://raw.githubusercontent.com/LivioGama/always/main/scripts/install.ps1 | iex
```

Or download `always-windows-x86_64.zip` (with `SHA256SUMS`) from [Releases](https://github.com/LivioGama/always/releases/latest).

**After first install (Windows):**

1. Run the installer with **Administrator** privileges to grant full microphone access.
2. Grant microphone permissions in **Settings → Privacy → Microphone**.
3. Launch Always from the Start menu.

**System Requirements:** Windows 10 1909+ or Windows 11, 4 GB RAM minimum.

---

## 🎤 My Voice

Optional, but it makes a big difference. Record three short samples and Always builds a voiceprint that lets it ignore everything except you — other people in the room, the TV, a podcast, a Zoom call. Your voiceprint never leaves your machine.

You can re-record anytime from **Settings → My Voice**.

---

## 🔧 Settings

Nine panels, all in the menu bar:

| Panel | What you'll find |
|---|---|
| **General** | Language, display mode, sound cues, startup |
| **Models** | Cloud/local backend, API key, model downloads |
| **My Voice** | Voice enrollment and threshold tuning |
| **Permissions** | Microphone, Accessibility, Input Monitoring |
| **Behavior** | Silence timing, auto-enter, live preview, idle pause |
| **Shortcuts** | Every shortcut, editable inline |
| **Library** | Vocabulary glossary and text snippets |
| **History** | Transcript history |
| **About** | Version, updates, links |

---

## 🔒 Privacy

- **No telemetry.** No analytics. No account. The app never phones home.
- **Cloud mode sends audio to Groq** — that's the only network destination. Local mode sends nothing.
- **API keys live in the OS Keychain** — never in config files, never in logs.
- **Your voiceprint stays local** — embeddings are stored on your machine and never transmitted.
- **Transcripts are not logged by default** in release builds.

---

## 📚 Documentation

| Document | Description |
|---|---|
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | System architecture and component diagrams |
| [docs/SETUP.md](docs/SETUP.md) | Platform-specific setup and configuration |
| [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) | Build, test, and contribute |
| [docs/TROUBLESHOOTING.md](docs/TROUBLESHOOTING.md) | Common issues and fixes |
| [docs/RELEASE.md](docs/RELEASE.md) | Release process and signing |

---

## 📝 License

[AGPL-3.0](LICENSE). Open source. Use it, modify it, distribute it — but if you modify and distribute it (including as a network service), share your modifications under the same license.

---

<div align="center">

**Built by humans who got tired of typing.**

[⭐ Star this repo](../../) ·
[Issues](https://github.com/LivioGama/always/issues) ·
[Discussions](https://github.com/LivioGama/always/discussions) ·
[Releases](https://github.com/LivioGama/always/releases)

</div>