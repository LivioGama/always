# Always Project Rules

## Opening pull requests

PRs in this repo are opened by `pr-gate`, which reviews and fixes the branch,
runs the real CI on a merge-preview branch, then opens the PR with a signature
marker. `.github/workflows/pr-gate-guard.yml` closes PRs without a valid one.

- Never run `gh pr create` or the GitHub MCP `create_pull_request` directly.
  Run `pr-gate submit` (the `gh` shim does this automatically).
- Release PRs (`release-x.y.z` -> `main`) are exempt; the `release` skill opens
  them directly.
- Maintainers can override with the `pr-gate-bypass` label (then reopen).

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
| Self-update (daemon `always update`) | on | off |

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

