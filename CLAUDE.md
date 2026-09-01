<!-- gitpixel:start -->
# GitPixel — Agent Workflow Contract

> Before the first file read of any feature/bug task, run
> `gitpixel targets "<task>"`. It returns a closed prioritized file list
> (P0/P1/P2) and activates `.gitpixel/targets.json`. Work P0 first; P2 is
> droppable. While the manifest is active, never read, grep, or edit repo
> files outside the list — if a file seems missing, the task description was
> wrong: re-run `gitpixel targets` with a refined task. Run
> `gitpixel targets --clear` when the task ends.
> When the user says something **was working before** (or the fix is in git
> history), run `gitpixel rescue "<problem>"` — never `git reset --hard`,
> never raw historical checkouts over in-progress work.

Claude Code additionally enforces all of this mechanically via the
`gitpixel-targets-guard` PreToolUse hook (off-list reads/edits blocked,
edits without an active manifest blocked, `git reset --hard` blocked).
Kill switch for debugging the guard: `GITPIXEL_TARGETS_GUARD=0`.

## CLI Reference

| Task                                         | Command                                              |
| -------------------------------------------- | ---------------------------------------------------- |
| Rebuild index + graph                        | `gitpixel ready . --no-daemon`                       |
| Regex search                                 | `gitpixel search '<regex>' .`                        |
| Symbol lookup                                | `gitpixel symbol <name> .`                           |
| 360° context (token-budgeted)                | `gitpixel context <uid> . --budget 4000`             |
| Blast radius / "What breaks if I change X?"  | `gitpixel impact <symbol> . --direction upstream`    |
| Callers / callees                            | `gitpixel uses <symbol> . --role callers`            |
| Trace A→B                                    | `gitpixel trace <a> <b> .`                           |
| Execution flows                              | `gitpixel processes .`                               |
| Functional clusters                          | `gitpixel clusters .`                                |
| Git-diff → affected flows                    | `gitpixel changes .`                                 |
| Task scoping (closed file list)              | `gitpixel targets "<task>" .`                        |
| Surgical revert planner                      | `gitpixel rescue "<problem>" .`                      |
| Error capture (wrap a command)               | `gitpixel sniper run -- <cmd>`                       |
| Newest errors                                | `gitpixel sniper last`                               |

`lower_bound: true` in a response = the resolver gave up on N same-name call sites; returned edges are a **lower bound**, not the full set. Treat "0 callers" + `lower_bound: true` as "unknown", not "unused".

<!-- gitpixel:end -->

# GitPixel Index-Powered Development

## Golden Rule: Think in Processes, Not Files

After running `gitpixel ready .`, **always use the `gitpixel` CLI instead of grep/find/manual exploration**. The index understands execution flows, dependencies, and impact — grep does not.

## Required Before Any Edit

### 1. Impact Analysis (MUST run before editing)

Before modifying ANY function, class, method, or file:

```bash
gitpixel impact <symbol> . --direction upstream
```

**Report to user**: Direct callers (d1 WILL BREAK), affected flows, risk level.

**Why**: A 1-line change might break 5 distant call sites. Without impact analysis, you ship silent bugs.

### 2. Change Detection (MUST run before commit)

After making changes:

```bash
gitpixel changes .
```

Verify your changes only touch expected symbols and execution flows.

**Why**: Prevents accidental scope creep and unintended modifications.

## Replacing grep/find with GitPixel

### Never Do This Anymore

```bash
# grep for code patterns
grep -r "handleClick" src/

# find files by name/path
find . -name "*Auth*" -type f

# manual symbol hunting
grep -r "getUserData" --include="*.ts"
```

### Use GitPixel Instead

**Search** (replaces grep — trigram-indexed, sound by construction):

```bash
gitpixel search 'handleClick' .
```

**Understand a symbol** (replaces manual exploration):

```bash
gitpixel symbol getUserData .
gitpixel uses getUserData . --role callers
```

Returns: all callers, callees, with confidence tiers.

**Find related code** (replaces find + grep):

```bash
gitpixel clusters .    # All functional areas
gitpixel processes .   # All execution flows by business logic
```

## Debugging with GitPixel

When tracing a bug:

**Don't**: Grep for error message, manually trace call stacks.
**Do**:

```bash
gitpixel search 'null pointer' .
gitpixel sniper last    # check the error sink for recent failures
```

**Trace execution from entry point**:

```bash
gitpixel processes .
gitpixel trace <entrySymbol> <targetSymbol> .
```

## Task Scoping

Before the first file read of any feature/bug task:

```bash
gitpixel targets "<task description>" .
```

Returns a closed prioritized file list (P0 = start here, P1 = likely, P2 = droppable). Clear with `gitpixel targets --clear .` when done.

## Surgical Revert ("was working before")

```bash
gitpixel rescue "<problem>" .
```

Never `git reset --hard` — use `rescue` to find and revert the breaking commit.

## Enforcement Rules (NEVER violate)

1. NEVER grep for symbols — use `gitpixel symbol` or `gitpixel search`.
2. NEVER use find for code exploration — use `gitpixel clusters` or `gitpixel processes`.
3. NEVER edit a symbol without `gitpixel impact` first.
4. NEVER commit without `gitpixel changes .` to verify scope.
5. NEVER manually trace code flows — use `gitpixel trace` or `gitpixel processes`.
6. NEVER `git reset --hard` over in-progress work — use `gitpixel rescue`.

## Quick Reference

| Task | Command | Bad Alternative |
|------|---------|-----------------|
| Find callers of a function | `gitpixel uses foo . --role callers` | `grep -r "foo(" src/` |
| Search by regex | `gitpixel search 'auth' .` | `grep -r "auth" src/` |
| Understand a flow | `gitpixel processes .` | Read files manually |
| Find impact before edit | `gitpixel impact foo . --direction upstream` | Change + test + pray |
| Verify changes are scoped | `gitpixel changes .` | Manual review |
| Explore architecture | `gitpixel clusters .` | Read file tree |
| Scope a task | `gitpixel targets "<task>" .` | Read everything |
| Revert surgically | `gitpixel rescue "<problem>" .` | `git reset --hard` |

## Why This Matters

**Before GitPixel**: code exploration = slow grep sessions, missed dependencies, silent bugs.
**After GitPixel**: code exploration = indexed regex search, call graph, blast-radius analysis, task scoping, surgical reverts.

The index understands your code's execution model. Use it.

# Always Voice-to-Text Development

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

**Every change to any `.rs` or `.swift` file MUST end with `scripts/dev-rebuild.sh`
completing successfully, before you report the work as finished.**

Not "deployed". Not "built". **Running.** These are different claims and only the
last one lets a human test what you did.

- Do NOT say done / fixed / shipped / ready while the old process is still alive.
- Do NOT hand back a change that only exists in `/Applications` but is not executing.
- Do NOT rely on `open -a Always` alone: if the app is already running, `open`
  just re-focuses the live instance and your new binary never executes.
- A Swift-only change still requires the full script. The GUI must be restarted
  to load it, and nothing else does that.

The script now proves this itself: after launching it compares each process's
start time against its binary's mtime and **exits non-zero** with
`✗ REBUILD NOT LIVE` if a process predates the code it was built from. If you see
that, the change is NOT testable — fix it before saying anything else.

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

**Required — use `scripts/dev-rebuild.sh` for every Rust+Swift rebuild.**
```bash
scripts/dev-rebuild.sh            # debug profile (default — transcripts visible)
scripts/dev-rebuild.sh release    # release profile (transcripts hidden)
```
The script kills the running app, rebuilds Rust + Swift, redeploys to `/Applications/Always.app`, and relaunches. It plays a short macOS system sound at each lifecycle marker (kill / compiled / up / fail) so you can hear progress while looking at logs. Mute with `ALWAYS_REBUILD_SILENT=1`.

**Do not use the manual equivalent for normal rebuilds.**
```bash
DO NOT run the manual equivalent for routine rebuilds.
```

**Manual equivalent (release for distribution) is only for special cases:**
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
