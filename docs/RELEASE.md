# Cutting a Release

End-to-end release flow for the `always` daemon + Mac app. Most of this
runs in CI on tag push; a few prerequisites must exist as repository
secrets.

## Required GitHub repository secrets

| Secret | Purpose |
|--------|---------|
| `MAC_CERT_P12_BASE64` | Apple Developer ID Application cert exported as `.p12`, base64-encoded. |
| `MAC_CERT_PASSWORD` | Password used when exporting the `.p12`. |
| `KEYCHAIN_PASSWORD` | Random string used to lock the temporary CI keychain. |
| `APPLE_ID` | Apple ID email for notarization. |
| `APPLE_TEAM_ID` | Apple Developer Team ID (e.g. `ZV4JCJ669Y`). |
| `APPLE_APP_SPECIFIC_PASSWORD` | App-specific password from appleid.apple.com. |
| `HOMEBREW_TAP_TOKEN` | Fine-grained PAT with `contents:write` on `rtk-ai/homebrew-tap`. |
| `CODECOV_TOKEN` | Codecov upload token (CI coverage). |

## Release procedure

1. **Bump version**
   * Edit `Cargo.toml` `version`.
   * Move the `[Unreleased]` block in `CHANGELOG.md` under a new
     `## [vX.Y.Z] - YYYY-MM-DD` heading.
   * Commit on `develop`, open PR to `main`, merge.

2. **Tag**
   ```bash
   git tag -s vX.Y.Z -m "always vX.Y.Z"
   git push origin vX.Y.Z
   ```
   Use a signed tag (`-s`) so the release artifacts trace back to a
   verifiable commit author.

3. **CI runs `release.yml` automatically.** It:
   * Builds a universal-binary daemon (`aarch64` + `x86_64` via `lipo`).
   * Codesigns + notarizes the Swift app via `Always/build.sh`.
   * Builds a DMG with a `/Applications` symlink.
   * Generates a CycloneDX SBOM.
   * Signs the DMG with `cosign` (keyless OIDC).
   * Builds + tarballs the Linux CLI daemon.
   * Computes `SHA256SUMS`.
   * Drafts a GitHub Release with all artifacts.
   * Opens an auto-PR to `rtk-ai/homebrew-tap` to bump the formula.
   * Generates SLSA Level 3 provenance via the
     `slsa-framework/slsa-github-generator` reusable workflow.

4. **Verify the draft release** before publishing:
   ```bash
   # Cosign verification (replace with the real cert identity)
   cosign verify-blob \
     --certificate-identity 'https://github.com/rtk-ai/always/.github/workflows/release.yml@refs/tags/vX.Y.Z' \
     --certificate-oidc-issuer 'https://token.actions.githubusercontent.com' \
     --signature always-X.Y.Z.dmg.sig \
     --certificate always-X.Y.Z.dmg.pem \
     always-X.Y.Z.dmg

   # SLSA provenance
   slsa-verifier verify-artifact \
     --provenance-path always-X.Y.Z.intoto.jsonl \
     --source-uri github.com/rtk-ai/always \
     --source-tag vX.Y.Z \
     always-X.Y.Z.dmg
   ```

5. **Publish the GitHub Release.** This makes the appcast.xml visible
   to Sparkle clients; existing installations will see an update prompt
   on next launch (or after `feedURL` poll interval).

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
# Drive the existing build script with notarization disabled to catch
# pure build failures.
ALWAYS_BUILD_PROFILE=release ./Always/build.sh

# Build the Linux artifact:
docker build -t always:linux -f Dockerfile .
docker run --rm always:linux always --version
```

## Notarization timeouts

`xcrun notarytool submit --wait --timeout 30m` is used in the workflow.
If notarization stalls, the failure is recoverable:

1. Find the submission UUID in the workflow log.
2. Run `xcrun notarytool log <uuid> --apple-id ... --team-id ...` for
   diagnostic JSON.
3. Re-run the failed job from the GitHub Actions UI. The DMG + signing
   are deterministic so the second run produces a byte-identical artifact.

## Pulling a release

If you need to retract a release after publication:

1. Mark the GitHub Release as a **pre-release** rather than deleting —
   users who already downloaded the DMG would otherwise still trust the
   matching cosign signature. Pre-releases are not picked up by Sparkle
   by default.
2. Open a CHANGELOG entry under `[Unreleased]` documenting the
   retraction.

## crates.io status

`cargo publish` is intentionally not part of the release workflow yet.
The daemon depends on a `vad-rs` git revision that is newer than the
latest compatible crates.io release, and crates.io packages cannot
publish with git-only dependencies. Re-enable crates.io distribution only
after `cargo package --locked` succeeds from a clean checkout.
