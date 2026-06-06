<!-- gitnexus:start -->
# GitNexus — Code Intelligence

This project is indexed by GitNexus as **always** (3573 symbols, 9068 relationships, 300 execution flows). Use the GitNexus MCP tools to understand code, assess impact, and navigate safely.

> If any GitNexus tool warns the index is stale, run `npx gitnexus analyze` in terminal first.

## Always Do

- **MUST run impact analysis before editing any symbol.** Before modifying a function, class, or method, run `gitnexus_impact({target: "symbolName", direction: "upstream"})` and report the blast radius (direct callers, affected processes, risk level) to the user.
- **MUST run `gitnexus_detect_changes()` before committing** to verify your changes only affect expected symbols and execution flows.
- **MUST warn the user** if impact analysis returns HIGH or CRITICAL risk before proceeding with edits.
- When exploring unfamiliar code, use `gitnexus_query({query: "concept"})` to find execution flows instead of grepping. It returns process-grouped results ranked by relevance.
- When you need full context on a specific symbol — callers, callees, which execution flows it participates in — use `gitnexus_context({name: "symbolName"})`.

## Never Do

- NEVER edit a function, class, or method without first running `gitnexus_impact` on it.
- NEVER ignore HIGH or CRITICAL risk warnings from impact analysis.
- NEVER rename symbols with find-and-replace — use `gitnexus_rename` which understands the call graph.
- NEVER commit changes without running `gitnexus_detect_changes()` to check affected scope.

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
