# Audit: swift-views-settings

Scope: Always/Sources/Always/Views/{SettingsWindow,CorrectionDialog,OnboardingView}.swift and Always/Sources/Always/Views/Settings/*.swift. All 15 files read in full; Config.swift, ModelInfo.swift, and the public signatures of CLIService/ModelManagerClient/SetupValidation were cross-checked to confirm call-site contracts.

---

### [HIGH] Clearing the Groq API key field can never persist the deletion, so a deleted key silently comes back
- file: Always/Sources/Always/Views/SettingsWindow.swift:173-175, :198-200; Always/Sources/Always/Models/SetupValidation.swift:18-19
- category: correctness
- confidence: high
- impact: A user who wants to remove their stored Groq key (e.g. to stop sending audio to a remote service) deletes the field text and saves — but the deletion is never written. The old key remains in daemon config and keeps being used. This is both a privacy and a "setting won't stick" defect.
- problem: Both save paths gate persistence on `shouldPersistApiKey(apiKey)`:
  ```swift
  if shouldPersistApiKey(apiKey) {
      _ = try await cliService.setConfig(key: "groq_api_key", value: apiKey.isEmpty ? "" : apiKey)
  }
  ```
  and `shouldPersistApiKey` is:
  ```swift
  func shouldPersistApiKey(_ apiKey: String) -> Bool {
      !apiKey.isEmpty && !apiKey.allSatisfy { $0 == "•" }
  }
  ```
  When `apiKey == ""` the guard is `false`, so the `setConfig("groq_api_key", "")` branch is unreachable — the `apiKey.isEmpty ? "" : apiKey` ternary's `""` arm is dead code. There is no way through the UI to clear the stored key. The user believes they removed it; the daemon keeps the previous value.
- fix: Distinguish "user typed a new key" from "user explicitly cleared." Persist a real key only via `saveApiKey()`, and add an explicit clear action (or detect a transition from masked → empty) that calls `setConfig("groq_api_key", "")`. Do not silently no-op an intentional clear.

---

### [HIGH] API key persistence is entangled with every Behavior autosave via the bullet-mask, leaking key length and re-saving on unrelated edits
- file: Always/Sources/Always/Views/SettingsWindow.swift:157-161, :185-200; Always/Sources/Always/Views/Settings/ModelsPanel.swift:39-41
- category: correctness
- confidence: medium
- impact: The masked-key handling is fragile: the mask length equals the real key length (length leak), and every unrelated setting change runs the full `saveConfig()` which re-evaluates the key branch. A user editing the silence timeout while the field still shows the mask is one logic slip away from writing the mask string as the real key.
- problem: On load the key becomes a same-length bullet run:
  ```swift
  if let key = config.groqApiKey, !key.isEmpty {
      apiKey = String(repeating: "•", count: key.count)   // leaks key length
  }
  ```
  `ModelsPanel.onChange(of: apiKey)` then mirrors that masked string into `config.groqApiKey` (`config.groqApiKey = newValue.isEmpty ? nil : newValue`, ModelsPanel.swift:40) — so `config` now holds the mask, not the real key. The only thing preventing the mask from being persisted is the `allSatisfy { $0 == "•" }` check; any future edit (e.g. user types one real char making it "abc•••…") defeats the all-bullet test and would persist a corrupted mixed string. The API-key write also lives inside `saveConfig()` (:198-200), which fires on every one of the nine `.onChange` handlers (:107-115), coupling unrelated settings to key persistence.
- fix: Remove the API-key write from `saveConfig()` entirely — persist the key only from the explicit `saveApiKey()`. Use a fixed-length placeholder (`"••••••••"`) instead of a key-length-matching mask, and track a `hasUnmaskedEdit` flag so the mirrored `config.groqApiKey` is only updated when the user actually replaces the mask.

---

### [HIGH] `loadConfig()` overwrites the user's in-progress typed API key on every Settings re-open / async completion
- file: Always/Sources/Always/Views/SettingsWindow.swift:152-166, :98-106
- category: correctness
- confidence: medium
- impact: If `getConfig()` resolves while the user is mid-typing in the Groq key field (or the panel re-appears), the field is reset to the bullet mask, losing the partially-typed key with no warning.
- problem: `onAppear` (:103-105) fires `Task { await loadConfig() }` unconditionally each time the view appears. `loadConfig` always reassigns `apiKey` (:157-161) and `config` (:155). There is no guard for "the user already has unsaved edits in the field." Because `apiKey` drives both the `SecureField` (ModelsPanel) and the onboarding/save logic, an async config load landing after the user starts typing silently discards their input. The masking compounds this: after the first save+reload the real key becomes bullets, so the displayed length leaks the key length and the field no longer holds a usable value.
- fix: Only populate `apiKey` from `loadConfig` on first load (track `private @State var didLoadConfig = false`), and never reassign `apiKey` if the field currently differs from the last-known mask. Also avoid masking with a same-length bullet run (length leak) — use a fixed placeholder like `"••••••••"` regardless of key length.

---

### [MEDIUM] Onboarding force-unwraps `apiKey.data(using: .utf8)!` — crash on rare encodings, and persists keys other code reads from a different Keychain service
- file: Always/Sources/Always/Views/OnboardingView.swift:324
- category: crash-panic
- confidence: high
- impact: `completeSetup()` can crash; and even when it succeeds the key is written under service `"com.always.daemon"` (:25) while the daemon/Settings round-trip uses `cliService.setConfig(key: "groq_api_key", ...)` (config file). The two stores can diverge, so a key entered in onboarding may not be the key the running daemon uses.
- problem:
  ```swift
  kSecValueData as String: apiKey.data(using: .utf8)!   // line 324
  ```
  `String.data(using: .utf8)` returns `String?`. For normal ASCII/UTF-8 keys it is non-nil, but force-unwrap is a crash primitive in a "Done" button that gates the whole app. Separately, onboarding writes only to Keychain (`SecItemAdd`, service `com.always.daemon`) and never calls `cliService.setConfig`, while `SettingsWindow.saveApiKey` writes via `cliService.setConfig("groq_api_key", …)` to the daemon config. Nothing in this file syncs the two.
- fix: Replace the force-unwrap with a guarded conversion and surface an error instead of crashing:
  ```swift
  guard let data = apiKey.data(using: .utf8) else {
      saveError = "Invalid API key encoding"; isSaving = false; return
  }
  ```
  And route onboarding's key through the same persistence path the daemon reads (`cliService.setConfig`) — or document/verify the daemon actually reads `com.always.daemon` Keychain so the two stores cannot diverge.

---

### [MEDIUM] Onboarding key validation marks a *valid* key invalid on any non-200 (rate-limit, 5xx, transient)
- file: Always/Sources/Always/Views/OnboardingView.swift:263-273; Always/Sources/Always/Models/SetupValidation.swift:8-12
- category: correctness
- confidence: high
- impact: A correct Groq key returns 429 (rate limited) or 500/503 (Groq outage) and is reported as "Invalid API key - Groq rejected the credentials" with a red X, blocking onboarding (the Done button requires `keyValid == true`, :28-33). The user cannot finish setup through no fault of their key.
- problem: `groqKeyValidationResult(statusCode:)` is binary — only 200 is valid, *everything else* is `.invalid`:
  ```swift
  func groqKeyValidationResult(statusCode: Int?) -> GroqKeyValidationResult {
      statusCode == 200 ? .valid
                        : .invalid("Invalid API key - Groq rejected the credentials")
  }
  ```
  So a 429/500/503/nil-status all surface the same "rejected the credentials" message even though the key may be perfectly valid. `validateGroqKey` then renders the red xmark (:91-93) and `keyValid = false` (:268) gates Done off.
- fix: Treat only auth failures (401/403) as invalid; treat other non-200 statuses as indeterminate (leave `keyValid = nil`) with a retry hint, and a `nil` status (no HTTPURLResponse) likewise. E.g. add a third state to `GroqKeyValidationResult` (`.unknown`) or branch on the status code:
  ```swift
  switch (response as? HTTPURLResponse)?.statusCode {
  case 200:        keyValid = true
  case 401, 403:   keyValid = false; saveError = "Groq rejected the credentials"
  default:         keyValid = nil;  saveError = "Couldn't verify the key — check your connection and retry."
  }
  ```

---

### [MEDIUM] `NumericSettingRow` clamps to range only on change — accepts and persists out-of-range values typed via the formatter, and the clamp can fight the formatter
- file: Always/Sources/Always/Views/Settings/NumericSettingRow.swift:50-58
- category: correctness
- confidence: medium
- impact: A user can type a value the `NumberFormatter` accepts but that exceeds the `range` and have it momentarily persisted via SettingsWindow's `onChange` autosave before the clamp re-fires; for `idlePauseSecs` (range `0...86_400`) the int formatter's max is `60_000` (SettingsWindow.swift:73-79), so values `60_001…86_400` are rejected by the formatter even though the row's declared range allows them.
- problem: The clamp lives in `.onChange(of: value)` (:54-58), which runs *after* the binding already mutated `value`. In `SettingsWindow`, the same `value` is observed by `.onChange(of: config.idlePauseSecs) { saveConfig() }` (:114), so an out-of-range intermediate can be written to the daemon before the clamp corrects it (then a second save fires). Additionally `SettingsWindow.intFormatter.maximum = 60_000` (:77) contradicts `range: 0...86_400` passed for "Pause After Inactivity" (BehaviorPanel.swift:179) — the formatter caps the field below the legal max, so the documented "up to 24h (86400s)" is unreachable through the UI.
- fix: Validate before committing (use a `set:` closure that clamps), and align the formatter max with the widest range that uses it (raise `intFormatter.maximum` to `86_400`, or give the idle row its own formatter). E.g.:
  ```swift
  value: Binding(get: { config.idlePauseSecs },
                 set: { config.idlePauseSecs = min(max($0, 0), 86_400) })
  ```

---

### [MEDIUM] `KeyCaptureButton` cannot record shortcuts whose final key is a non-character (arrows, F-keys, space) and clears nothing on cancel
- file: Always/Sources/Always/Views/Settings/KeyCaptureButton.swift:62-71
- category: correctness
- confidence: high
- impact: Users trying to bind e.g. `ctrl+alt+space`, `cmd+shift+F1`, or arrow-key combos get silently ignored — the recorder swallows the keydown (`return nil`) and stops recording with no feedback and no saved value.
- problem:
  ```swift
  let keyChar = event.charactersIgnoringModifiers?.lowercased() ?? ""
  if !keyChar.isEmpty, keyChar.count == 1, !parts.isEmpty {
      ...save...
  }
  stopRecording()
  return nil
  ```
  Space yields `" "` (count 1 but visually empty), function/arrow keys yield multi-scalar or empty `charactersIgnoringModifiers`, so `keyChar.count == 1` fails and nothing is bound. Because `return nil` always consumes the event and `stopRecording()` always runs, the user sees the field flip back to the old shortcut with no error. There is also no way to recover focus if they press Esc — it's just treated as an unrecordable key.
- fix: Use `event.keyCode` mapping for special keys (space/arrows/function/return), accept them explicitly, and only abort on Esc (keyCode 53) without saving. Give visual feedback ("Unsupported key — try another") when a press can't be mapped instead of silently reverting.

---

### [MEDIUM] `KeyCaptureButton` local event monitor leaks if the view disappears mid-recording
- file: Always/Sources/Always/Views/Settings/KeyCaptureButton.swift:24, 51-77
- category: resource-leak
- confidence: high
- impact: If the user clicks "Press keys…" then closes the Settings window (or switches panels — the panel view is rebuilt by `SettingsWindow.panelContent`), the `NSEvent` local monitor installed at :54 is never removed, leaking a global keystroke interceptor that swallows the next keydown app-wide and persists past the view's lifetime.
- problem: `monitor` is stored in `@State` and removed only in `stopRecording()` (:74-77), which is called from the event callback or `toggleRecording`. There is no `.onDisappear` to tear it down. SwiftUI can deallocate the view while `isRecording == true`, orphaning the monitor.
- fix: Add cleanup on disappear:
  ```swift
  .onDisappear { stopRecording() }
  ```
  (`stopRecording` already nil-checks the monitor, so it's safe.)

---

### [MEDIUM] `presetBinding` setter ignores the "Custom" tag, so selecting custom is a dead control / segmented Picker out-of-range risk
- file: Always/Sources/Always/Views/Settings/BehaviorPanel.swift:31-41, 97-107
- category: correctness
- confidence: medium
- impact: The segmented sensitivity Picker conditionally adds a "Custom" segment only when `currentPreset == nil` (:35-37). When the user has a real preset selected and the Picker has 3 segments, but then thresholds drift to custom (via Advanced rows), the bound selection becomes `nil` while the option set changes shape between renders — a class of segmented-Picker selection/index churn that on macOS 26 has produced the same `Picker`/array-indexing crashes the project already hit on the Models tab.
- problem: The setter:
  ```swift
  set: { newValue in
      guard let preset = newValue else { return }   // Custom (nil) is a no-op
      ...
  }
  ```
  "Custom" is selectable in the UI but tapping/selecting it does nothing (guard returns). More importantly the option set is dynamic: the `Custom` `Text(...).tag(Optional<SensitivityPreset>.none)` only exists while `currentPreset == nil`. A `.segmented` Picker whose number of tags changes between the value being `nil` and non-`nil` is exactly the unstable-selection pattern that caused the prior macOS 26 crash. The selection (`nil`) can transiently reference a tag that no longer exists after thresholds snap back to a preset.
- fix: Always include the "Custom" segment (disabled/greyed) so the tag set is stable, and make custom non-selectable via styling rather than removing the segment:
  ```swift
  ForEach(SensitivityPreset.allCases) { ... }
  Text("Custom").tag(Optional<SensitivityPreset>.none)   // always present
  ```
  Keep the guard in the setter, but a stable tag set prevents the index churn.

---

### [LOW] `saveApiKey()` has a hardcoded 1-second artificial delay shipped to production
- file: Always/Sources/Always/Views/SettingsWindow.swift:176-177
- category: ux
- confidence: high
- impact: Every save of the Groq key blocks the spinner for a full second for no functional reason, making the UI feel sluggish.
- problem:
  ```swift
  // Add a small delay to make the loader visible for testing
  try await Task.sleep(nanoseconds: 1_000_000_000) // 1 second
  ```
  This is debug scaffolding ("for testing") left in the release path.
- fix: Remove the `Task.sleep`. If a minimum spinner duration is desired for perceived responsiveness, gate it behind `#if DEBUG`.

---

### [LOW] AppVersion hardcoded in AboutPanel — drifts from the real bundle version
- file: Always/Sources/Always/Views/Settings/AboutPanel.swift:5, 19
- category: maintainability
- confidence: high
- impact: The About tab shows `Version 0.13.0` regardless of the actual shipped build; after any release bump this is wrong, misleading users and bug reports.
- problem:
  ```swift
  let appVersion = "0.13.0"
  ...
  Text("Version \(appVersion)")
  ```
  Hardcoded string instead of reading `CFBundleShortVersionString`.
- fix:
  ```swift
  let appVersion = Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "—"
  ```

---

### [LOW] `idlePauseAction` Picker has no "Custom"/unknown fallback — daemon value outside {"pause","pause_and_mute"} yields no selection
- file: Always/Sources/Always/Views/Settings/BehaviorPanel.swift:185-188
- category: correctness
- confidence: low
- impact: If the config file (hand-edited or from a newer/older daemon) holds an `idle_pause_action` value other than the two tags, the segmented Picker has no matching tag and renders with nothing selected; first interaction then snaps to whatever segment the user taps, silently changing config.
- problem:
  ```swift
  Picker("", selection: $config.idlePauseAction) {
      Text("Pause Only").tag("pause")
      Text("Pause + Mute").tag("pause_and_mute")
  }
  ```
  No tag covers unknown values; SwiftUI `.segmented` with an unmatched selection is the same unstable-selection family flagged above.
- fix: Normalize on load (coerce unknown to `"pause"`), or add a hidden fallback tag so the selection always matches one option.

---

### [LOW] CorrectionDialog force-unwraps `panel.contentView!.bounds`
- file: Always/Sources/Always/Views/CorrectionDialog.swift:41
- category: crash-panic
- confidence: low
- impact: `NSPanel.contentView` is optional; force-unwrap during dialog construction would crash if AppKit ever returns nil for the freshly-created panel (rare but a hard crash on the correction hotkey path).
- problem:
  ```swift
  let container = NSView(frame: panel.contentView!.bounds)
  ```
- fix: Build the container from the known contentRect instead of the optional:
  ```swift
  let container = NSView(frame: NSRect(x: 0, y: 0, width: 420, height: 160))
  ```
  (You already overwrite `panel.contentView = container` at :74, so reading the existing contentView's bounds is unnecessary.)

---

### [LOW] `VocabularyPanel.refreshVocabularyInfo` swallows a non-array glossary, reporting 0 terms for a valid-but-object JSON
- file: Always/Sources/Always/Views/Settings/VocabularyPanel.swift:88-94
- category: error-handling
- confidence: low
- impact: If `glossary.json` is a valid JSON object (or top-level dict) rather than an array, the cast `as? [[String: Any]]` fails and the UI shows "0 terms" even though the file is non-empty and the daemon may be using it — misleading the user into thinking their glossary is empty.
- problem:
  ```swift
  let json = try? JSONSerialization.jsonObject(with: data) as? [[String: Any]]
  ... vocabularyTermCount = json.count
  ```
  Any shape other than an array-of-objects silently reports 0.
- fix: Handle both array and object shapes, or show "unrecognized format" rather than "0 terms" when parsing succeeds but the shape is unexpected.

---

## Notes on things that are correct (not findings)
- `NumericSettingRow` generic over `Numeric & LosslessStringConvertible & Comparable` with optional `ClosedRange` is sound; the clamp logic itself is correct, the issue is timing/formatter-range mismatch (above).
- `ModelsSection` correctly guards `models.models.isEmpty` and uses `.first(where:)` rather than index subscripting (good — avoids the out-of-range crash class). The `.tint(.accentColor)` comment documents the prior macOS 26 recursion fix; leaving plain `.accentColor` is the right call.
- `CorrectionDialog` correctly keeps `buttonTarget` alive for the panel lifetime and nils it on close (no obvious leak).
- `SettingsSidebar` `.contentShape(Rectangle())` before `.onTapGesture` is the correct fix for dead-zone hit-testing.
