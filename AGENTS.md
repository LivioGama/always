<!-- pixel:start -->
# Pixel — Agent Workflow Contract

> Before the first file read of any feature/bug task, run
> `pixel targets "<task>"`. It returns a closed prioritized file list
> (P0/P1/P2) and activates `.pixel/targets.json`. Work P0 first; P2 is
> droppable. While the manifest is active, never read, grep, or edit repo
> files outside the list — if a file seems missing, the task description was
> wrong: re-run `pixel targets` with a refined task. Run
> `pixel targets --clear` when the task ends.
> When the user says something **was working before** (or the fix is in git
> history), run `pixel rescue "<problem>"` — never `git reset --hard`,
> never raw historical checkouts over in-progress work.

Claude Code additionally enforces all of this mechanically via the
`pixel-targets-guard` PreToolUse hook (off-list reads/edits blocked,
edits without an active manifest blocked, `git reset --hard` blocked).
Kill switch for debugging the guard: `PIXEL_TARGETS_GUARD=0`.

## CLI Reference

| Task                                         | Command                                              |
| -------------------------------------------- | ---------------------------------------------------- |
| Rebuild index + graph                        | `pixel ready . --no-daemon`                       |
| Regex search                                 | `pixel search '<regex>' .`                        |
| Symbol lookup                                | `pixel symbol <name> .`                           |
| 360° context (token-budgeted)                | `pixel context <uid> . --budget 4000`             |
| Blast radius / "What breaks if I change X?"  | `pixel impact <symbol> . --direction upstream`    |
| Callers / callees                            | `pixel uses <symbol> . --role callers`            |
| Trace A→B                                    | `pixel trace <a> <b> .`                           |
| Execution flows                              | `pixel processes .`                               |
| Functional clusters                          | `pixel clusters .`                                |
| Git-diff → affected flows                    | `pixel changes .`                                 |
| Task scoping (closed file list)              | `pixel targets "<task>" .`                        |
| Surgical revert planner                      | `pixel rescue "<problem>" .`                      |
| Error capture (wrap a command)               | `pixel sniper run -- <cmd>`                       |
| Newest errors                                | `pixel sniper last`                               |

`lower_bound: true` in a response = the resolver gave up on N same-name call sites; returned edges are a **lower bound**, not the full set. Treat "0 callers" + `lower_bound: true` as "unknown", not "unused".

<!-- pixel:end -->

# Always Project Rules

## 🚨 `SPEC.md` is normative — keep it in sync with the code

`SPEC.md` describes the behaviour this product is supposed to have. It is the
reference the code answers to, not documentation written afterwards.

**Every change that alters behaviour MUST update `SPEC.md` in the same commit.**
A spec that lags the code is worse than no spec: it looks authoritative while
lying, and the next person "fixes" working code to match a rule that no longer
holds.

- Changing a default, threshold, timing, state machine, or anything a user can
  perceive → update the relevant section in the same commit.
- Code disagreed with the spec and you fixed the code → the spec was right. Say
  so in the commit message; leave the spec alone.
- Found behaviour the spec does not describe → add it, or mark it `❓` if you
  could not verify it. Never guess and write it as fact.
- The **Invariants** (§2) are the highest bar in the repo. Breaking one is a
  regression regardless of what else improved. Never edit an invariant to make a
  change legal — raise it with the product owner.

**Never change behaviour that was not asked for.** Fix the reported symptom and
nothing else. If a fix genuinely requires touching adjacent behaviour, stop and
propose it with the trade-off stated. "Fix it by any means" authorises effort,
not scope. Every behavioural change that does ship names its requester in the
commit message.

## 🚨 NEVER kill or replace the production app — dev and prod are separate installs

Two Always identities coexist on this machine:

| | Production | Development |
|---|---|---|
| App | `/Applications/Always.app` | `/Applications/Always Dev.app` |
| Bundle id | `com.always.v3` | `com.always.v3.dev` |
| State dir | `~/Library/Application Support/always` | `~/Library/Application Support/always-dev` |
| Socket | `~/Library/Caches/Always/always.sock` | `~/Library/Caches/Always/always-dev.sock` |
| URL scheme | `always://` | `always-dev://` |
| Sparkle | on | off |

**The user dictates through the production app while agents work. Any command
that kills it cuts the user's voice mid-sentence. Therefore:**

- NEVER `pkill`, `kill`, `osascript -e 'quit'`, or otherwise terminate any
  process under `/Applications/Always.app` — or a bare `always-daemon run` /
  `always run` pattern that could match it. Only `Always Dev.app`-scoped
  patterns are safe (the literal string `Always Dev.app` can never match the
  prod path `Always.app`).
- NEVER deploy over, rsync into, or delete `/Applications/Always.app`.
- NEVER run `scripts/promote.sh` — it is the manual, user-invoked path that
  replaces the production app. If a change is ready to ship, say so and let
  the user run it.
- Dev-instance daemons can also be run from a terminal with
  `ALWAYS_INSTANCE=dev ./target/debug/always run` — same isolated paths, no
  bundle required.
- Expected behaviour: a dev daemon startup pauses the production daemon
  (one mic, never two transcribers) and production auto-resumes when the
  dev daemon exits. Seeing prod "paused" while a dev daemon runs is the
  handoff working, not a bug — do not "fix" it by unpausing prod.

## 🚨 Rebuild After Changes — nothing is "done" until the new build is RUNNING

**Every change to any `.rs` or `.swift` file MUST end with `scripts/dev-rebuild.sh`
completing successfully, before the work is reported as finished.**

Not "deployed". Not "built". **Running.** Those are different claims, and only the
last one lets a human test what you did.

```bash
./scripts/dev-rebuild.sh
```

The script kills the DEV app AND its daemon (unconditionally — a Swift-only
change still needs the GUI restarted to load it), builds Rust with the
`local-stt` feature, builds and bundles the Swift app as `Always Dev.app`,
deploys to `/Applications/Always Dev.app`, relaunches, and then **verifies the
running dev processes are the ones just built** — plus confirms the production
GUI is still alive at the end.

### The verification is the point

After launching, the script compares each process's start time against its
binary's mtime and **exits non-zero** with `✗ REBUILD NOT LIVE` if a process
predates the code it was built from. If you see that, the change is NOT testable.
Fix it before saying anything else — never report the work as shipped.

Check by hand any time:

```bash
stat -f "%Sm %N" -t "%H:%M:%S" "/Applications/Always Dev.app/Contents/MacOS/Always"
ps -o pid,lstart,comm -p $(pgrep -f "Always Dev.app/Contents/MacOS/Always$" | head -1)
# process start time MUST be later than the binary mtime
```

### Never do these

- Do NOT say done / fixed / shipped / ready while the old dev process is still alive.
- Do NOT rely on `open` alone. If the app is already running, `open`
  re-focuses the live instance and the new binary never executes.
- Do NOT skip the script for a "small" Swift change. That is exactly the case
  that broke.
- Do NOT substitute `promote.sh` for `dev-rebuild.sh` to satisfy this rule.

**Why this rule exists**: the kill step used to be gated on Rust having changed.
A Swift-only fix was therefore built, deployed, and reported as shipped while the
GUI had been running since 100 minutes earlier — `open` simply re-focused the
stale instance. The user tested a build that was never running and correctly
reported "I see no change". Hours were lost on both sides.

**Options**:
- `./scripts/dev-rebuild.sh release` - Release build instead of debug
- `./scripts/dev-rebuild.sh --no-daemon` - Skip daemon restart (use sparingly; the
  GUI is still restarted and verified)

### Shipping to production

`./scripts/promote.sh [debug|release]` — run by the user only. Kills production,
rebuilds, deploys over `/Applications/Always.app`, relaunches, verifies.

# Pixel Index-Powered Development

## Golden Rule: Think in Processes, Not Files

After running `pixel ready .`, **always use the `pixel` CLI instead of grep/find/manual exploration**. The index understands execution flows, dependencies, and impact — grep does not.

## Required Before Any Edit

### 1. Impact Analysis (MUST run before editing)

Before modifying ANY function, class, method, or file:

```bash
pixel impact <symbol> . --direction upstream
```

**Report to user**: Direct callers (d1 WILL BREAK), affected flows, risk level.

**Why**: A 1-line change might break 5 distant call sites. Without impact analysis, you ship silent bugs.

### 2. Change Detection (MUST run before commit)

After making changes:

```bash
pixel changes .
```

Verify your changes only touch expected symbols and execution flows.

**Why**: Prevents accidental scope creep and unintended modifications.

## Replacing grep/find with Pixel

### Never Do This Anymore

```bash
# grep for code patterns
grep -r "handleClick" src/

# find files by name/path
find . -name "*Auth*" -type f

# manual symbol hunting
grep -r "getUserData" --include="*.ts"
```

### Use Pixel Instead

**Search** (replaces grep — trigram-indexed, sound by construction):

```bash
pixel search 'handleClick' .
```

**Understand a symbol** (replaces manual exploration):

```bash
pixel symbol getUserData .
pixel uses getUserData . --role callers
```

Returns: all callers, callees, with confidence tiers.

**Find related code** (replaces find + grep):

```bash
pixel clusters .    # All functional areas
pixel processes .   # All execution flows by business logic
```

## Debugging with Pixel

When tracing a bug:

**Don't**: Grep for error message, manually trace call stacks.
**Do**:

```bash
pixel search 'null pointer' .
pixel sniper last    # check the error sink for recent failures
```

**Trace execution from entry point**:

```bash
pixel processes .
pixel trace <entrySymbol> <targetSymbol> .
```

## Task Scoping

Before the first file read of any feature/bug task:

```bash
pixel targets "<task description>" .
```

Returns a closed prioritized file list (P0 = start here, P1 = likely, P2 = droppable). Clear with `pixel targets --clear .` when done.

## Surgical Revert ("was working before")

```bash
pixel rescue "<problem>" .
```

Never `git reset --hard` — use `rescue` to find and revert the breaking commit.

## Enforcement Rules (NEVER violate)

1. NEVER grep for symbols — use `pixel symbol` or `pixel search`.
2. NEVER use find for code exploration — use `pixel clusters` or `pixel processes`.
3. NEVER edit a symbol without `pixel impact` first.
4. NEVER commit without `pixel changes .` to verify scope.
5. NEVER manually trace code flows — use `pixel trace` or `pixel processes`.
6. NEVER `git reset --hard` over in-progress work — use `pixel rescue`.

## Quick Reference

| Task | Command | Bad Alternative |
|------|---------|-----------------|
| Find callers of a function | `pixel uses foo . --role callers` | `grep -r "foo(" src/` |
| Search by regex | `pixel search 'auth' .` | `grep -r "auth" src/` |
| Understand a flow | `pixel processes .` | Read files manually |
| Find impact before edit | `pixel impact foo . --direction upstream` | Change + test + pray |
| Verify changes are scoped | `pixel changes .` | Manual review |
| Explore architecture | `pixel clusters .` | Read file tree |
| Scope a task | `pixel targets "<task>" .` | Read everything |
| Revert surgically | `pixel rescue "<problem>" .` | `git reset --hard` |

## Why This Matters

**Before Pixel**: code exploration = slow grep sessions, missed dependencies, silent bugs.
**After Pixel**: code exploration = indexed regex search, call graph, blast-radius analysis, task scoping, surgical reverts.

The index understands your code's execution model. Use it.

## Daemon Memory Profile (investigated 2026-08-30)

The daemon's ~700-900 MB RSS is **expected** when running a local STT model.
Breakdown (measured with `vmmap --summary`):

| Component | Resident | Notes |
|---|---|---|
| Parakeet encoder (int8 ONNX) | ~470 MB | 622 MB on disk, loaded into MALLOC_LARGE |
| ONNX runtime arenas | ~184 MB | MALLOC_SMALL — intermediate activation buffers |
| Silero VAD | ~2 MB | Embedded at compile time |
| WeSpeaker (My Voice) | ~25 MB | Only when speaker gate is enabled |
| Libraries (__TEXT/__LINKEDIT) | ~250 MB | Shared, mostly read-only |
| Rust heap + stacks | ~50 MB | 34 threads × 2 MB stack + allocations |

**CoreML was tried and reverted** — CoreML only supports 15/39 encoder nodes and
4/35 decoder nodes of the Parakeet graph, so weights stay in process RAM AND
CoreML adds its own compiled-model overhead, making RSS **worse** (1350 MB vs
865 MB CPU-only). See the `ort` dependency comment in `Cargo.toml`.

The only real memory reductions are:
1. Switch to Groq (cloud backend, ~80 MB RSS — no local model loaded)
2. Use a smaller local model (e.g. Moonshine-medium at 289 MB vs Parakeet at 631 MB)
3. Accept the cost — 622 MB of weights is inherent to this model class on CPU

