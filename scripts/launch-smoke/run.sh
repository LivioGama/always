#!/bin/bash
# Pre-promote launch smoke test. Builds nothing: point it at a built bundle.
#   scripts/launch-smoke/run.sh <path/to/Always.app> [secs]
# Runs the bundle's daemon + helpers as an isolated sandbox instance
# (ALWAYS_INSTANCE=smoke, ALWAYS_NO_PASTE=1, own socket/state/logs), master-
# pauses production for the window and always resumes it. Exit 0 = pass.
set -u
D="$(cd "$(dirname "$0")" && pwd)"
"$D/smoke.sh" "$1" "${2:-30}" || exit 1
python3 -I "$D/check.py"
