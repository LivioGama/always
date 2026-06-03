# 🎤 Always — Voice Activation Daemon

High-performance voice-to-text automation for macOS Apple Silicon. Speak naturally and have your words instantly appear in any application.

## ✨ Quick Start

```bash
always config set groq_api_key "your-key"
always start
```

## 🤔 What is Always?

Always is an always-on voice activation daemon that:
- 🎧 **Listens continuously** using advanced Voice Activity Detection
- ⚡ **Transcribes instantly** via Groq's Whisper API
- 🧠 **Filters intelligently** to block filler words & politeness phrases
- 📋 **Pastes automatically** into any active application
- 🔄 **Runs in background** without interrupting your workflow

## 📦 Installation

### macOS Apple Silicon

The current release supports macOS Apple Silicon only. Linux support is planned for a future release.

```bash
cargo install always
```

**Note**: Requires macOS 14+ (Sonoma or later) on Apple Silicon. The daemon uses macOS-specific APIs for audio capture and clipboard automation.

## 🎮 Usage

```bash
always start    # Start daemon in background
always status   # Check if running
always stop     # Stop daemon
always run      # Run in foreground (debugging)
```

### ⌨️ Keyboard Shortcuts
- `Ctrl+Shift+P` — Pause/unpause voice listening
- `Ctrl+Shift+A` — Toggle auto-enter mode
- `Ctrl+C` — Stop daemon (foreground mode)

### 🎯 Overlay Display Rules

The overlay displays based on the current state in `~/.config/always/state.json`, which is polled every 33ms with the following priority order:

1. **Paused** (orange circle) — When voice listening is paused
2. **Auto-Enter** (green circle) — When auto-enter mode is enabled
3. **Processing** (blue circle) — When filtering/post-processing transcription (after voice detection)
4. **Transcribing** (purple circle) — When transcribing audio to text
5. **Hidden** — When no activity (overlay is not visible)

The overlay does **not** show during voice detection to avoid excessive visual feedback before filtering occurs.

### Configuration
```bash
always config show                                    # View all settings
always config set stt_energy_threshold 0.01           # Sensitivity (lower = more)
always config set stt_silence 0.4                     # Silence timeout (seconds)
always config set stt_auto_enter true                 # Auto-enter after paste
```

### 📖 Custom Vocabulary

Always biases Whisper transcription using `glossary.json` at the project root.
Add domain-specific terms (tools, product names, jargon) so they're transcribed
correctly instead of phonetically guessed.

**Schema** — each entry is an object with three fields:
```json
{
  "term": "Kubernetes",
  "mistranscriptions": ["cuber netties", "kubernetics"],
  "frequency": 100
}
```
- `term` — canonical spelling (required)
- `mistranscriptions` — common phonetic misreads the post-processor should fix (optional)
- `frequency` — higher values are prioritized in Whisper's 224-token bias prompt (default 100)

**Add a term** — edit `glossary.json` and append a new entry:
```bash
jq '. += [{"term":"Kubernetes","mistranscriptions":["cuber netties"],"frequency":100}]' \
  glossary.json > glossary.json.tmp && mv glossary.json.tmp glossary.json
```

**Import from installed STT software** (Dragon, macOS Dictation, Whisper, etc.):
```bash
always vocab import
```docs/
docs/
**Override the glossary path** by setting `ALWAYS_GLOSSARY_PATH` to a custom file.

## 📚 Documentation

- [ARCHITECTURE.md](ARCHITECTURE.md) — Architecture details
- [DEVELOPMENT.md](DEVELOPMENT.md) — Development & contributing
- [TROUBLESHOOTING.md](TROUBLESHOOTING.md) — Troubleshooting guide

## 📄 License

Apache-2.0

## Future Optimization Opportunities

The following optimizations were identified during a recent latency audit but not yet implemented. They're documented here for future work.

### 🟢 Real wins still on the table

1. **Native audio via `cpal`** — eliminates the SoX subprocess permanently. ~5-10ms latency cut, removes the `/opt/homebrew/bin/rec` external dependency, kills the persistent-recorder restart cycle. Significant rewrite (~half day).
2. **Speculative early transcription** — start sending audio to Whisper before silence is fully detected. Could shave 0.4-1s off perceived latency for typical utterances. Risky (doubles API usage if not capped).
3. **NSPasteboard via objc2** — finally remove the last subprocess (`pbcopy`) on the hot path. ~5-15ms saved per paste. Adds heavy `objc2` dependency.

### 🟡 Marginal — only worth it if optimizing for clean code

4. **Remove `smart_filter.rs` and `ai_filter.rs`** — only used by dev test bins now. Faster compile, smaller binary, but breaks `cargo run --bin filter_comparison`.
5. **Vocabulary fuzzy matching with `aho-corasick`** instead of sequential string replace — only helps with large vocabularies (current default is small).
6. **SQLite WAL + `PRAGMA synchronous=NORMAL`** — not a bottleneck (config is read once at startup).

### 🔴 Not worth doing

- SIMD energy calculation — already < 1ms, dwarfed by API time
- Local Whisper fallback — quality tradeoff isn't worth it for dictation
- Replacing `rdev` keyboard listener — works fine

### Honest assessment

The single biggest remaining win is **fixing the overlay regression** so we can re-enable Swift release mode (currently builds in debug to work around a regression introduced during the latency optimization pass). After that, **speculative transcription** is the next big lever because the API call is still ~60% of total latency. Everything else is polish.