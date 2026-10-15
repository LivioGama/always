#!/usr/bin/env bash
# Always -- installer entry point for macOS and Linux.
#
#   curl -fsSL https://raw.githubusercontent.com/LivioGama/always/main/scripts/install-always.sh | bash
#
# Thin wrapper: the installer is scripts/release/install.sh, published as the
# `install.sh` asset of every release. From a checkout this runs the sibling
# copy; piped from curl it fetches the asset of the latest release (or of
# ALWAYS_VERSION). All ALWAYS_* overrides are documented in that script.
set -euo pipefail

REPO="LivioGama/always"

here="${BASH_SOURCE[0]:-}"
if [[ -n "$here" && -f "$(dirname "$here")/release/install.sh" ]]; then
  exec bash "$(dirname "$here")/release/install.sh" "$@"
fi

if [[ -n "${ALWAYS_VERSION:-}" ]]; then
  tag="${ALWAYS_VERSION}"
  [[ "$tag" == v* ]] || tag="v${tag}"
  url="https://github.com/${REPO}/releases/download/${tag}/install.sh"
else
  url="https://github.com/${REPO}/releases/latest/download/install.sh"
fi

script="$(curl -fsSL "$url")" || {
  echo "ERROR could not download ${url}" >&2
  echo "      No release exists yet (or the version is wrong). See https://github.com/${REPO}/releases" >&2
  exit 1
}
exec bash -c "$script" install-always "$@"
