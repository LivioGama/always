# Cutting a Release

How `always` is released and what a release contains. The procedure itself is
owned by the **release skill**: [`.agents/skills/release/SKILL.md`](../.agents/skills/release/SKILL.md)
(rule: [`.agents/rules/release.md`](../.agents/rules/release.md)). This page
is the reference for the pipeline and the assets; do not improvise the steps
from it.

Users install with a one-line script, or download raw packages from the
releases page. There are no disk images, no Homebrew cask, no Apple
notarization and no MSI: the macOS app is ad-hoc signed, and Windows ships as
a `.zip`.

## How users install

| OS | Command |
|----|---------|
| macOS / Linux | `curl -fsSL https://raw.githubusercontent.com/LivioGama/always/main/scripts/install-always.sh \| bash` |
| Windows | `irm https://raw.githubusercontent.com/LivioGama/always/main/scripts/install.ps1 \| iex` |

`scripts/install-always.sh` and `scripts/install.ps1` are thin wrappers that
fetch `install.sh` / `install.ps1` from the latest release. Those two scripts
(sources: `scripts/release/install.sh`, `scripts/release/install.ps1`) are
published as release assets, resolve `releases/latest`, download the
`always-<tag>-<target>.*` asset and its `.sha256`, verify the checksum, then
install. Publishing a release is what ships an update. The website
(`docs/index.html`) picks the right command for the visitor's OS.

## Release assets

Names are a stable contract shared by the installers and the website:
`always-<tag>-<target>.{tar.gz,deb,rpm,zip}`, each with a `.sha256` sibling
(for example `always-v0.1.0-aarch64-apple-darwin.tar.gz` and
`always-v0.1.0-aarch64-apple-darwin.tar.gz.sha256`).

| Target | Assets |
|--------|--------|
| `aarch64-apple-darwin` | `.tar.gz` (contains `Always.app`, ad-hoc signed) |
| `x86_64-unknown-linux-gnu` | `.tar.gz`, `.deb`, `.rpm` |
| `aarch64-unknown-linux-gnu` | `.tar.gz`, `.deb`, `.rpm` |
| `x86_64-pc-windows-msvc` | `.zip` |

Also published: `install.sh`, `install.ps1` and `always-<tag>.intoto.jsonl`
(the SLSA build-provenance bundle, checkable with `gh attestation verify`).
The `.deb` and `.rpm` install the systemd **user** unit
`scripts/release/always-daemon.service` (runs `always run`) into
`/usr/lib/systemd/user/`.

## Branches and flow

1. Work lands on `develop`. User-visible changes carry a changelog fragment in
   `.github/changelog.d/` (never bullets under `## [Unreleased]`).
2. `.agents/skills/release/prepare.sh x.y.z` on a `release-x.y.z` branch cut
   from `develop`: one version across every manifest, fragments folded into
   `CHANGELOG.md`, lockfiles, `scripts/release/check-release.sh`.
3. Pull request `release-x.y.z` into `main`. `main` is protected; merge with a
   **merge commit** (never squash or rebase).
4. Tag `vX.Y.Z` on that merge commit and push it. The tag triggers
   `release.yml`.
5. Back-merge `main` into `develop`.

## Pipeline (`.github/workflows/release.yml`)

| Job | What it does |
|-----|--------------|
| `verify` | Tag is on `main`; `check-release.sh` (tag equals every version file, CHANGELOG heading present, lockfiles fresh); `cargo deny check` |
| `test` | The `ci.yml` test suite on macOS |
| `assets` | Calls `release-build.yml`: builds macOS arm64, Linux x86_64 and aarch64, Windows x86_64; writes a `.sha256` per asset; attests build provenance; stages the `release-assets` artifact |
| `release` | Publishes a non-draft GitHub Release; body is the CHANGELOG section |
| `smoke` | Installs the published release with the real installers on Ubuntu (deb, tar.gz), Fedora (rpm), macOS arm64 and Windows, and runs `always --version` |

`workflow_dispatch` of `release.yml` on a **branch** is a dry run: it checks
and builds, smoke-tests the built assets through a local `file://` URL, and
publishes nothing (assets stay in the `release-assets` workflow artifact):

```bash
gh workflow run release.yml --ref release-x.y.z
```

## Secrets

None. The workflows use only the built-in `GITHUB_TOKEN` (plus OIDC for the
provenance attestation). There is no Apple signing identity, notarization
credential, Homebrew tap token or update-feed key.

CI coverage upload uses `CODECOV_TOKEN`; it is not a release prerequisite
(`fail_ci_if_error: false`).

## Verifying a release

```bash
V=vX.Y.Z; D=$(mktemp -d); cd "$D"
gh release download "$V" --repo LivioGama/always
shasum -a 256 -c ./*.sha256
gh attestation verify always-$V-aarch64-apple-darwin.tar.gz --repo LivioGama/always
```

The release skill lists the full verification (every asset, installers,
provenance). A browser-downloaded macOS archive carries the quarantine flag;
clear it with `xattr -dr com.apple.quarantine Always.app` (the installer does
not need this).

## Local checks

```bash
# Version and changelog consistency, as the verify job runs it
scripts/release/check-release.sh --report

# Local gates (fmt, clippy, tests, audit, deny, docs, Linux build, macOS app)
./scripts/verify-release-readiness.sh local

# Build the Linux artifact in Docker
docker build -t always:linux -f Dockerfile .
docker run --rm always:linux always --version
```

## Pulling a release

If you need to retract a release after publication:

1. Mark the GitHub Release as a **pre-release** rather than deleting it. The
   installers read `releases/latest`, which skips pre-releases, so new
   installs fall back to the previous release.
2. Record the retraction in a changelog fragment.
3. Fix forward with the next patch release; published assets are not replaced.

## crates.io status

`cargo publish` is intentionally not part of the release workflow yet.
The daemon depends on a `vad-rs` git revision that is newer than the
latest compatible crates.io release, and crates.io packages cannot
publish with git-only dependencies. Re-enable crates.io distribution only
after `cargo package --locked` succeeds from a clean checkout.
