<!-- gitnexus:start -->
# GitNexus — Code Intelligence

This project is indexed by GitNexus as **always** (847 symbols, 1793 relationships, 70 execution flows). Use the GitNexus MCP tools to understand code, assess impact, and navigate safely.

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

**CRITICAL:** To rebuild and launch the Always app (both daemon and Mac status bar app):

1. **Kill existing processes:**
   ```bash
   pkill -f "AlwaysApp" || true
   pkill -f "always" || true
   ```

2. **Build everything (daemon + Mac app):**
   ```bash
   cargo build --release
   ```

3. **Launch the Mac app (which starts both GUI and daemon):**
   ```bash
   open -a AlwaysApp
   ```

**NEVER** run `./target/release/always start` directly — this only starts the daemon without the Mac status bar interface. Always use `open -a AlwaysApp` to launch the complete system.

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
