# Overlay Debugging Report - SESSION 2

## Previous Session Summary

Session 1 identified that the app was crashing on launch due to `StatusOverlayController.shared` creating an `NSWindow` at module load time (before app initialization). Fixed by implementing lazy window initialization.

**Status at end of Session 1**: App is now stable and runs without crashes.

## Current Session: Combine Subscription Not Firing

### Problem Statement

The overlay system has all pieces in place:
1. ✅ Daemon broadcasts events via UDS socket
2. ✅ UDSClient connects and receives events successfully  
3. ✅ StateMonitor receives events and updates @Published properties
4. ❌ **AppDelegate Combine subscription never fires** — no "State changed" logs appear

### Investigation Timeline

#### Attempt 1: Threading Issue (SOLVED)
- **Problem**: StateMonitor was updating @Published properties on background thread
- **Evidence**: NotificationCenter publisher delivers on arbitrary thread
- **Fix**: Wrapped all property updates in `DispatchQueue.main.async`
- **Result**: Still no subscription firing

#### Attempt 2: AppDelegate Lifecycle Issue (ROOT CAUSE FOUND)
- **Problem**: `applicationDidFinishLaunching` never called for MenuBarExtra-only apps
- **Evidence**: Zero "AppDelegate: Setting up overlay subscription" logs
- **Root Cause**: SwiftUI's MenuBarExtra apps don't reliably trigger AppDelegate lifecycle methods
- **Attempted Fix 1**: Move subscription to `App.init()` — **Failed** (NSApp is nil)
- **Attempted Fix 2**: Create OverlayManager singleton called from AppDelegate — **Failed** (AppDelegate still not called)
- **Attempted Fix 3**: Move subscription directly into StateMonitor — **IN PROGRESS**

### Current Architecture (Attempt 3)

```
StateMonitor.init()
  ├─ setupUDSEventListener() → receives daemon events
  └─ setupOverlaySubscription() → subscribes to own @Published properties
       └─ Publishers.CombineLatest4($isPaused, $isAutoEnter, $isTranscribing, $isVoiceActivity)
            └─ .sink { ... updateOverlay() }
```

**Key insight**: StateMonitor is initialized early (when UDSClient is created), so the subscription should fire before any state changes occur.

### Testing Strategy

**Verification command**:
```bash
pkill -9 -f AlwaysApp
/Users/livio/Documents/always/AlwaysApp/.build/debug/AlwaysApp > /tmp/alwaysapp.log 2>&1 &
sleep 3
/Users/livio/Documents/always/target/release/always toggle-pause
sleep 2
grep -E "(StateMonitor: State changed|SHOWING|HIDING)" /tmp/alwaysapp.log
```

**Expected output**: 
- "StateMonitor: State changed - pause:true ..." (from Combine subscription)
- "StateMonitor: SHOWING overlay with state: paused" (from updateOverlay)

**Current output**: Only daemon events logged, no subscription firing

---

## Code Changes This Session

### StateMonitor.swift
1. Added `setupOverlaySubscription()` method that subscribes to @Published properties
2. Moved property updates to main thread with `DispatchQueue.main.async`
3. Subscription stored in StateMonitor's own `cancellables` set

### AlwaysApp.swift  
1. Removed AppDelegate Combine subscription (was never called)
2. Removed OverlayManager singleton (redundant)
3. Simplified back to minimal structure

---

## Next Steps

1. **Test current implementation** (subscription in StateMonitor.init)
2. If still not firing: Add debug prints to verify @Published actually triggers Combine
3. Consider manual overlay triggering as fallback (call updateOverlay directly from handleDaemonEvent)

---
