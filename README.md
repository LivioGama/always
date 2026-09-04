<div align="center">

# Always

### **Speak. It types. Anywhere.**

The always-on dictation app. No push-to-talk, no window to open — just talk.

[![Release](https://img.shields.io/github/v/release/LivioGama/always)](https://github.com/LivioGama/always/releases/latest)
[![License](https://img.shields.io/github/license/LivioGama/always)](LICENSE)

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

## ⌨️ Shortcuts

| Shortcut | What it does |
|---|---|
| **Hold ⌘** while speaking | Prevent auto-enter — text appears but doesn't send |
| **⌃⌥V** | Paste raw unfiltered text (bypasses profanity/filler filtering) |
| **⌃⌥W** | Correct last transcript and teach your vocabulary |
| **⌃⌥P** | Pause / resume dictation |
| **⌃⌥A** | Toggle auto-enter on/off |
| **⌃⌥⇧P** | Master pause — survives across all pause sources |

All shortcuts are customizable in Settings → Shortcuts.

---

## 🎤 My Voice

Optional, but it makes a big difference. Record three short samples and Always builds a voiceprint that lets it ignore everything except you — other people in the room, the TV, a podcast, a Zoom call. Your voiceprint never leaves your machine.

You can re-record anytime from Settings → My Voice.

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
