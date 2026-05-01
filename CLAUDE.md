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

## Build & Launch Process

**CRITICAL RULE:** After making ANY code changes to the AlwaysApp (Swift) or daemon (Rust), you MUST automatically rebuild and relaunch the app to test the changes.

### Simple Workflow (Do This Every Time)

```bash
pkill -f AlwaysApp
cd AlwaysApp && ./build.sh && open AlwaysApp.app
```

### Why This Process Matters

**Common Mistake:** Using `swift run` to test changes
- `swift run` builds in a temporary location
- It does NOT create the app bundle at the correct location
- Changes won't apply to the deployed app
- The running app uses the old version

**Correct Process:** Use `./build.sh`
- Builds the Swift app properly
- Copies the daemon binary into the app bundle
- Creates the app bundle at `AlwaysApp/AlwaysApp.app`
- Code signs the app for proper permissions
- This is the actual deployed version

### Detailed Steps

**CRITICAL:** To rebuild and launch the Always app (both daemon and Mac status bar app):

1. **Kill existing processes (no parallel versions):**
   ```bash
   ./target/release/always stop
   osascript -e 'tell application "AlwaysApp" to quit'
   ```

2. **Build the Rust daemon:**
   ```bash
   cargo build --release --lib --bin always
   ```

3. **Build the Swift Mac app:**
   ```bash
   cd AlwaysApp && ./build.sh
   ```

4. **Deploy Mac app to correct location:**
   ```bash
   # The build script creates AlwaysApp.app in AlwaysApp/
   # Ensure no other AlwaysApp instances are running before launching
   ```

5. **Launch the Mac app (which starts both GUI and daemon via CLIService):**
   ```bash
   open -a AlwaysApp
   # Or from the app bundle:
   open -a AlwaysApp/AlwaysApp.app
   ```

**IMPORTANT:**
- **NEVER** run `./target/release/always start` directly — this bypasses CLIService environment variables
- **NEVER** reference `./target/release/always` in code — the daemon binary is embedded in the Mac app bundle at `AlwaysApp.app/Contents/MacOS/always`
- **NEVER** have parallel versions running — always stop old instances before launching new ones
- The Mac app launches the daemon through CLIService which passes environment variables (like GROQ_API_KEY)
- Both daemon and Mac app must be rebuilt and deployed together after code changes
- The build script automatically copies the daemon binary into the app bundle

## Verification

After launching, verify both processes are running:
```bash
ps aux | grep -v grep | grep -i always
```

Should show:
- `/Applications/AlwaysApp.app/Contents/MacOS/AlwaysApp` (GUI)  
- `/path/to/always run --lang en --timeout 30 --silence 0.4` (daemon)

Check status bar for Always icon and logs:
```bash
tail -f "/Users/livio/Library/Application Support/always/always.log"
```
