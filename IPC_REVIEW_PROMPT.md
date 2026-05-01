# IPC Architecture Review Request

## Current Architecture

I have a macOS application with two processes:
1. **Rust daemon** (`always`) - runs in background, captures audio, performs transcription
2. **Swift GUI app** (`AlwaysApp.app`) - status bar app that displays daemon state

**Current communication method:** The daemon writes state updates to a JSON file at `~/.config/always/state.json`. The Swift app reads this file to get the current state.

### State Structure
```json
{
  "listening": bool,
  "processing": bool,
  "transcribing": bool,
  "paused": bool,
  "auto_enter": bool,
  "voice_activity": bool,
  "last_transcript": string?,
  "last_updated": uint64
}
```

### Current Implementation Details
- **Daemon (Rust):** Writes to state.json multiple times per utterance (listening, transcribing, transcript, etc.)
- **Swift app:** Uses `DispatchSourceFileSystemObject` to monitor file changes (kernel-level file events, no polling)
- **File writes:** Previously caused IO massacre, now commented out in daemon to reduce load

## The Problem

File-based IPC is inefficient and causes issues:
1. **IO overhead:** Writing to disk is slow compared to memory
2. **Complexity:** Need to handle file locking, atomic writes, temporary files
3. **Latency:** File system operations add latency
4. **Scalability:** Doesn't scale well with frequent updates
5. **Overhead:** Even with efficient file monitoring, the file itself is unnecessary

## Proposed Solutions

### 1. Unix Domain Socket
**Pros:**
- Bidirectional communication
- Efficient (no disk IO)
- Simple to implement
- Works well for local IPC
- Standard Unix approach

**Cons:**
- Need to define message protocol
- Socket connection management
- Serialization overhead (but less than file IO)

**Implementation complexity:** Medium

### 2. XPC (Apple's IPC Framework)
**Pros:**
- Native macOS solution
- Built-in security and sandboxing
- Efficient
- Apple-recommended for macOS apps

**Cons:**
- Complex to implement with Rust daemon (XPC is Swift/Objective-C focused)
- May require bridging or Rust bindings
- Steeper learning curve

**Implementation complexity:** High

### 3. Shared Memory
**Pros:**
- Fastest possible (no serialization)
- Zero-copy communication
- Extremely low latency

**Cons:**
- Complex synchronization needed
- Memory management complexity
- Harder to debug
- Need to handle concurrent access safely

**Implementation complexity:** Very High

### 4. HTTP/WebSocket Server
**Pros:**
- Well-understood protocol
- Easy to debug
- Language-agnostic

**Cons:**
- Overkill for local IPC
- HTTP overhead
- Network stack overhead

**Implementation complexity:** Low-Medium

## Questions for Review

1. **Which IPC mechanism is best for this use case?** (daemon + GUI app on macOS)
2. **Is Unix domain socket the right choice, or is there a better option?**
3. **What are the trade-offs I should consider?**
4. **Are there any macOS-specific solutions I should know about?**
5. **How complex would each solution be to implement?**
6. **Should I keep the current file-based approach with DispatchSource, or is it worth refactoring?**

## Context
- The daemon is written in Rust
- The GUI is written in Swift
- Both run on the same machine (local IPC only)
- State updates happen frequently (multiple times per utterance)
- Low latency is important for responsive UI
- The app is a voice-to-text tool, so performance matters

Please provide recommendations with pros/cons and implementation complexity estimates.
