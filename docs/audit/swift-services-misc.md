# Swift Audit — services-misc

Scope: ModelManagerClient.swift, UpdateService.swift, PermissionsManager.swift, FocusedAppMonitor.swift, AudioOutputMonitor.swift

Method: all five files read in full. Call sites confirmed via grep + reading `Always.swift` and `StateMonitor.swift`. Confirmed lifecycle facts that shaped severities:
- `AudioOutputMonitor.shared.start(...)` and `FocusedAppMonitor.shared.start(...)` are each called **exactly once** at launch (`Always.swift:158-159`, inside a `Task { @MainActor }` in `applicationDidFinishLaunching`). They are NOT re-called on reconnect, so duplicate-`start` leak risk is latent, not live (downgraded accordingly).
- On (re)connect, `StateMonitor.setupConnectionMonitoring` calls `FocusedAppMonitor.shared.resyncCurrentAppToDaemon()` (`StateMonitor.swift:84`) but deliberately does **NOT** re-push audio state (`StateMonitor.swift:85-86`, comment: "re-pushing on connect races focus resync"). So `AudioOutputMonitor.resyncToDaemon()` has **no caller in the connect path** — the audio-state staleness from a default-device change is therefore never repaired even on reconnect, which strengthens the HIGH finding below.
- `PermissionsManager.requestAccessibilityIfNeeded()` callers confirmed: `Always.swift:148` (main) and `PermissionsBanner.handleAction` (SwiftUI button, main). No off-main caller today → concurrency finding kept MEDIUM.

---

### [HIGH] AudioOutputMonitor binds to the output device at start() and never re-subscribes when the default output device changes
- file: Always/Sources/Always/Services/AudioOutputMonitor.swift:26-36, 64-84, 86-91
- category: correctness
- confidence: high
- impact: After the user changes the default output device (plug in headphones, connect AirPods/Bluetooth, switch to an external DAC, unplug) the listener still watches the device captured at launch. `kAudioDevicePropertyDeviceIsRunningSomewhere` on the stale device no longer reflects what is actually playing, so the daemon's "is system audio playing" state goes permanently wrong and the audio-based auto-pause silently breaks until app restart. This is a normal, frequent user action.
- problem: `start()` captures the device once and `installListener` hard-binds it into the callback; nothing listens for `kAudioHardwarePropertyDefaultOutputDevice` changes:
```swift
self.deviceID = device
installListener(on: device)
...
let block: AudioObjectPropertyListenerBlock = { [weak self] _, _ in
    guard let self = self else { return }
    let playing = self.isRunningSomewhere(device: device)   // 'device' frozen at start()
    self.notify(playing: playing)
}
let status = AudioObjectAddPropertyListenerBlock(device, &addr, DispatchQueue.global(qos: .utility), block)
```
`resyncToDaemon()` does re-read `defaultOutputDevice()` for a one-shot push, but (a) it never re-installs the listener on the new device, and (b) `StateMonitor` deliberately does not call it on reconnect (`StateMonitor.swift:85-86`) — so there is no path at all that repairs the stale binding after a device switch. Live change notifications keep coming only from the dead device until the app is restarted.
- fix: Install a second listener on `kAudioHardwarePropertyDefaultOutputDevice` (system object). On change: `AudioObjectRemovePropertyListenerBlock(oldDevice, &addr, queue, savedBlock)`, update `self.deviceID`, re-`installListener` on the new device, and push the new running state. Store the block in a property so it can be removed.

---

### [MEDIUM] AudioOutputMonitor never removes its CoreAudio property listener (no stop/deinit)
- file: Always/Sources/Always/Services/AudioOutputMonitor.swift:64-84
- category: resource-leak
- confidence: high
- impact: The installed `AudioObjectPropertyListenerBlock` is never removed — there is no `stop()`, no `deinit`, and no `AudioObjectRemovePropertyListenerBlock` anywhere. For a process-lifetime singleton this leaks one listener for the life of the app (minor on its own), but it also blocks any correct re-subscription strategy needed to fix the stale-device bug above, and the captured block retains nothing it shouldn't, so the leak is bounded.
- problem: No teardown path:
```swift
let status = AudioObjectAddPropertyListenerBlock(device, &addr, DispatchQueue.global(qos: .utility), block)
if status == noErr {
    listenerInstalled = true
    ...
}
// no matching AudioObjectRemovePropertyListenerBlock anywhere
```
Also the block is not retained in a property, so even an attempted removal later would have no handle to pass.
- fix: Save the block (`private var listenerBlock: AudioObjectPropertyListenerBlock?`) and add `stop()`/`deinit` that calls `AudioObjectRemovePropertyListenerBlock(deviceID, &addr, queue, block)`.

---

### [MEDIUM] PermissionsManager `didBecomeActiveNotification` observer + pollTimer have no teardown (no deinit)
- file: Always/Sources/Always/Services/PermissionsManager.swift:119-141
- category: resource-leak
- confidence: high
- impact: The `self`-target observer registered via `addObserver(self, selector:...)` is never removed (no `deinit`). For a singleton this is benign in steady state, but it is a latent crash pattern: NotificationCenter holds the raw `self` target; if the singleton is ever deallocated, the center messages a freed object → crash. It is also inconsistent with the other services in this scope, all of which store removable tokens. The early-launch `pollTimer` is invalidated by the 30s timeout and the both-granted check, so it is handled — only the notification observer leaks.
- problem: No `deinit`, and a target/selector observer that is never removed:
```swift
NotificationCenter.default.addObserver(
    self,
    selector: #selector(handleAppActivate),
    name: NSApplication.didBecomeActiveNotification,
    object: nil
)
```
(`ModelManagerClient` by contrast stores its token and removes it in `deinit`.)
- fix: Add `deinit { NotificationCenter.default.removeObserver(self); pollTimer?.invalidate() }`. Prefer the block-based `addObserver(forName:object:queue:)` storing the returned token to match the rest of the codebase.

---

### [MEDIUM] PermissionsManager.requestAccessibilityIfNeeded() mutates @Published off any caller thread without a main-thread hop
- file: Always/Sources/Always/Services/PermissionsManager.swift:85-96
- category: concurrency
- confidence: medium
- impact: `PermissionsManager` is a plain `ObservableObject` (not `@MainActor`). `refresh()` carefully hops to main for its writes, but `requestAccessibilityIfNeeded()` writes the `@Published accessibilityStatus` directly on whatever thread the caller is on. If invoked off-main (e.g. from a background `Task`), this is an unsynchronized publish that triggers SwiftUI's "Publishing changes from background threads is not allowed" runtime warning and can corrupt view state. AX APIs themselves are also expected on the main thread.
- problem:
```swift
@discardableResult
func requestAccessibilityIfNeeded() -> Bool {
    if AXIsProcessTrusted() {
        accessibilityStatus = .trusted        // direct @Published write, no main hop
        return true
    }
    ...
    let trusted = AXIsProcessTrustedWithOptions(options)
    accessibilityStatus = trusted ? .trusted : .notTrusted   // direct @Published write
    return trusted
}
```
The current sole caller is `PermissionsBanner.handleAction` (a SwiftUI button, on main) so today it is safe, but the method is `public` API with no isolation guarantee.
- fix: Mark the class `@MainActor` (it is UI-facing and a singleton), or wrap the `accessibilityStatus` assignments in `DispatchQueue.main.async`. `@MainActor` is cleanest since the AX calls want the main thread anyway.

---

### [MEDIUM] ModelManagerClient.download() shows an optimistic 0% that can stick forever if the command is dropped
- file: Always/Sources/Always/Services/ModelManagerClient.swift:115-119
- category: error-handling
- confidence: medium
- impact: `download()` immediately sets `downloadProgress[modelId] = 0`, which the UI renders as "downloading 0%". Recovery only happens via an inbound `ModelDownloadProgress/Complete/Cancelled/Failed` event. If the `DownloadModel` command is lost (UDS reconnect race, daemon mid-restart) no event ever arrives and the row is stuck at 0% indefinitely — no error, no spinner, no retry — and the user cannot tell it is dead.
- problem:
```swift
func download(_ modelId: String) {
    errors.removeValue(forKey: modelId)
    downloadProgress[modelId] = 0
    StateMonitor.shared.sendCommandWithData("DownloadModel", ["model_id": modelId])
}
```
No watchdog ties the optimistic 0% to a real daemon acknowledgement.
- fix: Either don't render 0% until the first real progress event (use a separate "requested" set shown as an indeterminate spinner), or start a per-id timeout that surfaces a retryable error if no progress arrives within N seconds.

---

### [LOW] ModelManagerClient: terminal events with a nil model_id leave the optimistic 0% entry stuck
- file: Always/Sources/Always/Services/ModelManagerClient.swift:148-170
- category: correctness
- confidence: low
- impact: Every terminal handler is gated on `if let id = event.modelId?.model_id`. A malformed completion/cancel event without an id falls through silently, so the optimistic `downloadProgress[id] = 0` set by `download()` is never cleared and the row stays at 0%. Edge case, depends on daemon emitting a malformed event.
- problem:
```swift
case .modelDownloadComplete:
    if let id = event.modelId?.model_id {
        downloadProgress.removeValue(forKey: id)
        ...
    }
    // else: nothing reconciled
```
- fix: In the `else` branch, log a warning and call `requestModelsList()` so the UI reconciles to ground truth.

---

### [LOW] UpdateService is not @MainActor yet constructs Sparkle UI machinery and publishes UI state in init
- file: Always/Sources/Always/Services/UpdateService.swift:19, 29-59, 62-68
- category: concurrency
- confidence: low
- impact: `SPUUpdater.start()`, `checkForUpdates()`, and `SPUStandardUserDriver` are main-thread APIs. `UpdateService` is a plain `ObservableObject`; if `shared` is first touched off the main thread, `init` runs the Sparkle bring-up and publishes `canCheckForUpdates` off-main. The only confirmed caller is `MenuBarView` (`@ObservedObject ... = UpdateService.shared`, main thread), so this is low-severity today but unguarded.
- problem: A class that builds Sparkle UI machinery and drives a SwiftUI `@Published` carries no actor isolation:
```swift
final class UpdateService: ObservableObject {
    static let shared = UpdateService()
    ...
    private init() {
        ...
        try updater.start()            // main-thread Sparkle API, no isolation
        self.canCheckForUpdates = updater.canCheckForUpdates
    }
```
- fix: Annotate `UpdateService` with `@MainActor`.

---

### [LOW] FocusedAppMonitor / AudioOutputMonitor: start() not idempotent (duplicate observer/listener if ever re-started)
- file: Always/Sources/Always/Services/FocusedAppMonitor.swift:44-61; Always/Sources/Always/Services/AudioOutputMonitor.swift:26-36
- category: resource-leak
- confidence: medium
- impact: Currently `start()` is called exactly once (`Always.swift:88-89`), so no live bug. But neither method guards re-entry: a second `start()` (e.g. a future daemon re-bootstrap that re-runs `wireMonitors`) would add a second `NSWorkspace.didActivateApplicationNotification` observer / a second CoreAudio listener while the first is never removed → duplicate `NotifyFocusedAppChanged` / `NotifySystemAudioState` commands and a leaked observer. Flagged as a latent hazard because the reconnect/resync architecture makes a future re-start plausible.
- problem: `FocusedAppMonitor.start()` overwrites `observer` without removing the prior one; `AudioOutputMonitor.start()` calls `installListener` with only a `listenerInstalled` flag that is set-after but never checked-before.
- fix: Guard each: `FocusedAppMonitor` — remove existing `observer` before re-adding (or `guard observer == nil`); `AudioOutputMonitor` — `guard !listenerInstalled else { return }` in `installListener` (reconciling with the default-device re-subscribe fix above).

---

### [LOW] UpdateService: redundant double-assignment of canCheckForUpdates
- file: Always/Sources/Always/Services/UpdateService.swift:55-58
- category: maintainability
- confidence: low
- impact: Cosmetic / minor. Line 55 manually reads the value, then `assign(to: &$canCheckForUpdates)` immediately re-emits the current value, so the manual line is redundant. The publisher subscription is a process-lifetime singleton (acceptable, no cancellation needed).
- problem:
```swift
self.canCheckForUpdates = updater.canCheckForUpdates
updater.publisher(for: \.canCheckForUpdates)
    .receive(on: DispatchQueue.main)
    .assign(to: &$canCheckForUpdates)
```
- fix: Drop line 55 and rely solely on the publisher (which back-fills the current value on subscribe).
