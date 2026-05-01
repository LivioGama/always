# Implementation Plan: Fix Always Overlay Display System

## Problem Summary
The overlay system is broken across the entire stack. The Rust daemon never sends processing/transcribing events (it just prints to stderr via loading.rs). The Swift overlay only renders "Listening: ON/OFF" / "Auto-Enter: ON/OFF" text despite having a proper OverlayState enum. And the AppDelegate only subscribes to pause/autoEnter changes, ignoring processing/transcribing/voiceActivity states entirely.

## Fix Overview (4 changes, ordered by dependency)

### Change 1: Rust -- Add transcribing_started/transcribing_stopped to EventBroadcaster
**File:** `/Users/livio/Documents/always/src/always/event.rs`

The DaemonEvent enum already has TranscribingStarted and TranscribingStopped variants (lines 17-19), and there are already processing_started()/processing_stopped() convenience methods (lines 87-94). But there are no transcribing_started()/transcribing_stopped() methods.

**Action:** Add two methods to the EventBroadcaster impl block, after line 94 (after processing_stopped):

```rust
pub fn transcribing_started(&self) {
    self.send(DaemonEvent::TranscribingStarted);
}

pub fn transcribing_stopped(&self) {
    self.send(DaemonEvent::TranscribingStopped);
}
```

### Change 2: Rust -- Replace loading:: calls with event broadcasts in vad.rs
**File:** `/Users/livio/Documents/always/src/always/vad.rs`

Currently vad.rs imports loading (line 8) and calls its functions at 6 locations. Each call must be replaced with an event broadcast. The function record_with_local_vad does not receive an EventBroadcaster parameter -- it should use event::global_broadcaster() (same pattern as keyboard.rs and event_loop.rs).

**Action:** Replace the `use crate::always::loading;` import (line 8) with `use crate::always::event;`.

Then replace each loading:: call site:

| Line | Current code | Replacement | Semantic meaning |
|------|--------------|-------------|------------------|
| 97 | `loading::show_loading_overlay()` | `event::global_broadcaster().voice_activity_detected()` | Voice detected -- show voice activity indicator |
| 134 | `loading::stop_loading()` | `event::global_broadcaster().voice_activity_ended()` | No speech found after timeout -- clear indicator |
| 152 | `loading::stop_loading()` | `event::global_broadcaster().voice_activity_ended()` | Energy too low -- clear indicator |
| 168 | `loading::show_persistent_overlay()` | `event::global_broadcaster().transcribing_started()` | About to call Groq API -- show transcribing |
| 176 | `loading::stop_loading()` | `event::global_broadcaster().transcribing_stopped()` | WAV creation failed -- clear indicator |
| 189 | `loading::stop_loading()` | `event::global_broadcaster().transcribing_stopped()` | Transcription succeeded -- clear indicator |
| 196 | `loading::stop_loading()` | `event::global_broadcaster().transcribing_stopped()` | Transcription failed -- clear indicator |

**Note on line 97:** The original call is inside an `if let Err(e) = ...` block because `loading::show_loading_overlay()` returned Result. The broadcaster methods return nothing, so change the pattern to just call the method directly (no error handling needed). Same for line 168.

Also note: the existing commented-out code at lines 94-96 and 166-167 (the old `DaemonState::set_transcribing()` calls) can be removed since they are dead comments.

**Decision -- VoiceActivityDetected vs ProcessingStarted:** Looking at the semantic flow in vad.rs:
- Line 97: Voice is first detected (VAD onset) -- this maps to VoiceActivityDetected
- Lines 134, 152: Voice activity ended without useful speech -- VoiceActivityEnded
- Line 168: About to send audio to Groq for transcription -- TranscribingStarted
- Lines 176, 189, 196: Transcription complete or failed -- TranscribingStopped

The ProcessingStarted/ProcessingStopped events could be used in event_loop.rs around the filter::should_accept_with_reason and apply_vocabulary calls (the post-processing step), but looking at the actual timing, that processing is near-instant. For practical purposes, the user-visible states are: voice activity (VAD detected speech) and transcribing (API call). Processing can be omitted from the initial fix or added as a wrapper around the handle_speech function in event_loop.rs if desired.

**Recommended:** For now, skip ProcessingStarted/ProcessingStopped since the post-processing is <10ms. The visible states become: paused, autoEnter, transcribing, voiceActivity, hidden.

### Change 3: Rust -- Deprecate/gut loading.rs
**File:** `/Users/livio/Documents/always/src/always/loading.rs`

After Change 2, nothing calls into loading.rs. Two options:
- **Option A (clean):** Remove loading.rs entirely and remove `pub mod loading;` from mod.rs (line 13).
- **Option B (safe):** Add `#[deprecated]` attributes to all public functions and leave the module for now.

**Recommended:** Option A. The module is 72 lines of dead code that only prints to stderr. Remove the file and remove the `pub mod loading;` line from `/Users/livio/Documents/always/src/always/mod.rs` line 13.

### Change 4: Swift -- Rewrite StatusOverlayView to use OverlayState
**File:** `/Users/livio/Documents/always/AlwaysApp/Sources/AlwaysApp/Views/StatusOverlay.swift`

The OverlayState enum (lines 3-29) is already perfect -- it has all 5 states with correct colors and icons. The problem is StatusOverlayView (lines 31-87) ignores it entirely.

**Rewrite StatusOverlayView:** Replace the listening/autoEnter Bool properties with a single OverlayState property. The draw method should render a colored circle + state label, matching the README spec.

**New StatusOverlayView:**
- Single property: `var state: OverlayState`
- Draw a filled circle using `state.color`, ~20pt diameter
- Draw the state label (`state.rawValue`) next to it in white/system text
- Use the SF Symbol from `state.iconName` instead of a plain circle if desired
- Window size can shrink from 200x70 to approximately 160x40

**Rewrite StatusOverlayWindow.show():** Change the signature from `show(listening:autoEnter:duration:)` to `show(state:)`. Remove the duration parameter entirely -- the overlay should persist until explicitly hidden (no auto-hide timer).

```swift
func show(state: OverlayState) {
    DispatchQueue.main.async { [weak self] in
        guard let self = self else { return }
        self.hideTimer?.invalidate()  // Remove timer logic entirely
        
        if self.overlayView == nil {
            self.overlayView = StatusOverlayView(frame: ...)
            self.contentView = self.overlayView
            self.positionAtBottomMiddle()
        }
        
        self.overlayView?.state = state
        if !self.isVisible { self.orderFrontRegardless() }
    }
}
```

**Update StatusOverlayController:** Change `show(listening:autoEnter:duration:)` to `show(state:)`.

### Change 5: Swift -- Rewrite AppDelegate overlay logic
**File:** `/Users/livio/Documents/always/AlwaysApp/Sources/AlwaysApp/AlwaysApp.swift`

The current AppDelegate (lines 64-92) only subscribes to `$isPaused` and `$isAutoEnter`. It needs to subscribe to ALL relevant states and compute the highest-priority visible state.

**Priority order (highest first):**
1. `isPaused` -> `OverlayState.paused` (orange) -- persistent, highest priority
2. `isTranscribing` -> `OverlayState.transcribing` (purple) -- transient, during API call
3. `isVoiceActivity` -> `OverlayState.voiceActivity` (red) -- transient, during speech
4. `isAutoEnter` -> `OverlayState.autoEnter` (green) -- persistent toggle
5. None active -> hide overlay

**Implementation approach:** Use `Publishers.CombineLatest` (or CombineLatest4/CombineLatest5 from multiple publishers) to merge all state signals into one stream, then compute the resolved `OverlayState?` and show/hide accordingly.

**Replace lines 57-93 with:**

```swift
DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) { [weak self] in
    guard let self = self, let monitor = self.stateMonitor else { return }
    
    // Combine all state publishers into one overlay state
    Publishers.CombineLatest4(
        monitor.$isPaused,
        monitor.$isAutoEnter,
        monitor.$isTranscribing,
        monitor.$isVoiceActivity
    )
    .sink { [weak self] isPaused, isAutoEnter, isTranscribing, isVoiceActivity in
        let resolvedState: OverlayState? = {
            if isPaused { return .paused }
            if isTranscribing { return .transcribing }
            if isVoiceActivity { return .voiceActivity }
            if isAutoEnter { return .autoEnter }
            return nil
        }()
        
        if let state = resolvedState {
            StatusOverlayController.shared.show(state: state)
        } else {
            StatusOverlayController.shared.hide()
        }
    }
    .store(in: &self.cancellables)
}
```

**Important design decision -- autoEnter persistence:** The autoEnter state is a toggle that stays on. If it always shows the overlay, that could be annoying. Two options:
- **Option A:** Show autoEnter persistently (per README spec). The green circle is always visible when auto-enter is on. This matches the README but may be intrusive.
- **Option B:** Only show autoEnter on transition (for 2 seconds), then hide. Paused stays persistent; transient states (transcribing, voiceActivity) show/hide with activity.

**Recommendation:** Option A for initial implementation (matches spec). If the user finds it too intrusive, the fix is easy -- move the `if isAutoEnter` check after `return nil` or add a short timer for it only. But note: if autoEnter is always on, the overlay would always be visible (green), which would mask the transient transcribing/voiceActivity indicators since they have lower priority.

**Revised recommendation:** Re-order priority so transient activity states take precedence over the persistent autoEnter toggle (as shown in the code above).

This way, when auto-enter is on AND the user is speaking, they see the purple/red transient indicator, and when idle they see the green auto-enter indicator. When paused, they always see orange regardless of other states.

### Change 6 (optional): Clean up unused properties in AppDelegate
**File:** `/Users/livio/Documents/always/AlwaysApp/Sources/AlwaysApp/AlwaysApp.swift`

Lines 29-34 define tracking variables (wasPaused, wasAutoEnter, etc.) that were used for transition detection. With the CombineLatest approach, these are no longer needed. Remove them.

## Sequencing
- **Change 1** (event.rs: add transcribing methods) -- no dependencies
- **Change 2** (vad.rs: replace loading calls) -- depends on Change 1
- **Change 3** (remove loading.rs) -- depends on Change 2
- **Change 4** (StatusOverlay.swift rewrite) -- independent of Rust changes
- **Change 5** (AlwaysApp.swift rewrite) -- depends on Change 4
- **Change 6** (cleanup) -- depends on Change 5

**Changes 1-3 (Rust side) and Changes 4-5 (Swift side) can be done in parallel** since they're different codebases, but should be deployed together.

## Testing Strategy

### Rust side
After changes 1-3, run the daemon with `RUST_LOG=debug` and connect to `/tmp/always.sock` with socat or nc to verify events are sent:
- Speak into mic -> should see VoiceActivityDetected then TranscribingStarted then TranscribingStopped then TranscriptFinal
- Low energy speech -> should see VoiceActivityDetected then VoiceActivityEnded (no transcribing events)

### Swift side
Build and run the app. Verify:
- Toggle pause (Ctrl+Shift+P) -> orange circle appears/disappears
- Toggle auto-enter (Ctrl+Shift+A) -> green circle appears/disappears
- Speak -> purple circle during transcription, then back to idle or auto-enter state
- No 2-second auto-hide -- overlay persists while state is active

### Edge cases
- Rapid state transitions (speak while paused -> nothing should show except orange)
- Disconnect/reconnect UDS -> initial state should be sent correctly (already handled in uds_server.rs lines 51-67, but will need to also send current processing/transcribing state if active during reconnect)

## Potential Issue: UDS Initial State
In `/Users/livio/Documents/always/src/always/uds_server.rs` lines 51-67, when a new client connects, the server sends the current pause and auto-enter state. It does NOT send the current processing/transcribing state. If the Swift app reconnects mid-transcription, it would miss the TranscribingStarted event.

**Mitigation:** Add a global AtomicBool for is_transcribing state in vad.rs (or in the event module), and send the current transcribing state in the initial connection events. This is a minor enhancement and can be deferred if the reconnect case is rare enough.

## Critical Files for Implementation
- `/Users/livio/Documents/always/src/always/vad.rs` -- Replace all loading:: calls with event broadcasts
- `/Users/livio/Documents/always/src/always/event.rs` -- Add transcribing_started/transcribing_stopped methods
- `/Users/livio/Documents/always/AlwaysApp/Sources/AlwaysApp/Views/StatusOverlay.swift` -- Rewrite view to use OverlayState enum with colored circle, remove auto-hide timer
- `/Users/livio/Documents/always/AlwaysApp/Sources/AlwaysApp/AlwaysApp.swift` -- Subscribe to all state publishers via CombineLatest, compute priority-based overlay state
- `/Users/livio/Documents/always/src/always/loading.rs` -- Delete entirely (dead code after vad.rs fix)
