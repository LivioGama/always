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

## 🚨 Rebuild After Changes — nothing is "done" until the new build is RUNNING

**Every change to any `.rs` or `.swift` file MUST end with `scripts/dev-rebuild.sh`
completing successfully, before the work is reported as finished.**

Not "deployed". Not "built". **Running.** Those are different claims, and only the
last one lets a human test what you did.

```bash
./scripts/dev-rebuild.sh
```

The script kills the app AND the daemon (unconditionally — a Swift-only change
still needs the GUI restarted to load it), builds Rust with the `local-stt`
feature, builds and bundles the Swift app, deploys to `/Applications/Always.app`,
relaunches, and then **verifies the running processes are the ones just built**.

### The verification is the point

After launching, the script compares each process's start time against its
binary's mtime and **exits non-zero** with `✗ REBUILD NOT LIVE` if a process
predates the code it was built from. If you see that, the change is NOT testable.
Fix it before saying anything else — never report the work as shipped.

Check by hand any time:

```bash
stat -f "%Sm %N" -t "%H:%M:%S" /Applications/Always.app/Contents/MacOS/Always
ps -o pid,lstart,comm -p $(pgrep -f "Always.app/Contents/MacOS/Always$" | head -1)
# process start time MUST be later than the binary mtime
```

### Never do these

- Do NOT say done / fixed / shipped / ready while the old process is still alive.
- Do NOT rely on `open -a Always` alone. If the app is already running, `open`
  re-focuses the live instance and the new binary never executes.
- Do NOT skip the script for a "small" Swift change. That is exactly the case
  that broke.

**Why this rule exists**: the kill step used to be gated on Rust having changed.
A Swift-only fix was therefore built, deployed, and reported as shipped while the
GUI had been running since 100 minutes earlier — `open` simply re-focused the
stale instance. The user tested a build that was never running and correctly
reported "I see no change". Hours were lost on both sides.

**Options**:
- `./scripts/dev-rebuild.sh release` - Release build instead of debug
- `./scripts/dev-rebuild.sh --no-daemon` - Skip daemon restart (use sparingly; the
  GUI is still restarted and verified)

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

