<div align="center">

<img src="icons/icon.png" width="128" height="128" alt="Always logo: a microphone wrapped in an infinity loop">

# Always

### Speak. It types. Anywhere.

The always-on dictation app. No push-to-talk, no window to open — just talk.

[![Latest release](https://img.shields.io/github/v/release/LivioGama/always?label=release&color=22d3ee)](https://github.com/LivioGama/always/releases/latest)
[![License: AGPL-3.0](https://img.shields.io/github/license/LivioGama/always?color=blue)](LICENSE)
[![macOS 14+ · Apple Silicon](https://img.shields.io/badge/macOS-14%2B%20%C2%B7%20Apple%20Silicon-000000?logo=apple&logoColor=white)](#-macos)
[![Linux · x86_64 · arm64](https://img.shields.io/badge/Linux-x86__64%20%C2%B7%20arm64-FCC624?logo=linux&logoColor=black)](#-linux)
[![Windows 10 · 11](https://img.shields.io/badge/Windows-10%20%C2%B7%2011-0078D6)](#-windows)

**[Website](https://liviogama.github.io/always/)** · **[Download](https://github.com/LivioGama/always/releases/latest)** · [Install](#-install) · [Features](#-features) · [Shortcuts](#-shortcuts) · [Privacy](#-privacy)

</div>

---

## ✨ What it is

You speak. Your words appear at your cursor — in any app, any text field. Nothing to press, nothing to click, nothing to remember. Always sits in your menu bar, listens, and types what you say.

It's less a speech-to-text tool than an idea: your hands should be free, speaking should feel as natural as thinking, and the technology should disappear so completely you forget it's there. That's hard to explain in words. Try it for a day — you'll get used to it faster than you expect.

> **Other dictation tools want to be the center of attention. Always wants to disappear.**

| 1 · Talk | 2 · Always listens | 3 · Your words land |
|:--|:--|:--|
| Just start speaking. No hotkey, no button. | A small floating badge shows it's listening and previews your words live. | The text is typed at your cursor, then sent after a 4-second countdown. Press any key to cancel. |

---

## 🎯 Features

> Everything below is the macOS app. On Linux and Windows, Always runs as a background daemon you drive from the command line — see [Install](#-install).

### 🗣️ Hands-free

- **Always on** — no push-to-talk. Start speaking and your words appear.
- **Types anywhere** — at your cursor in any app: Xcode, Slack, Notion, Terminal, your browser.
- **Auto-enter, with a safety net** — after typing, Always presses Return following a 4-second countdown. Any key cancels it; <kbd>⌃⌥A</kbd> turns it off.
- **Edit with your voice** — *"scratch that"*, *"change Tuesday to Wednesday"*, *"new line"*, *"make that a list"*, *"make that more formal"*, *"undo"*.
- **Snippets** — say a trigger phrase and Always types your saved text, word for word.

### 🎤 Accurate

- **My Voice** — record three short samples and Always listens only to you: not the people around you, not the call on your other screen. Your voiceprint never leaves your Mac.
- **Talk over music and video** — echo cancellation removes your Mac's own sound from the mic. With My Voice on, you never have to pause a video to dictate.
- **Learns your vocabulary** — correct a word once (<kbd>⌃⌥W</kbd>) and it stays fixed. Add your project's jargon and "Kubernetes" stops coming out as "cuber netties".
- **Hallucination filter** — drops what speech models invent out of silence: "thanks for watching", subtitle credits, repeated words.

### 🔐 In your control

- **You choose where it types** — Always starts paused in every app. Turn it on for the apps you want (<kbd>⌃⌥P</kbd>), so text never lands in the wrong window.
- **On-device by default** — on macOS 26+, Apple's speech engine transcribes on your Mac: no account, no API key. Prefer the cloud? Switch to [Groq](https://groq.com).
- **Plays nice with other mic apps** — pauses when Zoom, Superwhisper or anything else takes the microphone, and resumes when it's free.
- **Nothing gets lost** — whatever Always holds back is kept, and <kbd>⌃⌥V</kbd> pastes it.

---

## ⚡ Shortcuts

| Shortcut | What it does |
|:--|:--|
| <kbd>⌃</kbd> <kbd>⌥</kbd> <kbd>P</kbd> | Turn Always on or off for the current app |
| <kbd>⌃</kbd> <kbd>⌥</kbd> <kbd>⇧</kbd> <kbd>P</kbd> | Master pause — everywhere at once |
| <kbd>⌃</kbd> <kbd>⌥</kbd> <kbd>A</kbd> | Auto-enter on / off |
| <kbd>⌃</kbd> <kbd>⌥</kbd> <kbd>V</kbd> | Paste the last transcript Always held back |
| <kbd>⌃</kbd> <kbd>⌥</kbd> <kbd>W</kbd> | Correct the last transcript — Always learns the fix |
| <kbd>⌃</kbd> <kbd>⌥</kbd> <kbd>X</kbd> | Learn from your edit — select the text you fixed, press it, and Always compares it with what it typed |
| Hold <kbd>⌘</kbd> as the text arrives | Skip this paste (it's kept — <kbd>⌃⌥V</kbd> pastes it) |
| Hold <kbd>⌥</kbd> while speaking | Keep the sentence open through a long pause |

Change them in **Settings → Shortcuts**. Keyboard shortcuts are macOS-only; on Linux and Windows use `always toggle-pause` and `always toggle-auto-enter`.

---

## 🚀 Install

|  |  macOS | 🐧 Linux | 🪟 Windows |
|:--|:--|:--|:--|
| **You get** | The full app — menu bar, settings, guided setup | The `always` daemon + CLI | The `always` daemon + CLI |
| **Requires** | macOS 14+ on Apple Silicon | glibc 2.39+ (Ubuntu 24.04+, Debian 13+, Fedora 40+), x86_64 or arm64 | Windows 10 or 11, x64 |
| **Speech engine** | Apple on-device (macOS 26+) or Groq | Groq ([free API key](https://console.groq.com/keys)) | Groq ([free API key](https://console.groq.com/keys)) |

###  macOS

```bash
curl -fsSL https://raw.githubusercontent.com/LivioGama/always/main/scripts/install-always.sh | bash
```

This installs **Always.app** in `/Applications` and opens it. The first launch walks you through the rest:

1. **Permissions** — Microphone, Accessibility (to type into other apps) and, on macOS 26+, Speech Recognition. Each has a button that opens the right prompt.
2. **Speech model** — on macOS 26+, Apple's on-device model downloads in the background, with progress.
3. **My Voice** *(optional)* — three short samples.
4. **Where to type** — pick the apps Always types into. Every other app stays paused until you turn it on with <kbd>⌃⌥P</kbd> or from the menu bar (*Resume Always for …*).

On macOS 14–25, or to use the cloud: add a [Groq API key](https://console.groq.com/keys) in **Settings → Models** and choose **Use Groq (Remote)**.

<details>
<summary><b>Manual download or build from source</b></summary>

<br>

**Manual download** — grab `always-vX.Y.Z-aarch64-apple-darwin.tar.gz` and its `.sha256` from [Releases](https://github.com/LivioGama/always/releases/latest).

**Build from source** — needs Rust and Xcode 26 (older SDKs build without on-device Apple speech). No Homebrew, no SoX: everything Always needs ships inside the app.

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # Rust
git clone https://github.com/LivioGama/always.git && cd always
cargo build --release --bin always
ALWAYS_BUILD_PROFILE=release ALWAYS_SWIFT_CONFIGURATION=release ./Always/build.sh   # installs /Applications/Always.app
```

</details>

### 🐧 Linux

```bash
curl -fsSL https://raw.githubusercontent.com/LivioGama/always/main/scripts/install-always.sh | bash
```

The installer picks the `.deb`, `.rpm` or tarball for your machine. Then save your Groq key and start the daemon:

```bash
always config set groq_api_key <your-groq-key>
systemctl --user enable --now always-daemon.service
```

<details>
<summary><b>Install a package manually</b></summary>

<br>

```bash
V=0.1.0   # the latest release, without the "v"

# Debian / Ubuntu
wget https://github.com/LivioGama/always/releases/download/v$V/always-v$V-x86_64-unknown-linux-gnu.deb
sudo apt install ./always-v$V-x86_64-unknown-linux-gnu.deb

# Fedora / RHEL
wget https://github.com/LivioGama/always/releases/download/v$V/always-v$V-x86_64-unknown-linux-gnu.rpm
sudo dnf install ./always-v$V-x86_64-unknown-linux-gnu.rpm
```

On arm64, replace `x86_64` with `aarch64`. A `.tar.gz` is published too.

</details>

### 🪟 Windows

```powershell
irm https://raw.githubusercontent.com/LivioGama/always/main/scripts/install.ps1 | iex
```

This installs `always.exe` in `%LOCALAPPDATA%\Programs\Always` and adds it to your `PATH` — no admin rights needed. Then:

1. Allow microphone access: **Settings → Privacy & security → Microphone → Let desktop apps access your microphone**.
2. In a new terminal:

   ```powershell
   always config set groq_api_key <your-groq-key>
   always run
   ```

Prefer a manual download? Grab `always-vX.Y.Z-x86_64-pc-windows-msvc.zip` and its `.sha256` from [Releases](https://github.com/LivioGama/always/releases/latest).

---

## 🔧 Settings

On macOS, everything lives in one Settings window, opened from the menu bar:

| Panel | What's inside |
|:--|:--|
| **General** | Sound cues, status overlay, the apps Always types into |
| **Models** | Speech engine (Apple on-device or Groq), Groq API key, language |
| **My Voice** | Record or re-record your voiceprint |
| **Permissions** | Microphone and Accessibility |
| **Behavior** | Silence timing, auto-enter, live preview, idle pause |
| **Shortcuts** | Every shortcut, editable inline |
| **Library** | Vocabulary and snippets |
| **History** | Recent transcripts (when transcript logging is on — see below) |
| **About** | Version and updates |

---

## 🔒 Privacy

- **No telemetry, no analytics, no account.**
- **On-device by default** — on macOS 26+, Apple's speech engine transcribes on your Mac.
- **Groq only if you add a key** — audio goes to Groq when it's your speech engine, and transcript text when Groq does the clean-up pass. Without a key nothing is sent; the daemon only opens an empty warm-up connection to `api.groq.com` at startup.
- **Other traffic** — update checks against GitHub Releases (macOS) and one-time model downloads.
- **Your voiceprint stays local** — in `~/Library/Application Support/always/voiceprint.json`, never uploaded.
- **Transcripts stay out of the logs** in release builds, unless you opt in with `ALWAYS_LOG_TRANSCRIPTS=1`.
- **Your API key** is stored in Always's local database (not the Keychain) or read from `GROQ_API_KEY`, and is never written to logs.

---

## 📚 Documentation

| Document | What's in it |
|:--|:--|
| [docs/SETUP.md](docs/SETUP.md) | Platform setup and configuration |
| [docs/TROUBLESHOOTING.md](docs/TROUBLESHOOTING.md) | Common issues and fixes |
| [SPEC.md](SPEC.md) | How Always is supposed to behave — the reference the code answers to |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | System architecture and component diagrams |
| [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) | Building and testing |
| [CONTRIBUTING.md](CONTRIBUTING.md) | How to contribute |
| [docs/RELEASE.md](docs/RELEASE.md) | Release pipeline, assets and provenance |
| [CHANGELOG.md](CHANGELOG.md) | What changed in each release |
| [SECURITY.md](SECURITY.md) | Reporting a vulnerability |

---

## 📝 License

[AGPL-3.0](LICENSE). Use it, modify it, share it — if you distribute a modified version (including as a network service), publish your changes under the same license.

---

<div align="center">

**Built by humans who got tired of typing.**

[Website](https://liviogama.github.io/always/) ·
[Releases](https://github.com/LivioGama/always/releases) ·
[Issues](https://github.com/LivioGama/always/issues) ·
[⭐ Star on GitHub](https://github.com/LivioGama/always)

</div>
