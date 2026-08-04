<!-- gitnexus:start -->
# GitNexus — Code Intelligence

This project is indexed by GitNexus as **always** (4794 symbols, 11926 relationships, 300 execution flows). Use the GitNexus MCP tools to understand code, assess impact, and navigate safely.

> Index stale? Run `node .gitnexus/run.cjs analyze` from the project root — it auto-selects an available runner. No `.gitnexus/run.cjs` yet? `npx gitnexus analyze` (npm 11 crash → `npm i -g gitnexus`; #1939).

## Always Do

- **MUST run impact analysis before editing any symbol.** Before modifying a function, class, or method, run `impact({target: "symbolName", direction: "upstream"})` and report the blast radius (direct callers, affected processes, risk level) to the user.
- **MUST run `detect_changes()` before committing** to verify your changes only affect expected symbols and execution flows. For regression review, compare against the default branch: `detect_changes({scope: "compare", base_ref: "develop"})`.
- **MUST warn the user** if impact analysis returns HIGH or CRITICAL risk before proceeding with edits.
- When exploring unfamiliar code, use `query({search_query: "concept"})` to find execution flows instead of grepping. It returns process-grouped results ranked by relevance.
- When you need full context on a specific symbol — callers, callees, which execution flows it participates in — use `context({name: "symbolName"})`.
- For security review, `explain({target: "fileOrSymbol"})` lists taint findings (source→sink flows; needs `analyze --pdg`).

## Never Do

- NEVER edit a function, class, or method without first running `impact` on it.
- NEVER ignore HIGH or CRITICAL risk warnings from impact analysis.
- NEVER rename symbols with find-and-replace — use `rename` which understands the call graph.
- NEVER commit changes without running `detect_changes()` to check affected scope.

## Resources

| Resource | Use for |
|----------|---------|
| `gitnexus://repo/always/context` | Codebase overview, check index freshness |
| `gitnexus://repo/always/clusters` | All functional areas |
| `gitnexus://repo/always/processes` | All execution flows |
| `gitnexus://repo/always/process/{name}` | Step-by-step execution trace |

## CLI

| Task | Read this skill file |
|------|---------------------|
| Understand architecture / "How does X work?" | `.claude/skills/gitnexus/gitnexus-exploring/SKILL.md` |
| Blast radius / "What breaks if I change X?" | `.claude/skills/gitnexus/gitnexus-impact-analysis/SKILL.md` |
| Trace bugs / "Why is X failing?" | `.claude/skills/gitnexus/gitnexus-debugging/SKILL.md` |
| Rename / extract / split / refactor | `.claude/skills/gitnexus/gitnexus-refactoring/SKILL.md` |
| Tools, resources, schema reference | `.claude/skills/gitnexus/gitnexus-guide/SKILL.md` |
| Index, status, clean, wiki CLI commands | `.claude/skills/gitnexus/gitnexus-cli/SKILL.md` |

<!-- gitnexus:end -->

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

# GitNexus Index-Powered Development

## Golden Rule: Think in Processes, Not Files

After running `npx gitnexus analyze`, **always use GitNexus tools instead of grep/find/manual exploration**. The index understands execution flows, dependencies, and impact — grep does not.

## Required Before Any Edit

### 1. Impact Analysis (MUST run before editing)

Before modifying ANY function, class, method, or file:

```
gitnexus_impact({target: "symbolName", direction: "upstream"})
```

**Report to user**: Direct callers, affected processes, risk level.

**Why**: A 1-line change might break 5 distant call sites. Without impact analysis, you ship silent bugs.

### 2. Change Detection (MUST run before commit)

After making changes:

```
gitnexus_detect_changes()
```

Verify your changes only touch expected symbols and execution flows.

**Why**: Prevents accidental scope creep and unintended modifications.

## Replacing grep/find with GitNexus Queries

### Never Do This Anymore

```bash
# grep for code patterns
grep -r "handleClick" src/

# find files by name/path
find . -name "*Auth*" -type f

# manual symbol hunting
grep -r "getUserData" --include="*.ts"
```

### Use GitNexus Instead

**Search by execution flow** (replaces grep):

```
gitnexus_query({query: "handle user authentication"})
```

Returns: process-grouped results ranked by relevance. Includes call graph context.

**Understand a symbol** (replaces manual exploration):

```
gitnexus_context({name: "getUserData"})
```

Returns: all callers, callees, which processes it participates in, and data flow.

**Find related code** (replaces find + grep):

```
gitnexus_clusters()   # All functional areas
gitnexus_processes()  # All execution flows by business logic
```

## Refactoring with GitNexus (Safe by Definition)

Instead of find-and-replace + manual checking:

**Rename a symbol safely**:

```
gitnexus_rename({target: "oldName", newName: "newName"})
```

Updates all call sites in the call graph. No orphaned references.

**Extract a function**:

```
gitnexus_extract({source: "filePath:line", target: "newFile", symbol: "extractedFunctionName"})
```

Automatically updates all callers.

**Understand what can be split**:

```
gitnexus_impact({target: "moduleName", direction: "bidirectional"})
```

Shows if a module can be split without breaking dependencies.

## Debugging with GitNexus

When tracing a bug:

**Don't**: Grep for error message, manually trace call stacks.
**Do**:

```
gitnexus_query({query: "error: null pointer in payment flow"})
```

Returns all execution paths that can trigger this error, ranked by likelihood.

**Trace execution from entry point**:

```
gitnexus_process({name: "checkoutFlow"})
```

Shows step-by-step execution: where it enters, every function called, data transformations, where it exits.

## Exploration by Business Logic (Not Files)

Replace "understanding the codebase" grep-and-read sessions:

```
gitnexus_clusters()                       # functional areas
gitnexus_processes()                      # core business flows
gitnexus_process({name: "userLoginFlow"}) # trace a flow
gitnexus_query({query: "session validation"})
```

## Enforcement Rules (NEVER violate)

1. NEVER grep for symbols — use `gitnexus_context()` or `gitnexus_query()`.
2. NEVER use find for code exploration — use `gitnexus_clusters()` or `gitnexus_processes()`.
3. NEVER edit a symbol without `gitnexus_impact()` first.
4. NEVER rename with find-and-replace — use `gitnexus_rename()`.
5. NEVER commit without `gitnexus_detect_changes()` to verify scope.
6. NEVER manually trace code flows — use `gitnexus_process()` or `gitnexus_query()`.

## Quick Reference

| Task | Tool | Bad Alternative |
|------|------|-----------------|
| Find callers of a function | `gitnexus_context({name: "foo"})` | `grep -r "foo(" src/` |
| Search by concept | `gitnexus_query({query: "auth"})` | `grep -r "auth" src/` |
| Understand a process | `gitnexus_process({name: "checkout"})` | Read files manually |
| Find impact before edit | `gitnexus_impact({target: "foo"})` | Change + test + pray |
| Rename safely | `gitnexus_rename(...)` | Find-and-replace |
| Verify changes are scoped | `gitnexus_detect_changes()` | Manual review |
| Explore architecture | `gitnexus_clusters()` | Read file tree |
| Extract a function | `gitnexus_extract(...)` | Manual refactoring |

## Why This Matters

**Before GitNexus**: code exploration = slow grep sessions, missed dependencies, silent bugs.
**After GitNexus**: code exploration = semantic search, full call graph, safe refactoring by definition.

The index understands your code's execution model. Use it.
