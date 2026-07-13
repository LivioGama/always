#!/usr/bin/env bash
#
# Generate Sparkle appcast.xml from a built+signed DMG.
#
# Invoked from .github/workflows/release.yml after the DMG is signed.
# The Sparkle generator consults the EdDSA private key referenced by
# SPARKLE_ED_PRIVATE_KEY_PATH (set in the workflow from a secret) and
# embeds the resulting signature inside the appcast `<enclosure>`.
#
# Required env:
#   ALWAYS_VERSION                  — semver, e.g. 1.0.0
#   ALWAYS_DMG_PATH                 — path to the signed DMG
#   ALWAYS_RELEASE_NOTES_URL        — URL to the GitHub release notes
#   SPARKLE_ED_PRIVATE_KEY_PATH     — path to the EdDSA private key file
#
# Output: appcast.xml in $PWD.

set -euo pipefail

: "${ALWAYS_VERSION:?must be set}"
: "${ALWAYS_DMG_PATH:?must be set}"
: "${ALWAYS_RELEASE_NOTES_URL:?must be set}"
: "${SPARKLE_ED_PRIVATE_KEY_PATH:?must be set}"

if ! command -v sign_update >/dev/null 2>&1; then
  echo "sign_update tool from Sparkle distribution not found on PATH" >&2
  echo "install via: brew install --cask sparkle  OR  use the Sparkle release tarball" >&2
  exit 1
fi

DMG_BASENAME="$(basename "$ALWAYS_DMG_PATH")"
DMG_SIZE_BYTES="$(stat -f %z "$ALWAYS_DMG_PATH")"
PUB_DATE="$(date -u +"%a, %d %b %Y %H:%M:%S +0000")"
SIGNATURE="$(sign_update -f "$SPARKLE_ED_PRIVATE_KEY_PATH" "$ALWAYS_DMG_PATH")"

cat > appcast.xml <<XML
<?xml version="1.0" encoding="utf-8"?>
<rss version="2.0" xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle">
  <channel>
    <title>Always — AppCast</title>
    <link>https://github.com/rtk-ai/always</link>
    <description>Latest signed releases of the Always macOS app.</description>
    <language>en</language>
    <item>
      <title>Always ${ALWAYS_VERSION}</title>
      <pubDate>${PUB_DATE}</pubDate>
      <sparkle:version>${ALWAYS_VERSION}</sparkle:version>
      <sparkle:shortVersionString>${ALWAYS_VERSION}</sparkle:shortVersionString>
      <sparkle:minimumSystemVersion>14.0</sparkle:minimumSystemVersion>
      <description><![CDATA[See full release notes at <a href="${ALWAYS_RELEASE_NOTES_URL}">${ALWAYS_RELEASE_NOTES_URL}</a>.]]></description>
      <enclosure
        url="https://github.com/rtk-ai/always/releases/download/v${ALWAYS_VERSION}/${DMG_BASENAME}"
        length="${DMG_SIZE_BYTES}"
        type="application/octet-stream"
        ${SIGNATURE} />
    </item>
  </channel>
</rss>
XML

echo "Wrote appcast.xml for version ${ALWAYS_VERSION}"
