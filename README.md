<div align="center">

<img src="icons/icon.png" width="128" height="128" alt="Always logo: a microphone wrapped in an infinity loop">

# Always

### Speak. It types. Anywhere.

The always-on dictation app. No push-to-talk, no window to open — just talk.

[![Latest release](https://img.shields.io/github/v/release/LivioGama/always?label=release&color=22d3ee)](https://github.com/LivioGama/always/releases/latest)
[![License: AGPL-3.0](https://img.shields.io/github/license/LivioGama/always?color=blue)](LICENSE)
[![macOS 14+ · Apple Silicon](https://img.shields.io/badge/macOS-14%2B%20%C2%B7%20Apple%20Silicon-000000?logo=apple&logoColor=white)](#-install)
[![Linux · x86_64 · arm64](https://img.shields.io/badge/Linux-x86__64%20%C2%B7%20arm64-FCC624?logo=linux&logoColor=black)](#-install)
[![Windows 10 · 11](https://img.shields.io/badge/Windows-10%20%C2%B7%2011-0078D6)](#-install)

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

### 📖 Day one, it already knows your words

The first time you open Always, before you've said a single word, it quietly reads the names of the apps on your Mac, your macOS Text Replacements, and the dictionaries you spent months building in **Wispr Flow** and **SuperWhisper** — and makes them its own. So the very first time you say "Notion", "Kubernetes" or that client's name you once taught Wispr Flow, it comes out right. Switching costs you nothing you already taught another app.

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

** macOS** (14+, Apple Silicon) and **🐧 Linux** (x86_64 or arm64, glibc 2.39+):

```bash
curl -fsSL https://raw.githubusercontent.com/LivioGama/always/main/scripts/install-always.sh | bash
```

**🪟 Windows** (10 or 11, x64):

```powershell
irm https://raw.githubusercontent.com/LivioGama/always/main/scripts/install.ps1 | iex
```

On macOS you get the full app, and its first launch walks you through everything. On Linux and Windows you get the `always` daemon and CLI: add a [free Groq key](https://console.groq.com/keys) and start it (details below).

### 🤖 Let your agent install it

Paste this into Claude Code, Codex or any coding agent with a shell:

```text
Install Always (https://github.com/LivioGama/always) for me: follow the
"For AI agents" section of its README, then tell me what you did and
what is left for me to click.
```

<details>
<summary><b>After you install · manual downloads · build from source</b></summary>

<br>

** macOS, first launch**

1. Grant **Microphone**, **Accessibility** (to type into other apps) and, on macOS 26+, **Speech Recognition**. Each has a button that opens the right prompt.
2. On macOS 26+, Apple's on-device speech model downloads in the background. No account, no API key.
3. *Optional:* record three short samples for **My Voice**.
4. Pick the apps Always types into. Every other app stays paused until you turn it on with <kbd>⌃⌥P</kbd>.

On macOS 14–25, or to use the cloud: add a Groq key in **Settings → Models** and choose **Use Groq (Remote)**.

**🐧 Linux**

```bash
always config set groq_api_key <your-groq-key>
systemctl --user enable --now always-daemon.service
```

**🪟 Windows:** allow microphone access (**Settings → Privacy & security → Microphone → Let desktop apps access your microphone**), then in a new terminal:

```powershell
always config set groq_api_key <your-groq-key>
always run
```

**Manual downloads:** every installer and package (`.tar.gz`, `.deb`, `.rpm`, `.zip`), each with its `.sha256`, is on [Releases](https://github.com/LivioGama/always/releases/latest).

**Build from source (macOS):** needs Rust and Xcode 26 (older SDKs build without on-device Apple speech). No Homebrew, no SoX: everything Always needs ships inside the app.

```bash
git clone https://github.com/LivioGama/always.git && cd always
cargo build --release --bin always
ALWAYS_BUILD_PROFILE=release ALWAYS_SWIFT_CONFIGURATION=release ./Always/build.sh   # installs /Applications/Always.app
```

</details>

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

## 🤖 For AI agents

If you are an AI agent setting Always up for a user, do this, in order. Only install it if the user asked you to.

1. **Check first.** macOS: `ls /Applications/Always.app`. Linux and Windows: `always --version`. If it is already installed, skip to step 5.
2. **Check the machine.** macOS needs 14+ on Apple Silicon (`sw_vers -productVersion`, and `uname -m` prints `arm64`). Linux needs glibc 2.39+ (`ldd --version`) on x86_64 or arm64. Windows needs 10 or 11 on x64. If the machine doesn't qualify, stop and tell the user why.
3. **Install** with the one-liner for the OS from [Install](#-install). On macOS and Windows it needs no admin rights; on Linux, installing the package may ask for `sudo`.
4. **Finish setup.**
   - **macOS:** the installer opens Always. Tell the user the first-launch window asks for Microphone, Accessibility and (macOS 26+) Speech Recognition, and that only they can grant these.
   - **Linux and Windows:** ask the user for their Groq API key (free at https://console.groq.com/keys) and run `always config set groq_api_key <key>`. Then start it: on Linux `systemctl --user enable --now always-daemon.service`; on Windows `always run` in a new terminal.
5. **Verify** with `always status` (on macOS: `/Applications/Always.app/Contents/MacOS/always-daemon status`). It must print `Always-on daemon running`. Report what it printed, and do not claim success otherwise.
6. **Tell the user** they can start talking, and that on macOS Always only types into the apps they picked during setup (<kbd>⌃⌥P</kbd> adds the current one).

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
