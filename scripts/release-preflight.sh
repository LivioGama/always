#!/usr/bin/env bash
set -euo pipefail

required=(
  MAC_CERT_P12_BASE64
  MAC_CERT_PASSWORD
  KEYCHAIN_PASSWORD
  APPLE_ID
  APPLE_TEAM_ID
  APPLE_APP_SPECIFIC_PASSWORD
  HOMEBREW_TAP_TOKEN
  SPARKLE_ED_PRIVATE_KEY
)

missing=()
for name in "${required[@]}"; do
  if [ -z "${!name:-}" ]; then
    missing+=("$name")
  fi
done

if [ "${#missing[@]}" -gt 0 ]; then
  printf 'Missing required release secret(s):\n' >&2
  printf '  - %s\n' "${missing[@]}" >&2
  printf '\nSet them in GitHub repository secrets before cutting a release.\n' >&2
  exit 1
fi

if ! printf '%s' "$MAC_CERT_P12_BASE64" | base64 --decode >/dev/null 2>&1; then
  printf 'MAC_CERT_P12_BASE64 is not valid base64.\n' >&2
  exit 1
fi

if ! printf '%s' "$APPLE_TEAM_ID" | grep -Eq '^[A-Z0-9]{10}$'; then
  printf 'APPLE_TEAM_ID should look like a 10-character Apple team id.\n' >&2
  exit 1
fi

if ! printf '%s' "$SPARKLE_ED_PRIVATE_KEY" | grep -Eq '.{20,}'; then
  printf 'SPARKLE_ED_PRIVATE_KEY is unexpectedly short.\n' >&2
  exit 1
fi

printf 'Release secrets preflight passed.\n'
