#!/bin/bash
# Build the echo-cancelling capture helper (drop-in replacement for `rec`).
# Output: capture/always-capture. Bundled by Always/build.sh into
# Contents/MacOS/ and signed with the app (stable identity, no TCC churn).
set -euo pipefail
cd "$(dirname "$0")"
swiftc -O -target arm64-apple-macos14.0 -o always-capture always-capture.swift
echo "✓ built $(pwd)/always-capture"
