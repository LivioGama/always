# 🏛️ Architecture

## Core Components

- `src/always/vad.rs` — Voice activity detection & recording
- `src/always/audio.rs` — Optimized audio processing with memory pooling
- `src/always/filter.rs` — Two-tier intelligent filtering
- `src/always/config.rs` — Configuration management
- `src/always/event_loop.rs` — Main daemon processing loop

## Performance Optimizations

- 🧠 **Memory Pooling** — Reused audio buffers
- 🌐 **Connection Reuse** — HTTP client pooling
- ⚡ **Optimized VAD** — Fast energy detection
- 🎙️ **Efficient Recording** — Persistent SoX processes
- 🎯 **Smart Filtering** — Two-stage filtering

## Vocabulary System

- 📚 Base vocabulary — Common programming terms
- 🔍 Context vocabulary — Project-specific terms
- 🧠 Learning system — Groq-powered corrections
- 🎨 Pattern matching — Code patterns & file paths
