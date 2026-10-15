# Releasing

Always loaded: the only sanctioned release path.

- **To release, run the `release` skill** (`.agents/skills/release/SKILL.md`).
  It owns the whole flow: version pick, `prepare.sh` (changelog cut, one
  version across every manifest, lockfiles, `check-release.sh`) on a
  `release-x.y.z` branch cut from `develop`, a pull request into `main` merged
  as a **merge commit**, then the `vX.Y.Z` tag on that merge commit, which
  triggers `release.yml`.
- **Branches.** Work lands on `develop`. `main` is protected and deploys; it
  receives changes only through develop -> main release PRs. Never squash or
  rebase that PR (develop/main ancestry must stay intact), and back-merge main
  into develop after the tag.
- **User-visible changes need a changelog fragment**:
  `.github/changelog.d/<slug>.<added|changed|deprecated|removed|fixed|security>.md`
  (see that directory's README). Never write bullets under `## [Unreleased]` in
  CHANGELOG.md; `prepare.sh` and `check-release.sh` refuse it.
- A request to "ship", "tag", "publish" or "release" a version, or a failed
  Release workflow run, is the `release` skill's trigger. Do not improvise the
  steps.
- **Keep the validated candidate exact.** Record BASE, MAIN_BASE and
  PREPARE_HEAD; run the skill's `check-candidate.py` before merge and before tag.
  Base drift means renewed changelog coverage, never a BEHIND bypass.
- **Never** push a tag, delete or move a tag, or merge into `main` without the
  user's explicit release ask; never bypass branch protection.
- No notarization and no Homebrew cask (user decision). `workflow_dispatch` of
  `release.yml` on a branch is a dry run that publishes nothing.
