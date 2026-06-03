<!-- gitnexus:start -->
# GitNexus — Code Intelligence

This project is indexed by GitNexus as **always** (971 symbols, 2059 relationships, 81 execution flows). Use the GitNexus MCP tools to understand code, assess impact, and navigate safely.

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

# Always Voice-to-Text Development

## What Must Be Rebuilt Together

The overlay system depends on TWO binaries that must be in sync:
- **Rust daemon** (`target/release/always`) — sends UDS events (voice, transcribing, pause, etc.)
- **Swift app** (`AlwaysApp/AlwaysApp.app`) — receives UDS events and shows the overlay

**If either is stale, the overlay silently breaks.** This is what causes "overlay disappeared" bugs.

### Rebuild Decision Matrix

| You changed... | Must rebuild |
|---|---|
| Any `.rs` file in `src/` | Rust daemon (`cargo build --release`), then Swift app (`build.sh`) |
| Any `.swift` file in `AlwaysApp/Sources/` | Swift app only (`build.sh`) |
| Both | Rust first, then Swift |

**Why rebuild Swift after Rust changes?** `build.sh` copies the daemon binary into the Swift app bundle. If you only rebuild Rust, the bundle still has the old binary.

### Simple Workflow (Do This Every Time)

**After Rust changes:**
```bash
pkill -f AlwaysApp
cargo build --release --lib --bin always
cd AlwaysApp && ./build.sh && open -a AlwaysApp
```

**After Swift-only changes:**
```bash
pkill -f AlwaysApp
cd AlwaysApp && ./build.sh && open -a AlwaysApp
```

### ⚠️ Critical: Always launch from `/Applications/AlwaysApp.app`

`build.sh` automatically deploys to `/Applications/AlwaysApp.app` as the final step. This is the canonical installed location. Always launch from there:

```bash
open -a AlwaysApp
```

**Never** run directly from `AlwaysApp/AlwaysApp.app` in the project directory — that is only the intermediate build artifact before deployment.

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
   pkill -f AlwaysApp
   ```

2. **Build the Rust daemon (if any `.rs` changed):**
   ```bash
   cargo build --release --lib --bin always
   ```

3. **Build the Swift Mac app:**
   ```bash
   cd AlwaysApp && ./build.sh
   ```

4. **Launch the Mac app:**
   ```bash
   open -a AlwaysApp
   ```

**IMPORTANT:**
- **NEVER** run `./target/release/always start` directly — this bypasses CLIService environment variables
- **NEVER** reference `./target/release/always` in code — the daemon binary is embedded in the Mac app bundle at `AlwaysApp.app/Contents/MacOS/always`
- **NEVER** have parallel versions running — always stop old instances before launching new ones
- The Mac app launches the daemon through CLIService which passes environment variables (like GROQ_API_KEY)
- `build.sh` builds, bundles the daemon binary, and deploys to `/Applications/AlwaysApp.app` automatically

## Verification

After launching, verify both processes are running:
```bash
ps aux | grep -v grep | grep -i always
```

Should show:
- `/Applications/AlwaysApp.app/Contents/MacOS/AlwaysApp` (GUI)
- `.../always run --lang en --timeout 30 --silence 0.4` (daemon)

Check status bar for Always icon and logs:
```bash
tail -f "/Users/livio/Library/Application Support/always/always.log"
```
