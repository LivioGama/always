#!/bin/bash
set -e

APP_DIR="$(cd "$(dirname "$0")" && pwd)"

echo "Building Always..."
cd "$APP_DIR"
swift build

echo "Creating app bundle..."
mkdir -p Always.app/Contents/MacOS
mkdir -p Always.app/Contents/Resources

# Debug: check if executable exists
if [ -f .build/debug/Always ]; then
    echo "✓ Executable found at .build/debug/Always"
else
    echo "✗ Executable NOT found at .build/debug/Always"
    echo "Trying direct path..."
    if [ -f .build/arm64-apple-macosx/debug/Always ]; then
        echo "✓ Found at .build/arm64-apple-macosx/debug/Always"
    fi
fi

# Always update bundle contents
cp Info.plist Always.app/Contents/
cp .build/debug/Always Always.app/Contents/MacOS/
cp Resources/AlwaysIcon.icns Always.app/Contents/Resources/

# Copy daemon binary into app bundle.
# Pick newest of release/debug so local dev (debug build) keeps showing
# transcripts in logs (privacy gate is auto-on in debug builds).
# Override with ALWAYS_BUILD_PROFILE=release|debug to pin a specific profile.
RELEASE_BIN="../target/release/always"
DEBUG_BIN="../target/debug/always"
DAEMON_PATH=""
case "${ALWAYS_BUILD_PROFILE:-}" in
    release) DAEMON_PATH="$RELEASE_BIN" ;;
    debug)   DAEMON_PATH="$DEBUG_BIN" ;;
    *)
        if [ -f "$RELEASE_BIN" ] && [ -f "$DEBUG_BIN" ]; then
            if [ "$DEBUG_BIN" -nt "$RELEASE_BIN" ]; then
                DAEMON_PATH="$DEBUG_BIN"
            else
                DAEMON_PATH="$RELEASE_BIN"
            fi
        elif [ -f "$DEBUG_BIN" ]; then
            DAEMON_PATH="$DEBUG_BIN"
        else
            DAEMON_PATH="$RELEASE_BIN"
        fi
        ;;
esac

if [ -f "$DAEMON_PATH" ]; then
    # CRITICAL: target name MUST differ from the GUI binary in case-insensitive
    # ways. macOS APFS defaults to case-insensitive, so `MacOS/Always` (GUI) and
    # `MacOS/always` (daemon) resolve to the same inode and the second `cp`
    # silently overwrites the first. That is exactly what broke the daemon
    # spawn after the c55cc4a merge — the bundled "Always" binary was actually
    # the daemon (or vice-versa) and the Mac app could not start it. Use a
    # distinct file name and read it back through CLIService accordingly.
    echo "Copying daemon binary to app bundle ($DAEMON_PATH → MacOS/always-daemon)..."
    mkdir -p Always.app/Contents/MacOS
    cp "$DAEMON_PATH" Always.app/Contents/MacOS/always-daemon
    echo "✓ Daemon binary copied"
else
    # Bundle without the daemon is unshippable — the GUI cannot spawn it.
    # Previously we warned and continued, which deployed a broken bundle
    # to /Applications. Fail loudly instead.
    echo "✗ Daemon binary not found at $DAEMON_PATH"
    echo "  Build the daemon first: cd .. && cargo build --bin always (or --release)"
    exit 1
fi

# Bundle Sparkle.framework. Swift Package Manager downloads it as part of
# the Sparkle xcframework; we ship the macos-arm64_x86_64 slice. Without
# this step the app crashes at launch with a dyld "Library not loaded:
# @rpath/Sparkle.framework" error because @rpath resolves under
# Contents/Frameworks/ in a real bundle.
SPARKLE_SRC=".build/artifacts/sparkle/Sparkle/Sparkle.xcframework/macos-arm64_x86_64/Sparkle.framework"
if [ -d "$SPARKLE_SRC" ]; then
    echo "Bundling Sparkle.framework..."
    mkdir -p Always.app/Contents/Frameworks
    rm -rf Always.app/Contents/Frameworks/Sparkle.framework
    cp -R "$SPARKLE_SRC" Always.app/Contents/Frameworks/Sparkle.framework
    # `swift build` does NOT add @executable_path/../Frameworks to LC_RPATH.
    # Without this the dyld lookup at launch fails because the framework
    # only resolves at the standard bundle search path.
    if ! otool -l Always.app/Contents/MacOS/Always \
            | grep -A2 LC_RPATH | grep -q "@executable_path/../Frameworks"; then
        install_name_tool -add_rpath "@executable_path/../Frameworks" \
            Always.app/Contents/MacOS/Always
        echo "✓ Added @executable_path/../Frameworks rpath"
    fi
    echo "✓ Sparkle.framework copied"
else
    echo "⚠️  Sparkle.framework not found at $SPARKLE_SRC — auto-update will not work"
fi

echo "Code signing app..."
# Use stable bundle identifier for permissions persistence
SIGN_IDENTITY="${ALWAYS_CODESIGN_IDENTITY:-}"
if [ -z "$SIGN_IDENTITY" ]; then
    SIGN_IDENTITY="$(security find-identity -v -p codesigning 2>/dev/null | sed -n 's/.*"\(Apple Development: [^"]*\)".*/\1/p' | head -1)"
fi
if [ -z "$SIGN_IDENTITY" ]; then
    SIGN_IDENTITY="-"
fi
echo "Using signing identity: ${SIGN_IDENTITY}"
codesign --force --deep --sign "$SIGN_IDENTITY" --identifier "com.always" --entitlements Always.entitlements Always.app

# Notarization (only if using a proper Apple Developer identity, not ad-hoc).
# All three env vars are required to authenticate notarytool:
#   ALWAYS_NOTARIZE_TEAM_ID — Apple Team ID (e.g. ZV4JCJ669Y).
#   ALWAYS_NOTARIZE_APPLE_ID — developer Apple ID email.
#   ALWAYS_NOTARIZE_APP_PWD — app-specific password (NOT the AppleID pwd).
# The previous version hardcoded --apple-id "com.always" (the bundle ID,
# not an Apple ID) and never read the app-specific password, so the
# notarytool call would silently fail with an authentication error and
# the script would still report "success" thanks to a `|| echo ""` on
# the JSON parse. Catch that explicitly now.
if [ "$SIGN_IDENTITY" != "-" ] \
        && [ -n "${ALWAYS_NOTARIZE_TEAM_ID:-}" ] \
        && [ -n "${ALWAYS_NOTARIZE_APPLE_ID:-}" ] \
        && [ -n "${ALWAYS_NOTARIZE_APP_PWD:-}" ]; then
    echo "Notarizing app..."

    # Pre-flight: Info.plist must not still carry the Sparkle EdDSA
    # placeholder. Shipping a release with the placeholder bricks
    # auto-update (Sparkle silently refuses to verify the appcast).
    if /usr/libexec/PlistBuddy -c "Print :SUPublicEDKey" \
            Always.app/Contents/Info.plist 2>/dev/null \
            | grep -q "REPLACE_WITH_BASE64_EDDSA_PUBLIC_KEY"; then
        echo "✗ Info.plist still contains the Sparkle SUPublicEDKey placeholder."
        echo "  Replace with the real EdDSA public key before publishing — see docs/RELEASE.md."
        exit 1
    fi

    ZIP_PATH="Always.zip"
    ditto -c -k --keepParent "Always.app" "$ZIP_PATH"

    # Submit for notarization. `--wait --timeout 30m` blocks until Apple's
    # service reports a terminal state instead of hanging indefinitely.
    NOTARIZATION_OUTPUT=$(xcrun notarytool submit "$ZIP_PATH" \
        --team-id "$ALWAYS_NOTARIZE_TEAM_ID" \
        --apple-id "$ALWAYS_NOTARIZE_APPLE_ID" \
        --password "$ALWAYS_NOTARIZE_APP_PWD" \
        --wait \
        --timeout 30m \
        --output-format json)

    # Parse the submission JSON. python3's json.load with check ensures
    # we hard-fail on a malformed/empty response — previously a `|| echo ""`
    # let the script continue with an empty ID and skip stapling silently.
    NOTARIZATION_ID=$(printf '%s' "$NOTARIZATION_OUTPUT" \
        | python3 -c "import sys, json; print(json.load(sys.stdin)['id'])")
    NOTARIZATION_STATUS=$(printf '%s' "$NOTARIZATION_OUTPUT" \
        | python3 -c "import sys, json; print(json.load(sys.stdin).get('status', 'unknown'))")

    if [ "$NOTARIZATION_STATUS" != "Accepted" ]; then
        echo "✗ Notarization failed: status=$NOTARIZATION_STATUS id=$NOTARIZATION_ID"
        echo "  Fetch the log: xcrun notarytool log $NOTARIZATION_ID --team-id $ALWAYS_NOTARIZE_TEAM_ID --apple-id $ALWAYS_NOTARIZE_APPLE_ID --password '<app-pwd>'"
        rm -f "$ZIP_PATH"
        exit 1
    fi

    echo "✓ Notarization accepted (ID: $NOTARIZATION_ID)"
    xcrun stapler staple "Always.app"
    xcrun stapler validate "Always.app"
    echo "✓ Notarization ticket stapled + validated"

    rm -f "$ZIP_PATH"
else
    echo "⚠️  Skipping notarization (requires ALWAYS_NOTARIZE_TEAM_ID + ALWAYS_NOTARIZE_APPLE_ID + ALWAYS_NOTARIZE_APP_PWD + a real signing identity)"
fi

# Sanity-check before deploy: the bundle must contain BOTH the GUI and the
# daemon binary as distinct files. macOS APFS is case-insensitive by
# default and the previous layout (`MacOS/Always` + `MacOS/always`) silently
# collapsed to a single file, which broke daemon spawn. Fail the build
# loudly rather than ship a half-bundle.
GUI_BIN="Always.app/Contents/MacOS/Always"
DAEMON_BIN_IN_BUNDLE="Always.app/Contents/MacOS/always-daemon"
if [ ! -f "$GUI_BIN" ] || [ ! -f "$DAEMON_BIN_IN_BUNDLE" ]; then
    echo "✗ Bundle integrity check failed:"
    [ ! -f "$GUI_BIN" ] && echo "   missing GUI binary: $GUI_BIN"
    [ ! -f "$DAEMON_BIN_IN_BUNDLE" ] && echo "   missing daemon binary: $DAEMON_BIN_IN_BUNDLE"
    exit 1
fi
gui_size=$(stat -f%z "$GUI_BIN")
daemon_size=$(stat -f%z "$DAEMON_BIN_IN_BUNDLE")
if [ "$gui_size" = "$daemon_size" ]; then
    echo "✗ Bundle integrity check failed: GUI and daemon binaries are the same size ($gui_size). Likely a case-insensitive cp collision."
    exit 1
fi
echo "✓ Bundle integrity: GUI=${gui_size}B, daemon=${daemon_size}B"

echo "Deploying to /Applications..."
rm -rf /Applications/Always.app
cp -r Always.app /Applications/
echo "✓ Deployed to /Applications/Always.app"

echo "App bundle ready. Run with: open -a Always"
