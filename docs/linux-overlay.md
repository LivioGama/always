# Linux Overlay Implementation

The Linux overlay is a companion process that mirrors the macOS status overlay using the existing daemon event stream. It does not record audio, transcribe, paste, or make pause decisions.

## Architecture

The overlay process:
1. Connects to `/run/user/$UID/always.sock`.
2. Reads newline-delimited daemon `DaemonEvent` JSON.
3. Reduces daemon events into overlay states with the same persistent/flash/hidden model as macOS.
4. Detects display server (X11 or Wayland) and creates appropriate renderer.
5. Renders a compact HUD window near the bottom center of the screen.

## Backend Architecture

The overlay uses a backend-agnostic architecture with a trait-based interface:

### Backend Trait (`src/overlay/backend.rs`)
- `OverlayBackend` trait defines the interface for all renderers
- Backend detection automatically chooses X11 or Wayland based on environment
- Runtime detection uses `WAYLAND_DISPLAY` and `DISPLAY` environment variables

### X11 Backend (`src/overlay/renderer.rs`)
- Implements `OverlayBackend` for X11 sessions
- Creates always-on-top window with `_NET_WM_STATE_ABOVE` hint
- Window type: `_NET_WM_WINDOW_TYPE_NOTIFICATION` (compatible with most WMs)
- Properties: non-decorated, skip taskbar/pager, skip focus (doesn't steal input)
- Position: bottom-center, 160px from bottom
- Styling: GitHub dark theme colors, state indicator circle, rounded corners

### Wayland Backend (`src/overlay/wayland.rs`)
- Placeholder for layer-shell implementation
- Currently returns error with clear message about limitations
- Designed for wlroots-based compositors (Sway, Hyprland)
- Future work: Implement actual layer-shell protocol

### Backend Factory (`src/overlay/factory.rs`)
- `create_renderer()` detects backend and creates appropriate renderer
- Prefers Wayland if available, falls back to X11
- Returns error if no display server detected

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

`src/overlay/renderer.rs` (X11 backend)

- Uses X11 through the `x11` crate
- Implements `OverlayBackend` trait
- Creates a borderless bottom-center HUD window
- Sets always-on-top/sticky/taskbar-skip hints where the window manager supports them
- Draws a dark panel with light text and a colored state indicator
- Unmaps the window on `OverlayState::Hidden`
- Styling: GitHub dark theme colors, state indicator circle, rounded corners
- Focus handling: Skip focus hint to avoid stealing keyboard input

`src/overlay/wayland.rs` (Wayland backend)

- Placeholder for layer-shell implementation
- Returns error with clear message about current limitations
- Designed for wlroots-based compositors (Sway, Hyprland)
- Future work: Implement actual layer-shell protocol

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

# In another shell (auto-detects X11 vs Wayland)
./target/debug/always-overlay

# Or through the main CLI, when both binaries are installed side by side
always overlay run
```

### Backend Selection

The overlay automatically detects the display server:
- **Wayland detected**: Attempts Wayland layer-shell backend
- **Wayland fails**: Falls back to X11 backend
- **X11 detected**: Uses X11 backend
- **No display server**: Returns error

### Platform Support

**X11**: Full production-quality overlay with always-on-top window. Compatible with GNOME (X11), KDE Plasma (X11), i3, Sway (X11), and other EWMH-compliant window managers.

**Wayland**: X11 backend works on XWayland (Wayland with X11 compatibility). Native Wayland layer-shell is a placeholder with clear error message. Designed for wlroots compositors (Sway, Hyprland). GNOME/KDE Wayland not yet supported - requires portal integration or notification-style overlay.

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

### Unit Tests

- **Backend selection tests**: Verify backend detection and type handling
- **Text truncation tests**: Verify text fitting with unicode support
- **State reducer tests**: Verify event parsing and state transitions (14 tests)

### Manual Testing

1. Start daemon: `always run`
2. Start overlay: `./target/debug/always-overlay`
3. Trigger voice activity and verify overlay appears as X11 window
4. Test pause/resume: `always toggle-pause`
5. Verify overlay shows state changes in X11 window
6. Verify reconnection on daemon restart

## Future Work

- **Wayland layer-shell**: Implement full layer-shell protocol for wlroots compositors
- **GNOME/KDE Wayland**: Add portal integration or notification-style overlay
- **Active monitor detection**: Position overlay on active monitor instead of primary
- **Focused window tracking**: Position overlay near focused window when possible
- **Animation system**: Dot-wave animation matching macOS
- **System icon theming**: Use system icon theme for better integration

## Platform Limitations

- **X11**: Full production-quality overlay with broad window manager compatibility
- **Wayland**: X11 backend works on XWayland. Native Wayland layer-shell is a placeholder with clear error message
- **GNOME/KDE Wayland**: Not yet supported - requires portal integration or notification-style overlay
- **wlroots compositors**: Layer-shell implementation is future work
- **If XOpenDisplay fails**: Verify the process has `DISPLAY` and `XAUTHORITY` for an X11 session
