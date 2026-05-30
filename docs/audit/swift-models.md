# Swift Models Audit — scope: models

Files audited (all read in full):
- Always/Sources/Always/Models/Config.swift
- Always/Sources/Always/Models/ModelInfo.swift
- Always/Sources/Always/Models/DaemonStatus.swift
- Always/Sources/Always/Models/SetupValidation.swift

Note on confidence: the Swift sources were read in full and are authoritative. Cross-checks against the Rust daemon schema (`src/managers/model_registry.rs`) were attempted but the shell/Read tooling intermittently returned empty output for the Rust file, so schema-drift findings that depend on the exact Rust serialization are marked medium/low confidence and framed around the Swift-side contract that is verifiable from the Swift code + its own documentation comments.

---

### [CRITICAL] `ModelInfo` decode crashes the entire Models catalog if the daemon adds/renames any field or emits an unknown `engine_type`
- file: Always/Sources/Always/Models/ModelInfo.swift:34-53 (struct) and :7-16 / :46 (`engine_type: EngineType`)
- category: crash-panic
- confidence: high
- impact: A single new/renamed field in the Rust `ModelInfo`, or a new engine variant the daemon ships before the Swift app is updated, makes `JSONDecoder().decode([ModelInfo])` throw. Because every field is a non-optional `let` with no `init(from:)` and no defaults, the whole `models` array fails to decode — the entire Models tab silently shows nothing (or errors), not just the one new model. This is exactly the "drift = silent breakage" failure mode for a continuously-running menu-bar app that auto-updates on a different cadence than the daemon.
- problem: Every property is a required non-optional with no fallback, and the enum has no unknown-case handling:
  ```swift
  struct ModelInfo: Codable, Identifiable, Equatable, Hashable {
      let id: String
      let name: String
      ...
      let size_mb: UInt64
      ...
      let engine_type: EngineType        // throws on any unknown raw value
      let accuracy_score: Float
      ...
      let supported_languages: [String]
      ...
  }
  enum EngineType: String, Codable, Equatable {
      case whisper = "Whisper"
      ...
      case cohere = "Cohere"
      // no `case unknown` / no custom init(from:) — decode throws on anything else
  }
  ```
  `Codable`'s synthesized decoder treats every key as required. The doc comment says fields "match `ModelInfo` in model_registry.rs ... so the JSON ... decodes here" — that is precisely the brittle coupling: any daemon-side addition breaks the consumer.
- fix: Make the enum tolerant of unknown variants and add resilient decoding so one new field/model can't nuke the whole list. Minimum: tolerant enum:
  ```swift
  enum EngineType: String, Codable, Equatable {
      case whisper = "Whisper"
      // ... existing cases ...
      case cohere = "Cohere"
      case unknown

      init(from decoder: Decoder) throws {
          let raw = try decoder.singleValueContainer().decode(String.self)
          self = EngineType(rawValue: raw) ?? .unknown
      }
  }
  ```
  Better: decode `ModelInfo` defensively (custom `init(from:)` with `decodeIfPresent` + defaults for non-identity fields), and decode the array element-by-element so a single malformed entry is skipped, not the whole catalog. At minimum the wrapper should `decodeIfPresent` and drop bad rows rather than failing the array.

---

### [HIGH] `EngineType.unknown`/forward-compat absence forces a hard daemon↔app version lock
- file: Always/Sources/Always/Models/ModelInfo.swift:7-16
- category: correctness
- confidence: high
- impact: The Rust `EngineType` (per the file's own reference to `src/managers/model_registry.rs`) is the source of truth and can grow. The instant the daemon advertises a model with a new engine the user upgraded the daemon to but not the app (Sparkle update lag, partial install), the Models list decode throws (see CRITICAL above). This is a frequent real-world condition for separately-versioned binaries.
- problem: The enum is a closed set with no escape hatch:
  ```swift
  enum EngineType: String, Codable, Equatable {
      case whisper = "Whisper"
      ...
      case cohere = "Cohere"
  }
  ```
- fix: Same as the CRITICAL fix — add `.unknown` + custom `init(from:)`. Then `displayName` must handle `.unknown` (e.g. return a humanized raw string or "Unknown engine"). This decouples the two binaries' release cadence.

---

### [MEDIUM] `ModelInfo` numeric fields assume exact Rust integer types; size/score type drift silently corrupts or crashes
- file: Always/Sources/Always/Models/ModelInfo.swift:41 (`size_mb: UInt64`), :44 (`partial_size: UInt64`), :47-48 (`accuracy_score: Float`, `speed_score: Float`)
- category: correctness
- confidence: medium
- impact: `UInt64` decode throws if the daemon ever emits the size as a float (e.g. `146.0`) or a negative/`null`. `Float` for scores means a daemon emitting a JSON number with more precision is fine, but a `null` (which the daemon uses 0 as the sentinel for, per the `hidesScores` comment) is assumed to always be present and numeric. Any of these throwing kills the whole list (no per-field default).
- problem:
  ```swift
  let size_mb: UInt64
  let partial_size: UInt64
  let accuracy_score: Float
  let speed_score: Float
  ```
  No `decodeIfPresent`, no fallback. The `sizeLabel`/`hidesScores`/`languageLabel` helpers all assume these decoded successfully.
- fix: In a custom `init(from:)`, use `decodeIfPresent(UInt64.self) ?? 0` for sizes and `decodeIfPresent(Float.self) ?? 0` for scores, so missing/null/typed-differently values degrade gracefully instead of failing the array decode.

---

### [MEDIUM] `Config.fromCLI` line parser silently drops every value containing a colon (URLs, JSON, time strings)
- file: Always/Sources/Always/Models/Config.swift:77-81
- category: correctness
- confidence: high
- impact: `per_app_settings_json` is stored as a JSON blob (`perAppSettingsJson`), and the daemon's `config show` can emit values that themselves contain `:` (JSON objects, URLs, `HH:MM` etc.). `split(separator: ":", maxSplits: 1)` correctly splits only on the first colon, BUT the subsequent `parts.count == 2` guard combined with `.split(separator: "\n")` discards lines whose JSON value spans the printed output. More concretely: a line with no colon at all (header, blank-ish, multi-line JSON continuation) is dropped silently — and multi-line `per_app_settings_json` output is truncated to its first physical line, corrupting the stored JSON.
- problem:
  ```swift
  let lines = output.split(separator: "\n")
  for line in lines {
      let parts = line.split(separator: ":", maxSplits: 1)
      if parts.count == 2 { ... }   // single-line assumption
  }
  ```
  `maxSplits: 1` is correct for `key: value` with colons in the value, but the per-line model assumes the CLI never wraps a value across lines. If `per_app_settings_json` is pretty-printed or contains newlines, only the fragment up to the first `\n` is captured (line 133-134), producing invalid JSON in `perAppSettingsJson`.
- fix: Confirm the daemon emits `per_app_settings_json` as a single physical line (compact JSON). If it can be multi-line, switch `config show` to a machine-readable format (JSON) and decode it, or accumulate continuation lines until the next `key:` token. At minimum add a parse-failure log when `perAppSettingsJson` is set but not valid JSON.

---

### [MEDIUM] `Config.fromCLI` never returns nil, so callers can't distinguish "daemon gave us nothing" from "all defaults"
- file: Always/Sources/Always/Models/Config.swift:73-155
- category: error-handling
- confidence: high
- impact: The signature is `-> Config?` (implying failure is possible), but the body always returns a fully-populated `defaultConfig`-based value even when `output` is empty, an error string, or a panic message. The UI will display default settings as if they were the daemon's real config, and a user "Save" then overwrites real daemon config with defaults. Silent data-loss of user settings on a transient CLI failure.
- problem:
  ```swift
  static func fromCLI(output: String) -> Config? {
      var config = defaultConfig
      let lines = output.split(separator: "\n")
      for line in lines { ... }
      return config            // never nil, even for garbage/empty input
  }
  ```
- fix: Return `nil` when zero recognized keys were parsed (e.g. track a `matchedCount`, `guard matchedCount > 0 else { return nil }`). Callers can then keep the last-known-good config / retry instead of clobbering settings with defaults.

---

### [MEDIUM] `idle_pause_action` unknown values are silently ignored, leaving a stale/default action with no signal
- file: Always/Sources/Always/Models/Config.swift:137-140
- category: correctness
- confidence: medium
- impact: If the daemon adds a third idle action (e.g. `mute_only`) the Swift side silently keeps `defaultConfig.idlePauseAction` ("pause") and never warns — unlike unknown top-level keys which DO log. The user sees "pause" in the UI while the daemon is doing something else (drift with no diagnostic). Same closed-set risk as `EngineType` but here it fails closed-silent instead of throwing.
- problem:
  ```swift
  case "idle_pause_action":
      if value == "pause" || value == "pause_and_mute" {
          config.idlePauseAction = value
      }
      // else: silently dropped, no warning, value stays default
  ```
- fix: Add an `else { configLogger.warning("unknown idle_pause_action: \(value, privacy: .public)") }`, and consider storing the raw value so the UI can show "Unknown (\(value))" rather than misrepresenting it as "pause".

---

### [MEDIUM] `DaemonStatus.fromCLI` returns `isRunning: false` for any error/garbage output (false "not running")
- file: Always/Sources/Always/Models/DaemonStatus.swift:8-20
- category: error-handling
- confidence: high
- impact: The function only checks for the exact substring `"Always-on daemon running"`. Any other output — an error message, a timeout, a changed status string in a future daemon, a localized message — yields `isRunning: false`. Callers will conclude the daemon is dead and may (re)spawn it or show "not running" while it is actually alive, causing duplicate daemons (the project's CLAUDE.md explicitly warns "NEVER have parallel versions running").
- problem:
  ```swift
  if output.contains("Always-on daemon running") {
      ...
      return DaemonStatus(isRunning: true, ...)
  }
  return DaemonStatus(isRunning: false, pid: nil, logPath: nil)  // also the error/garbage case
  ```
  There is no third "unknown/parse-failed" state.
- fix: Make the return optional-meaningful: return `nil` (already `-> DaemonStatus?`) when the output matches neither a known "running" nor a known "stopped" marker, so callers treat it as "unknown — don't act" rather than "definitely stopped". Also `logPath` is always `nil` even though the struct/JSON suggests it should be populated (dead field — see LOW).

---

### [MEDIUM] `DaemonState.fromCLI` uses default `JSONDecoder` keys but the struct relies on Swift property names matching the daemon's JSON exactly
- file: Always/Sources/Always/Models/DaemonStatus.swift:23-30
- category: correctness
- confidence: medium
- impact: `DaemonState` has `isPaused`/`isAutoEnter` (camelCase) and decodes with a plain `JSONDecoder()` (no `convertFromSnakeCase`, no `CodingKeys`). If the Rust daemon serializes these as `is_paused`/`is_auto_enter` (snake_case — the convention used everywhere else in this codebase's wire format, e.g. all of `ModelInfo`'s snake_case fields), decode returns `nil` via `try?` and the caller silently gets no state. The pause/auto-enter UI would never reflect the real daemon state, with zero error surfaced.
- problem:
  ```swift
  struct DaemonState: Codable {
      let isPaused: Bool      // camelCase keys
      let isAutoEnter: Bool
      static func fromCLI(output: String) -> DaemonState? {
          guard let data = output.data(using: .utf8) else { return nil }
          return try? JSONDecoder().decode(DaemonState.self, from: data)  // swallows the error
      }
  }
  ```
  Compare `ModelInfo` (same file-set) which deliberately uses snake_case property names "so the JSON serialised by serde_json decodes here without per-field coding keys." `DaemonState` breaks that convention, strongly suggesting a key mismatch with the daemon.
- fix: Verify the daemon's JSON shape. If snake_case, either add `CodingKeys` (`case isPaused = "is_paused"`, `case isAutoEnter = "is_auto_enter"`) or set `decoder.keyDecodingStrategy = .convertFromSnakeCase`. Also replace `try?` with a logged `do/catch` so a future schema change is visible instead of silently producing nil.

---

### [LOW] `DaemonStatus.logPath` is a permanently-nil dead field
- file: Always/Sources/Always/Models/DaemonStatus.swift:6, :13-17, :19
- category: maintainability
- confidence: high
- impact: `logPath` is declared and part of the `Codable` surface but is hardcoded `nil` in every return path of `fromCLI`. Consumers binding to it will always get nothing; it's a misleading API that looks populated.
- problem:
  ```swift
  let logPath: String?
  ...
  return DaemonStatus(isRunning: true, pid: pid, logPath: nil)
  return DaemonStatus(isRunning: true, pid: nil, logPath: nil)
  return DaemonStatus(isRunning: false, pid: nil, logPath: nil)
  ```
- fix: Either parse the log path out of the CLI output (the daemon prints it) and populate it, or remove the field to avoid implying data that never arrives.

---

### [LOW] `Config` groq/shortcut parsing treats the literal token `(not set)` specially but any other sentinel slips through as a real value
- file: Always/Sources/Always/Models/Config.swift:109-130
- category: correctness
- confidence: medium
- impact: The code special-cases the exact substring `"(not set)"` to mean "unset". If the daemon ever prints a different unset marker (`<none>`, empty string, `null`) the GUI will store that literal as the API key / shortcut and then persist it back, writing a bogus value into the daemon config.
- problem:
  ```swift
  case "groq_api_key":
      if !value.contains("(not set)") { config.groqApiKey = value }
  case "shortcut_pause":
      if !value.contains("(not set)") { config.shortcutPause = value }
  // ...same pattern for the other shortcuts
  ```
  Also an empty `value` (key printed with nothing after the colon) is NOT treated as unset for shortcuts — it would overwrite the default with `""`.
- fix: Centralize an `isUnset(_ value:)` helper that also treats empty/`<none>`/`null` as unset, and guard against writing empty strings over non-empty defaults.

---

### [LOW] `Config.perAppSettingsJson` only nils on the exact string `"{}"`, not on whitespace/empty/`null`
- file: Always/Sources/Always/Models/Config.swift:133-134
- category: correctness
- confidence: medium
- impact: `config.perAppSettingsJson = value == "{}" ? nil : value` keeps `" {} "`, `""`, or `"null"` as a non-nil "settings present" value, so downstream code that checks `perAppSettingsJson != nil` to decide whether per-app overrides exist will misbehave for these equivalent-empty representations.
- problem:
  ```swift
  case "per_app_settings_json":
      config.perAppSettingsJson = value == "{}" ? nil : value
  ```
- fix: Normalize/trim and treat empty, `"{}"`, and `"null"` as nil: `let v = value.trimmingCharacters(in: .whitespaces); config.perAppSettingsJson = (v.isEmpty || v == "{}" || v == "null") ? nil : v`.

---

### [LOW] `ModelDownloadProgressData` / `ModelsListData` / event payload structs are all-required with no resilience
- file: Always/Sources/Always/Models/ModelInfo.swift:92-121
- category: error-handling
- confidence: medium
- impact: `ModelDownloadProgressData` (`model_id`, `downloaded`, `total`, `percentage`), `ModelIdData`, `ModelErrorData`, `ActiveTranscriberChangedData` are all non-optional. If the daemon renames any field or omits one for a particular event, the event decode throws and that progress/completion update is dropped — a download could appear stuck because a single progress frame failed to decode, or a "complete" event is missed leaving the UI in "downloading" forever.
- problem:
  ```swift
  struct ModelDownloadProgressData: Codable {
      let model_id: String
      let downloaded: UInt64
      let total: UInt64
      let percentage: Double
  }
  ```
  No tolerance for missing `total` (0 during early frames?) or `percentage` rounding type differences.
- fix: For the long-lived download UX, decode these defensively (`decodeIfPresent` with sane defaults: missing `total` → 0, missing `percentage` → compute from downloaded/total) and log decode failures rather than dropping frames silently.

---

### [LOW] `SetupValidation.groqKeyValidationResult` collapses all non-200 statuses into "Invalid API key"
- file: Always/Sources/Always/Models/SetupValidation.swift:8-12
- category: ux
- confidence: high
- impact: A `429` (rate limit), `500` (Groq outage), `403` (region/billing), or `nil` status all produce the message "Invalid API key - Groq rejected the credentials". Users with a perfectly valid key during a Groq outage are told their key is wrong and may delete/replace a good key. Misleading error → user data loss (good key discarded).
- problem:
  ```swift
  func groqKeyValidationResult(statusCode: Int?) -> GroqKeyValidationResult {
      statusCode == 200
          ? .valid
          : .invalid("Invalid API key - Groq rejected the credentials")
  }
  ```
- fix: Branch on status: `401/403` → invalid credentials; `429` → "Rate limited, try again"; `5xx` → "Groq is unavailable, key not verified"; `nil`/other → "Couldn't verify key". Only assert "invalid key" for auth-class codes.
