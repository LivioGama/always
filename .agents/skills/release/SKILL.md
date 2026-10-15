---
name: release
description: Cut an Always release end to end — pick the version, prepare the release commit (changelog fragments folded into CHANGELOG.md, one version across Cargo.toml, crates/always_tauri, src-tauri, tauri.conf.json, always-gui/package.json and Always/Info.plist, Cargo.lock) with prepare.sh on a release-x.y.z branch cut from develop, merge it into main as a MERGE COMMIT, tag vX.Y.Z on that merge commit to trigger .github/workflows/release.yml, verify the published release and installers with evidence, then back-merge main into develop. Also covers resuming an interrupted release and recovering from a failed Release run. Use when asked to release, publish, tag or ship a new Always version, bump the version, cut the changelog, check a published release, or when the Release workflow failed.
---

# Releasing Always

Branch model (differs from Pixel's single-branch model):

- **`develop`** is where work lands. Pull requests target it.
- **`main`** is protected and deploys. It receives changes only through a
  `release-x.y.z` pull request cut from `develop`, merged as a **merge commit**
  (never squash or rebase: develop/main ancestry must stay intact).
- A release is the tag **`vX.Y.Z` on main's merge commit**. Pushing the tag runs
  `.github/workflows/release.yml`. Ordinary CI does not run on tags.
- After the tag, **`main` is back-merged into `develop`** (fast-forward when
  possible), so the tag is an ancestor of develop and the next
  `prepare.sh` inventory is exact.

## What the tag does

`verify` -> `assets` -> `release` -> `smoke` (`release.yml`; `assets` calls
`release-build.yml`).

| Job | Does | A failure means |
| --- | --- | --- |
| `verify` | tag is on main; `scripts/release/check-release.sh <tag>` (tag == every version file, CHANGELOG heading, empty Unreleased, fresh lockfiles); `cargo deny check`; locked tests | nothing built or published |
| `assets` | builds macOS arm64 (`Always/build.sh` release, ad-hoc signed), Linux x86_64 and aarch64 (glibc: tar.gz, deb, rpm), Windows x86_64 (zip); `.sha256` per asset; `actions/attest` build provenance; stages `release-assets` | nothing published |
| `release` | publishes a NON-draft release; body = the CHANGELOG section; uploads archives, packages, `.sha256`, `install.sh`, `install.ps1`, `always-<tag>.intoto.jsonl` | publication may already be immutable |
| `smoke` (x7) | verifies provenance, then runs the installer from the published release on Ubuntu (deb, tar), Fedora (rpm), macOS arm64, Windows; each prints `always --version` | already published: distinguish code from infra |

Asset names (a public contract; the installers and website use them):
`always-<tag>-<target>.{tar.gz,deb,rpm,zip}` + `.sha256` sibling, targets
`aarch64-apple-darwin`, `x86_64-unknown-linux-gnu`,
`aarch64-unknown-linux-gnu`, `x86_64-pc-windows-msvc`.

Facts that shape everything:

- **No notarization, no Homebrew.** The macOS app is ad-hoc signed; curl does
  not quarantine, so the installer-delivered app opens. A browser-downloaded
  archive needs `xattr -dr com.apple.quarantine`.
- **`workflow_dispatch` on a branch is a dry run**: verify (version/changelog
  checks reported, not enforced), assets, then smoke against the built assets via
  `file://`. No release is created. Run it on the release branch before merging:
  `gh workflow run release.yml --ref release-x.y.z`.
- **A published release is immutable-ish**: an asset fix ships as the next
  patch. Title and notes stay editable (`gh release edit --notes-file`).
- **Users install releases, not `main`.** `install.sh` / `install.ps1` are
  release assets; `scripts/install-always.sh` and `scripts/install.ps1` on main
  are thin wrappers that fetch them.

## Authority

A release starts on the user's explicit ask ("release", "cut 0.1.0"); that ask
covers the whole happy path: pick version, prepare, open the PR, merge it once
green, tag its merge commit once its push CI is green, verify, back-merge. Say
each outward command (merge, tag push) in a progress line just before running
it. An ask that limits the scope ("prepare 0.1.0", "don't tag yet") stops there.

Stop and ask when: a precondition fails, the version is not clear-cut, a gate
or PR check is red and not provably local, `main`'s CI is red, the merge commit
is not the prepare PR's, or any Release run fails (classify first, see
Recovery). Always wait for their own go: deleting or moving a tag,
force-pushing, retagging, deleting a release or its assets. `main` is
protected: if the client refuses the merge, hand the exact SHA, checks and
command to the maintainer; never bypass protection.

## The release record

Keep `release-x.y.z.md` in the session scratchpad, updated after every step:
version and reason; BASE (develop SHA), MAIN_BASE (main SHA), PREPARE_HEAD,
MERGE SHA; validation (SHA, command, exit); PR number; tag and run URL; smoke
results; failure class; next action. On resume re-read live state (`gh pr view`,
`gh run view`, `git ls-remote origin refs/tags/vX.Y.Z`) before acting.

## 1. Preconditions

```bash
git fetch origin --tags
git status --short                                   # clean
LAST=$(git tag --list 'v[0-9]*' --sort=-v:refname | head -n 1)
git merge-base --is-ancestor "$LAST" origin/develop && echo "$LAST is on develop"
git log --oneline "$LAST"..origin/develop | head     # something to release
gh run list --branch develop --workflow ci.yml --status completed -L 1    # success
gh run list --branch main --workflow ci.yml --status completed -L 1       # success
git merge-base --is-ancestor origin/main origin/develop && echo "main has nothing develop lacks"
```

If `main` has commits `develop` lacks (a hotfix), back-merge main into develop
first; `check-candidate.py` refuses otherwise. A red `cargo deny check` is a
`chore(deps)` PR into develop before the cut. Run the dry run early.

## 2. Pick the version

Pre-1.0: **patch** for fixes and additions; **minor** when a user of the
previous minor must act (removed command or flag, changed on-disk format,
changed config key). Check that what a `Removed`/`Changed` fragment breaks
actually shipped in the last tag (`git cat-file -e v<last>:<path>`). State the
version with its one-line reason and go on; stop only on the cases above.

Optionally write `.github/changelog.d/_highlights.md` on the release branch: a
lead paragraph then a `### Highlights` list of 2 or 3 bullets (`###` and
below, 2000 bytes). It tops the released section and the release body.

## 3. Prepare the release commit

```bash
git fetch origin
BASE=$(git rev-parse origin/develop)
MAIN_BASE=$(git rev-parse origin/main)
git switch -c release-x.y.z origin/develop
.agents/skills/release/prepare.sh x.y.z        # --date YYYY-MM-DD to override today
```

`prepare.sh` refuses before writing when the tag or the `## [x.y.z]` heading
exists, no fragment is present, a fragment is malformed (needs
`**scope:** ` opening, <= 900 bytes, a PR link or a number-led slug), or
`## [Unreleased]` carries a bullet. Otherwise it: sets the version in lockstep in
all six version files; folds the fragments into `## [x.y.z] - DATE` under an
empty `## [Unreleased]` and deletes them; runs `cargo update --workspace` for
the root, `crates/always_tauri` and `src-tauri` lockfiles; lists PRs merged into
develop since the last tag and commits in no PR; runs
`scripts/release/check-release.sh vx.y.z`; and checks the diff is prepare-only
(`scripts/release/release-prepare-only.py HEAD`). It must end with
`release-prepare-only: the diff is what prepare.sh writes`.

Then, before committing:

- **Changelog completeness.** Every `feat`/`fix`/`perf` PR and anything
  user-visible needs an entry; the run lists the PRs next to the fragments it
  released. Write a missing one straight into the new `## [x.y.z]` section in
  the same commit (re-running would refuse the heading).
- **Release body.** Read the new section as a stranger: it is published verbatim.
- **SPEC.md.** The repo's SPEC.md is normative; a behaviour change in the
  release must already be in it (it should have landed with the change).
- **Gates.** `cargo fmt --all -- --check`, `cargo clippy --all-targets --locked -- -D warnings`,
  `cargo test --locked --bins --lib --tests -- --skip macos_overlay` (what ci.yml runs).
- **Local app check.** Per CLAUDE.md, a change is not done until the new build is
  running: `scripts/dev-rebuild.sh` before declaring a release candidate good.

```bash
PREPARE_HEAD=$(git rev-parse HEAD)   # after the commit below
git commit -am "release: prepare x.y.z"
git push -u origin release-x.y.z
gh workflow run release.yml --ref release-x.y.z          # dry run: assets + installer smoke
gh pr create --base main --title "release: prepare x.y.z" --body-file <body>
```

Body: the version, patch/minor reason, gate output, the dry-run URL, "tag
`vx.y.z` follows on this PR's merge commit". Record `PREPARE_HEAD=$(git rev-parse HEAD)`
after the final commit.

Wait for the dry run and the PR checks (`gh pr checks <n> --watch --interval 30`
in the background; a watcher finishing is not proof every workflow reported).
Immediately before merging:

```bash
git fetch origin
python3 .agents/skills/release/check-candidate.py "$BASE" "$PREPARE_HEAD" --main "$MAIN_BASE" --tip origin/main
gh pr merge <n> --merge                  # MERGE COMMIT. Not --squash, not --rebase.
```

If main moved or GitHub reports BEHIND, refresh coverage and validation; never
bypass a stale candidate.

## 4. Tag

Tag the prepare PR's merge commit, not whatever main's head is by then. Wait for
that commit's push CI on main to be green.

```bash
git fetch origin
SHA=$(gh pr view <n> --json mergeCommit --jq .mergeCommit.oid)
python3 .agents/skills/release/check-candidate.py "$BASE" "$PREPARE_HEAD" --main "$MAIN_BASE" --tip origin/main --merge "$SHA"
gh run list --branch main --commit "$SHA"                   # CI: success
git tag -a vx.y.z -m "Always x.y.z" "$SHA"                  # annotated
git push origin vx.y.z                                      # the release ask covers it
gh run list --workflow release.yml -L 1                     # the run for vx.y.z
gh run watch <run-id> --exit-status                         # in the background
```

Pass run ids literally (zsh does not word-split unquoted `$var`).

## 5. Back-merge and verify

```bash
git fetch origin
git switch develop && git merge --ff-only origin/main || git merge origin/main   # tag becomes an ancestor of develop
git push origin develop
```

Then verify from the published state, into a fresh directory:

```bash
V=vx.y.z; D=$(mktemp -d); cd "$D"
gh release view $V --repo LivioGama/always --json isDraft,isPrerelease,body --jq '{isDraft, isPrerelease, body: .body[0:200]}'
gh release download $V --repo LivioGama/always
ls                                   # 8 archives/packages, 8 .sha256, install.sh, install.ps1, intoto.jsonl
shasum -a 256 -c ./*.sha256
for f in always-v*.tar.gz always-v*.zip always-v*.deb always-v*.rpm install.sh install.ps1; do
  gh attestation verify "$f" --repo LivioGama/always --signer-workflow LivioGama/always/.github/workflows/release-build.yml || exit "$?"
done
git -C <repo> show "${V}:scripts/release/install.sh" | diff - install.sh && echo "install.sh == tag's"
curl -fsSL https://raw.githubusercontent.com/LivioGama/always/main/scripts/install-always.sh | ALWAYS_NO_LAUNCH=1 ALWAYS_APP_DIR="$D/Apps" bash   # the website one-liner
```

Report: `Release vx.y.z: published and verified | NOT verified`, run URL, per-job
result, asset count and names, attestation, install one-liner results per OS,
the back-merge SHA, caveats (what was not checked and why). The macOS app is
first run on a real Mac by the maintainer: say so.

## Recovery

Classify from `gh run view <run-id> --log-failed`: **infra** (runner lost,
timeout, 5xx: `gh run rerun <run-id> --failed` once), **tooling** (an action or
toolchain changed: a rerun replays the same workflow file, fix on develop and
re-release), **code** (check-release or a test fails, a target does not build).

| Failed in | State | Do |
| --- | --- | --- |
| `verify` or `assets` | tag pushed, nothing published | with the user's go, delete the tag and re-tag the fixed commit (a fix reaches main through a new develop->main release PR) |
| `release` before publish | nothing published | as above, or rerun for infra |
| `smoke`, installer only | binaries fine, published script broken | the script ships with the release: fix on develop, release the next patch |
| `smoke`, infra | unknown | `gh run rerun <run-id> --failed` |
| `smoke`, code, or a later bad-binary report | users may have it | never move the tag: fix on develop, release `x.y.z+1`, say in its changelog what was wrong |
