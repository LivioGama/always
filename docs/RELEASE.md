# Cutting a Release

End-to-end release flow for `always`. Releases are built and published by
CI when a version tag is pushed to `main`. Users install with a one-line
script, or download raw packages from the releases page. Disk images,
package-manager casks and Apple notarization are not part of the flow for
now.

## How users install

| OS | Command |
|----|---------|
| macOS / Linux | `curl -fsSL https://raw.githubusercontent.com/LivioGama/always/main/scripts/install-always.sh \| bash` |
| Windows | `irm https://raw.githubusercontent.com/LivioGama/always/main/scripts/install.ps1 \| iex` |

Both installers fetch the assets of the `releases/latest` GitHub Release,
so publishing a release is what ships an update. The website
(`docs/index.html`) picks the right command for the visitor's OS.

## Release assets

| Asset | Platform |
|-------|----------|
| `always-macos-arm64.tar.gz` | macOS, Apple Silicon |
| `always-linux-x86_64.tar.gz` / `.deb` / `.rpm` | Linux, x86_64 |
| `always-linux-aarch64.tar.gz` / `.deb` / `.rpm` | Linux, aarch64 |
| `always-windows-x86_64.zip` | Windows, x86_64 |
| `SHA256SUMS` | Checksums for every asset above |

## Required GitHub repository secrets

None are required to publish the assets above: the release job uses the
workflow's own `GITHUB_TOKEN`. Apple signing secrets and the Sparkle key
(see below) are only needed if app signing or auto-update is switched
back on.

CI coverage upload also uses `CODECOV_TOKEN`. It is intentionally not a
release preflight secret because the Codecov upload is non-blocking
(`fail_ci_if_error: false`).

## Release procedure

1. **Work on `develop`.** Land features and fixes there.

2. **Bump version**
   * Edit `Cargo.toml` `version`.
   * Move the `[Unreleased]` block in `CHANGELOG.md` under a new
     `## [vX.Y.Z] - YYYY-MM-DD` heading.
   * Commit on `develop`.

3. **Open a PR `develop` to `main`.** `main` is protected: the required CI
   checks must pass before it can be merged. Never push to `main` directly.

4. **Tag `main`** once the PR is merged:
   ```bash
   git checkout main && git pull
   git tag -s vX.Y.Z -m "always vX.Y.Z"
   git push origin vX.Y.Z
   ```
   Use a signed tag (`-s`) so the release traces back to a verifiable
   commit author.

5. **CI runs `release.yml` automatically.** The `vX.Y.Z` tag triggers it,
   and it builds and publishes the assets listed above (macOS arm64,
   Linux x86_64 and aarch64, Windows x86_64) plus `SHA256SUMS` to a
   GitHub Release.

6. **Verify the release.**
   ```bash
   # Download an asset and SHA256SUMS from the release, then:
   shasum -a 256 -c SHA256SUMS --ignore-missing

   # Smoke-test the installers against releases/latest
   curl -fsSL https://raw.githubusercontent.com/LivioGama/always/main/scripts/install-always.sh | bash
   ```
   On Windows: `irm https://raw.githubusercontent.com/LivioGama/always/main/scripts/install.ps1 | iex`.

   If the release is a draft, publish it. The installers read
   `releases/latest`, which only sees published, non-pre-release releases.

## Sparkle EdDSA key rotation

The Sparkle update feed is signed with an EdDSA private key kept in the
`SPARKLE_ED_PRIVATE_KEY` repo secret. Rotate by:

1. Generate a fresh keypair with `bin/sparkle-generate_keys` from the
   Sparkle distribution.
2. Update `SPARKLE_ED_PRIVATE_KEY` in repo secrets.
3. Update the `SUPublicEDKey` value in `Always/Info.plist`.
4. Cut a release; existing installs will refuse the new appcast (the
   stored public key won't match the new signature) and prompt the user
   to download the new release manually. Plan rotations alongside major
   releases.

## Local dry-run

The release workflow can be exercised locally with `act` or `nektos/act`,
but the most useful manual validation is:

```bash
# Run all local release-readiness gates (CI checks, docs, app bundle,
# and bundle smoke test).
./scripts/verify-release-readiness.sh local

# Check that required GitHub release secrets are present.
./scripts/verify-release-readiness.sh secrets

# Drive the existing build script with notarization disabled to catch
# pure build failures.
ALWAYS_BUILD_PROFILE=release ./Always/build.sh

# Build the Linux artifact:
docker build -t always:linux -f Dockerfile .
docker run --rm always:linux always --version
```

## Pulling a release

If you need to retract a release after publication:

1. Mark the GitHub Release as a **pre-release** rather than deleting.
   The installers read `releases/latest`, which skips pre-releases, so
   new installs fall back to the previous release.
2. Open a CHANGELOG entry under `[Unreleased]` documenting the
   retraction.

## crates.io status

`cargo publish` is intentionally not part of the release workflow yet.
The daemon depends on a `vad-rs` git revision that is newer than the
latest compatible crates.io release, and crates.io packages cannot
publish with git-only dependencies. Re-enable crates.io distribution only
after `cargo package --locked` succeeds from a clean checkout.
