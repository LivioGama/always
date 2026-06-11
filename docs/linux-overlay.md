# Linux Overlay Implementation

The Linux overlay is a companion process that mirrors the macOS status overlay using the existing daemon event stream. It does not record audio, transcribe, paste, or make pause decisions.

## Architecture

The overlay process:
1. Connects to `/run/user/$UID/always.sock`.
2. Reads newline-delimited daemon `DaemonEvent` JSON.
3. Reduces daemon events into overlay states with the same persistent/flash/hidden model as macOS.
4. Renders a compact X11 HUD window near the bottom center of the screen.

## Components

### State Reducer

`src/overlay/state.rs`

- Reuses `always::always::event::DaemonEvent`.
- Tracks persistent states: listening, transcribing, auto-enter countdown.
- Tracks flash states: paused, resumed, auto-enter on/off, filtered, transcription failed, grammar corrected, low microphone volume, idle auto-paused.
- Restores the pending persistent state after a flash expires.
- Suppresses initial-sync toggle flashes after `Hello`.

### UDS Client

`src/overlay/uds_client.rs`

- Connects to the daemon UDS socket.
- Parses newline-delimited events.
- Surfaces socket close/errors so the main loop can reconnect.

### Renderer

`src/overlay/renderer.rs`

- Uses X11 through the `x11` crate.
- Creates a borderless bottom-center HUD window.
- Sets always-on-top/sticky/taskbar-skip hints where the window manager supports them.
- Draws a dark panel with light text and a colored state indicator.
- Unmaps the window on `OverlayState::Hidden`.

### CLI Integration

- `always-overlay` runs the overlay in the foreground.
- `always overlay run` spawns the sibling `always-overlay` binary and exits.
- `systemd/always-overlay.service` runs `/usr/local/bin/always-overlay`.

## Building

```bash
cargo build --no-default-features --features overlay
cargo build --no-default-features --features overlay --bin always-overlay
```

The `overlay` feature enables Xlib bindings. The build environment must have X11 development/runtime libraries available.

## Running

```bash
# Start daemon first
always run

# In another shell
./target/debug/always-overlay

# Or through the main CLI, when both binaries are installed side by side
always overlay run
```

## Event Behavior

Persistent states remain visible until cleared:

- `VoiceActivityDetected` -> Listening
- `TranscribingStarted` -> Transcribing
- `AutoEnterCountdownStarted` / `AutoEnterCountdownTick` -> Auto-enter countdown

Flash states auto-hide after their duration and then restore any pending persistent state:

- `Paused` / `Resumed`
- `AutoEnterEnabled` / `AutoEnterDisabled`
- `TranscriptionFiltered`
- `TranscriptionFailed`
- `GrammarCorrected`
- `LowMicrophoneVolume`
- `IdleAutoPaused`

Hidden/state-only events:

- `TranscriptFinal` clears listening/transcribing state.
- `AutoEnterCountdownCancelled` / `AutoEnterCountdownFinished` clear countdown state.
- `PausedQuietly` / `ResumedQuietly` update state without flashing.

## Testing

```bash
cargo test --no-default-features --features overlay --bin always-overlay
bash test-overlay.sh
```

Manual X11 smoke test:

1. Start the daemon: `always run`.
2. Start the overlay: `./target/debug/always-overlay`.
3. Speak or trigger daemon events.
4. Verify the HUD appears near the bottom center, updates for listening/transcribing/countdown, flashes pause/error states, and hides when state clears.
5. Restart the daemon and verify the overlay reconnects.

## Platform Limitations

- Current GUI renderer is X11 only.
- Wayland compositors need a layer-shell renderer; this is future work.
- If `XOpenDisplay` fails, verify the process has `DISPLAY` and `XAUTHORITY` for an X11 session.
