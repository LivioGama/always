# Always Voice-to-Text Development

## Opening pull requests

PRs in this repo are opened by `pr-gate`, which reviews and fixes the branch,
runs the real CI on a merge-preview branch, then opens the PR with a signature
marker. `.github/workflows/pr-gate-guard.yml` closes PRs without a valid one.

- Never run `gh pr create` or the GitHub MCP `create_pull_request` directly.
  Run `pr-gate submit` (the `gh` shim does this automatically).
- Release PRs (`release-x.y.z` -> `main`) are exempt; the `release` skill opens
  them directly.
- Maintainers can override with the `pr-gate-bypass` label (then reopen).

## Terminology Conventions

- **`dep`** — In the crate process, refers to checking a node of type "dependency". When we say "do it in dep", it means we check the dependency node in the dependency graph/crate system.

## What Must Be Rebuilt Together

The overlay system depends on TWO binaries that must be in sync:
- **Rust daemon** (`target/release/always`) — sends UDS events (voice, transcribing, pause, etc.)
- **Swift app** (`Always/Always.app`) — receives UDS events and shows the overlay

**If either is stale, the overlay silently breaks.** This is what causes "overlay disappeared" bugs.

### Rebuild Decision Matrix

| You changed... | Must rebuild |
|---|---|
| Any `.rs` file in `src/` | Rust daemon (`cargo build`), then Swift app (`build.sh`) |
| Any `.swift` file in `Always/Sources/` | Swift app only (`build.sh`) |
| Both | Rust first, then Swift |

**Why rebuild Swift after Rust changes?** `build.sh` copies the daemon binary into the Swift app bundle. If you only rebuild Rust, the bundle still has the old binary.

**Profile choice — debug for local dev, release for distribution.**
Local development uses the **debug** profile so `cfg!(debug_assertions)` is `true` and `should_log_transcripts()` returns `true` automatically — actual transcribed text shows in `always logs --pretty` without setting `ALWAYS_LOG_TRANSCRIPTS=1`. Release builds hide transcripts by default for privacy.

`build.sh` auto-picks the newest of `target/release/always` and `target/debug/always`. Force a profile with `ALWAYS_BUILD_PROFILE=release|debug ./build.sh`.

## 🚨 HARD RULE — `SPEC.md` is normative, and you keep it in sync

`SPEC.md` describes the behaviour this product is supposed to have. It is not
documentation written after the fact; it is the reference the code answers to.

**Every change that alters behaviour MUST update `SPEC.md` in the same commit.**
A spec that lags the code is worse than no spec — it looks authoritative while
lying, and the next person "fixes" the code to match a rule that no longer holds.

- Changing a default, a threshold, a timing, a state machine, or anything a user
  can perceive → update the relevant section, in the same commit.
- Fixing a bug where the code disagreed with the spec → the spec was right; say
  so in the commit message and leave it alone.
- Finding behaviour the spec does not describe → add it, or mark it `❓` if you
  could not verify it. Never guess and write it as fact.
- The **Invariants** section (§2) is the highest bar in the repo. If a change
  breaks one, it is a regression no matter what else it improves. Do not edit an
  invariant to make a change legal — raise it with the product owner.

**Never change behaviour that was not asked for.** Fix the reported symptom and
nothing else. If a fix genuinely requires touching adjacent behaviour — a
threshold, a timing, a default — stop and propose it with the trade-off stated.
"Fix it by any means" authorises effort, not scope. When a behavioural change
does ship, the commit message says who asked for it and why.

**Why this rule exists:** a session's worth of fixes drifted the product away
from what the owner wanted — a wider trust window, a changed threshold, altered
paste behaviour — each defensible alone, none requested, and none written down.
The owner had no way to see the accumulated change except by using the app and
finding it different. The spec is how that stops.

### 🚨 HARD RULE — nothing is "done" until the new build is RUNNING

**Every change to any `.rs` or `.swift` file MUST end with its platform's
development rebuild script completing successfully before you report the work
as finished.** Use `scripts/dev-rebuild-macos.sh` on macOS and
`scripts/dev-rebuild-linux.sh` on Linux.

Not "deployed". Not "built". **Running.** These are different claims and only the
last one lets a human test what you did.

- Do NOT say done / fixed / shipped / ready while the old process is still alive.
- Do NOT hand back a change that only exists in `/Applications` but is not executing.
- On macOS, do NOT rely on `open -a Always` alone: if the app is already running, `open`
  just re-focuses the live instance and your new binary never executes.
- A Swift-only change still requires the full script. The GUI must be restarted
  to load it, and nothing else does that.

The macOS script compares each process's start time against its binary's mtime;
the Linux script verifies the launched settings app belongs to this checkout.
Both exit non-zero when the new app cannot be verified. If either fails, the
change is NOT testable — fix it before saying anything else.

To check by hand at any time:

```bash
stat -f "%Sm %N" -t "%H:%M:%S" /Applications/Always.app/Contents/MacOS/Always
ps -o pid,lstart,comm -p $(pgrep -f "Always.app/Contents/MacOS/Always$" | head -1)
# the process start time MUST be later than the binary mtime
```

**Why this rule exists:** a Swift-only fix was built, deployed, and reported as
shipped while the GUI had been running since 100 minutes earlier — the script's
kill step was gated on Rust having changed, so nothing was killed and `open`
re-focused the stale instance. The user tested a build that was never running and
correctly reported "I see no change". Hours were lost on both sides.

### Simple Workflow (Do This Every Time)

**Required — use the platform-specific script for every rebuild.**
```bash
scripts/dev-rebuild-macos.sh            # macOS debug profile
scripts/dev-rebuild-macos.sh release    # macOS release profile
scripts/dev-rebuild-linux.sh            # Linux daemon + settings app
```
The macOS script rebuilds and relaunches `Always Dev.app`. The Linux script builds the daemon, frontend, and Tauri settings app, then launches it from this checkout.

**Do not use the manual equivalent for normal rebuilds.**
```bash
DO NOT run the manual equivalent for routine rebuilds.
```

**Manual equivalent (macOS release for distribution) is only for special cases:**
```bash
pkill -f "Always.app"
cargo build --release --lib --bin always
cd Always && ALWAYS_BUILD_PROFILE=release ./build.sh && open -a Always
```

**After Swift-only changes:**
```bash
pkill -f "Always.app"
cd Always && ./build.sh && open -a Always
```

### ⚠️ Critical: Always launch from `/Applications/Always.app`

`build.sh` automatically deploys to `/Applications/Always.app` as the final step. This is the canonical installed location. Always launch from there:

```bash
open -a Always
```

**Never** run directly from `Always/Always.app` in the project directory — that is only the intermediate build artifact before deployment.

### Why `./build.sh` and Not `swift run`

`swift run` builds in a temporary location and does NOT create the app bundle. The app bundle is required for:
- Code signing (Accessibility permissions)
- Copying the daemon binary into `Contents/MacOS/always`
- Proper launch via `open`

### Verifying the Overlay Is Wired Up

After launching, check these two logs to confirm the full stack is connected:

```bash
# UDS client connected to daemon?
cat /tmp/udsclient.log | tail -5
# Must show: ✅ Connected to daemon

# StateMonitor receiving events?
cat /tmp/statemonitor.log | tail -5
# Must show: received daemon event: ListeningStarted
```

If `/tmp/udsclient.log` doesn't exist, the running app is a stale build without UDS support.

### Detailed Steps

**CRITICAL:** To rebuild and launch the Always app (both daemon and Mac status bar app):

1. **Kill existing processes (no parallel versions):**
   ```bash
   pkill -f "Always.app"
   ```

2. **Build the Rust daemon (if any `.rs` changed):**
   ```bash
   cargo build --lib --bin always       # debug — local dev, transcripts visible
   # or:
   cargo build --release --lib --bin always   # release — distribution, transcripts hidden
   ```

3. **Build the Swift Mac app:**
   ```bash
   cd Always && ./build.sh
   ```

4. **Launch the Mac app:**
   ```bash
   open -a Always
   ```

**IMPORTANT:**
- **NEVER** run `./target/release/always start` directly — this bypasses CLIService environment variables
- **NEVER** reference `./target/release/always` in code — the daemon binary is embedded in the Mac app bundle at `Always.app/Contents/MacOS/always`
- **NEVER** have parallel versions running — always stop old instances before launching new ones
- The Mac app launches the daemon through CLIService which passes environment variables (like GROQ_API_KEY)
- `build.sh` builds, bundles the daemon binary, and deploys to `/Applications/Always.app` automatically

## Verification

After launching, verify both processes are running:
```bash
ps aux | grep -v grep | grep -i always
```

Should show:
- `/Applications/Always.app/Contents/MacOS/Always` (GUI)
- `.../always run --lang en --timeout 30 --silence 0.4` (daemon)

Check status bar for Always icon and logs:
```bash
# Pretty (emoji) streaming — preferred:
always logs --pretty
# (or, for the bundled CLI: /Applications/Always.app/Contents/MacOS/always logs --pretty)

# Raw JSON tail of today's file:
tail -F ~/Library/Logs/Always/always.$(date +%Y-%m-%d)
```

Transcripts (raw text in pasted/filtered/transcribed events) are visible automatically in **debug builds** (`cfg!(debug_assertions)` toggles `should_log_transcripts()`). For release builds set `ALWAYS_LOG_TRANSCRIPTS=1` (e.g. `launchctl setenv ALWAYS_LOG_TRANSCRIPTS 1`) before launching.

## Voice-to-Text Verification Checklist

**After ANY refactoring that touches `event_loop.rs`, `audio.rs`, or logging infrastructure:**

1. **BUILD → VERIFY → NEXT rule (non-negotiable):**
   ```bash
   cargo build --lib --bin always       # debug — local dev (use --release before shipping)
   pkill -f "Always.app"
   cd Always && ./build.sh && open -a Always
   sleep 2
   ```
   - Speak into mic — verify transcription appears in status bar
   - Check logs for "listening_started" event
   ```bash
   tail -20 ~/Library/Logs/Always/always.$(date +%Y-%m-%d) | grep listening
   ```
   - If no "listening_started" or "voice_detected" in logs → **audio pipeline is broken**, don't commit

2. **Verify log locations:**
   - New location: `~/Library/Logs/Always/always.YYYY-MM-DD` (JSON format)
   - Old location: `~/Library/Application Support/always/always.log` (frozen, don't use)
   - Both exist but only `~/Library/Logs/Always/` is active with new logging infrastructure

3. **Check the event chain:**
   - Daemon logs "listening_started" ✓
   - Daemon logs "voice_detected" on speech ✓
   - Swift app logs received UDS event ✓
   - Overlay appears on status bar ✓
   - If any step missing → something in the chain is broken

4. **Mark incomplete scaffolding explicitly:**
   - Use `TODO: audio stream wiring incomplete` comments if leaving work unfinished
   - Don't bury incomplete work in commit message details
   - Run `cargo check` to catch compilation errors before pushing

5. **Document breaking changes:**
   - Log location changes → mention in commit message
   - Event format changes → mention which modules are affected
   - API changes → add `MIGRATION.md` note

## Overlay Won't Show? Check the Event Chain

Overlay appearing = audio pipeline is working. No overlay = debug in this order:

**Event chain (all must fire in sequence):**
```
Audio capture starts
    ↓ (check logs for "listening_started")
Voice detected (VAD triggers)
    ↓ (check logs for "voice_detected")
Daemon sends UDS event
    ↓ (check Swift app logs for "received daemon event")
Swift app updates overlay
    ↓
Overlay shows on status bar
```

**Troubleshooting:**

1. **Logs show no "listening_started" or "voice_detected":**
   - Audio capture not initialized (most common after refactoring)
   - Check `event_loop.rs` — is audio stream startup wired?
   - Check that `MicrophoneMonitor` is NOT confused with audio capture (it's only a system monitor)

2. **Logs show "listening_started" but no "voice_detected":**
   - Audio streaming but VAD not triggering
   - Check microphone permission (System Preferences > Security & Privacy > Microphone)
   - Check microphone input level / silence threshold in config
   - Test with spoken voice, not just background noise

3. **Logs show "voice_detected" but no Swift app event logs:**
   - UDS socket broken or daemon not sending events
   - Check socket exists: `ls -la ~/Library/Caches/Always/always.sock`
   - Check daemon process: `ps aux | grep always`
   - Rebuild Swift app: `cd Always && ./build.sh && open -a Always`

4. **All events firing but no overlay:**
   - Swift app receiving events but UI not updating
   - Check Swift app logs in `~/Library/Logs/Always/`
   - Rebuild with `build.sh` (not `swift run`)
   - Verify code-signed: `codesign -v /Applications/Always.app`

**Pro tip:** If overlay is missing, **always check step 1 first** (daemon events). A silent daemon (runs but produces no events) is worse than a crashed daemon — harder to diagnose.
