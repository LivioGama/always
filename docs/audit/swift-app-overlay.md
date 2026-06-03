# Swift App Audit — scope: app-overlay

Files reviewed:
- Always/Sources/Always/Always.swift
- Always/Sources/Always/Views/StatusOverlay.swift
- Always/Sources/Always/Views/ListeningIndicator.swift
- Always/Sources/Always/Views/MenuBarView.swift
- Always/Sources/Always/Views/MenuBarStatusLabel.swift

Cross-referenced (callers/related, to confirm defects): Services/StatusOverlayController.swift, Services/StateMonitor.swift, Models/DaemonState.swift, Views/StatusTabView.swift, Package.swift, build.sh.

---

### [CRITICAL] Duplicate `StatusOverlayController` class — module fails to compile
- file: Always/Sources/Always/Views/StatusOverlay.swift:8 (and Always/Sources/Always/Services/StatusOverlayController.swift:8)
- category: crash-panic
- confidence: high
- impact: The SwiftPM target does not build — `invalid redeclaration of 'StatusOverlayController'`. Every overlay/menu-bar change is blocked; the bundled app cannot be produced.
- problem: Two `final class StatusOverlayController` are declared in the SAME module. `Package.swift` compiles the whole tree as one target (`path: "Sources/Always"`, no `exclude:`), so both files are in scope:
  - `Views/StatusOverlay.swift:8` → `final class StatusOverlayController {` (uses `NSWindow`, `DispatchWorkItem`)
  - `Services/StatusOverlayController.swift:8` → `final class StatusOverlayController {` (uses `NSPanel`, `Timer`)
  Both expose an identical `func update(for state: DaemonState)`, so even name-qualification can't disambiguate `AppDelegate`'s `StatusOverlayController()` call (Always.swift:84).
- fix: Keep exactly one. The `Services/` version (NSPanel + `.nonactivatingPanel`) is the better implementation for a non-activating menu-bar overlay; delete the duplicate `StatusOverlayController` from `Views/StatusOverlay.swift` (keep only `StatusOverlayViewModel` + `StatusOverlayView` there, which the service version references), or merge and remove one file. Verify with `swift build -c release` after the change.

### [CRITICAL] Stray unmatched `}` at top level in MenuBarStatusLabel.swift
- file: Always/Sources/Always/Views/MenuBarStatusLabel.swift:44
- category: crash-panic
- confidence: high
- impact: Hard syntax error — `extraneous '}' at top level`. The target will not compile.
- problem: The class closes at line 41; line 44 is a second, unmatched closing brace after a stray comment:
  ```swift
  40      }
  41  }
  42
  43  // MARK: - Always.swift moved to Always/Sources
  44  }      // <-- extraneous closing brace at file scope
  ```
- fix: Delete line 44 (and the dangling comment on 43 if unwanted). The file should end at the class's `}` on line 41.

### [HIGH] `onStateChange` is a single closure, silently overwritten — overlay stops updating
- file: Always/Sources/Always/Always.swift:93 (assigner) and Always/Sources/Always/Views/StatusTabView.swift:14 (overwriter), via StateMonitor.swift:10
- category: correctness
- confidence: high
- impact: The overlay's state subscription is destroyed whenever the Status tab view is constructed (every popover open / SwiftUI re-render). After that the overlay never updates again — exactly the "overlay disappeared / shown at wrong time" class of bug noted as recent churn. State updates also stop reaching StatusTabView since its handler body is empty.
- problem: `StateMonitor` exposes a SINGLE assignable handler:
  ```swift
  var onStateChange: ((DaemonState) -> Void)?   // StateMonitor.swift:10
  ```
  `AppDelegate.startMonitoring()` claims it to drive the overlay:
  ```swift
  monitor.onStateChange = { [weak self] state in
      DispatchQueue.main.async { self?.overlayController?.update(for: state) }   // Always.swift:93–97
  }
  ```
  but `StatusTabView.init` overwrites the very same property with an empty closure:
  ```swift
  stateMonitor?.onStateChange = { [weak self] state in
      // ...                                                    // StatusTabView.swift:14–16
  }
  ```
  Last-writer-wins; the overlay handler is clobbered. `MenuBarStatusLabel.observeState()` (MenuBarStatusLabel.swift:19) does the same if ever used.
- fix: Make `StateMonitor` a multicast publisher. Since it is already `ObservableObject` with `@Published private(set) var currentState`, have consumers observe `currentState` via Combine `.sink`/`.$currentState` (store the cancellable) or `onReceive`, and remove the shared mutable `onStateChange`. For the overlay, subscribe in `AppDelegate` with a retained `AnyCancellable`. Do NOT assign closures from a SwiftUI `View.init`.

### [HIGH] `onChange(of:_:)` two-parameter form requires macOS 14, target is macOS 13
- file: Always/Sources/Always/Views/ListeningIndicator.swift:36
- category: crash-panic
- confidence: high
- impact: Build error on the declared deployment target (`Package.swift` → `.macOS(.v13)`): the zero/two-parameter `onChange` is only available macOS 14+. If forced to build against a newer SDK without availability gating, it breaks on macOS 13 at runtime.
- problem:
  ```swift
  .onChange(of: state) { _, newState in    // ListeningIndicator.swift:36 — macOS 14 API
      animating = isActiveState(newState)
  }
  ```
  Package declares `platforms: [ .macOS(.v13) ]`.
- fix: Either raise the deployment target to `.macOS(.v14)` in Package.swift, or use the macOS 13-compatible single-parameter form:
  ```swift
  .onChange(of: state) { newState in
      animating = isActiveState(newState)
  }
  ```

### [MEDIUM] Overlay window positioned only once — wrong/offscreen on display change or non-main screen
- file: Always/Sources/Always/Views/StatusOverlay.swift:65 (and Services/StatusOverlayController.swift:56)
- category: correctness
- confidence: high
- impact: If the user disconnects/reconnects a monitor, changes resolution, or works on a non-main display, the overlay shows at a stale top-right coordinate — potentially off-screen or on the wrong monitor — appearing as "overlay missing".
- problem: `positionWindow`/`positionPanel` is called only inside `createWindow()`/`makePanel()`, which runs once (window/panel is cached). `show()`/`showPanel()` only call `orderFrontRegardless()` and never reposition:
  ```swift
  private func show() {
      ...
      if window == nil { createWindow() }
      window?.orderFrontRegardless()      // StatusOverlay.swift:33 — no reposition
  }
  ```
  Also `NSScreen.main` is "key window's screen", not necessarily where the user is looking.
- fix: Reposition on every `show()`/`showPanel()` (call `positionWindow(window)` before ordering front), and pick the screen under the mouse or the active screen rather than `NSScreen.main`. Optionally observe `NSApplication.didChangeScreenParametersNotification` to recompute.

### [MEDIUM] Global event monitor leaked / re-added on every popover open
- file: Always/Sources/Always/Always.swift:117
- category: resource-leak
- confidence: high
- impact: Each time the popover opens, a new global mouse-down monitor is installed and the previous one is never removed (it is only removed in `applicationWillTerminate`). Monitors accumulate for the process lifetime; stale monitors keep firing `performClose` and waste CPU on every click app-wide.
- problem:
  ```swift
  eventMonitor = NSEvent.addGlobalMonitorForEvents(...) { [weak self] _ in
      self?.popover?.performClose(nil)        // Always.swift:117 — overwrites prior handle, leaking it
  }
  ```
  The previous `eventMonitor` reference is overwritten without `NSEvent.removeMonitor`, and nothing removes it on `performClose`.
- fix: Remove the monitor whenever the popover closes (in `performClose` path / a `popoverDidClose` delegate or right before re-adding):
  ```swift
  if let m = eventMonitor { NSEvent.removeMonitor(m); eventMonitor = nil }
  ```
  Install it only while shown, and tear it down in the close branch of `togglePopover`.

### [LOW] `MenuBarStatusLabel` is dead code that would re-break the overlay if wired
- file: Always/Sources/Always/Views/MenuBarStatusLabel.swift:8
- category: maintainability
- confidence: high
- impact: The class is never instantiated anywhere (grep shows only its own definition). It silently hijacks the same single `onStateChange` slot (line 19), so the moment someone wires it up it will clobber the overlay subscription (same root cause as the HIGH finding). Dead, misleading, and a latent regression.
- problem: No call site constructs `MenuBarStatusLabel(...)`; it duplicates the menu-bar-icon-by-state logic that nothing uses, while competing for `onStateChange`.
- fix: Either delete the file, or fold its icon-by-state logic into `AppDelegate`/`StateMonitor`'s multicast observation. Do not leave an unused observer that mutates shared state.

### [LOW] Overlay window/panel and hosting controller retained for app lifetime (never torn down)
- file: Always/Sources/Always/Views/StatusOverlay.swift:29 / Services/StatusOverlayController.swift:28
- category: resource-leak
- confidence: medium
- impact: Minor steady-state memory; the hidden window/panel and its `NSHostingController` are kept alive after `orderOut`. Acceptable for a long-lived menu-bar app, but the overlay is reused via `orderOut` only — never released even after long idle.
- problem: Window/panel is created lazily and cached; `scheduleHide` only `orderOut(nil)`, never releasing. Combined with the single-instance pattern this is fine, but there is no path that ever frees it.
- fix: Acceptable to leave, or set `window = nil` / `panel = nil` after a longer idle in the hide work item if memory matters. Lower priority than the build-breakers.
