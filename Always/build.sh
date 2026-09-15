#!/bin/bash
set -e

APP_DIR="$(cd "$(dirname "$0")" && pwd)"
CRATE_VERSION="$(grep '^version' "$APP_DIR/../Cargo.toml" | head -1 | cut -d'"' -f2)"

# Instance identity. Defaults produce the production app; the dev loop
# overrides all four so `Always Dev.app` is a separate install that never
# collides with /Applications/Always.app.
APP_NAME="${ALWAYS_APP_NAME:-Always}"
APP_BUNDLE="${APP_NAME}.app"
BUNDLE_ID="${ALWAYS_BUNDLE_ID:-com.always.v3}"
DEPLOY_PATH="${ALWAYS_DEPLOY_PATH:-/Applications/${APP_BUNDLE}}"
URL_SCHEME="${ALWAYS_URL_SCHEME:-always}"
# 0 strips Sparkle keys from the plist — dev builds must never self-update
# into a production release.
SPARKLE_ENABLED="${ALWAYS_SPARKLE:-1}"

echo "Building ${APP_NAME}..."
cd "$APP_DIR"
SWIFT_CONFIGURATION="${ALWAYS_SWIFT_CONFIGURATION:-${ALWAYS_BUILD_PROFILE:-debug}}"
case "$SWIFT_CONFIGURATION" in
    release|debug) ;;
    *)
        echo "✗ Invalid Swift configuration: $SWIFT_CONFIGURATION"
        echo "  Use ALWAYS_SWIFT_CONFIGURATION=debug or release."
        exit 1
        ;;
esac
swift build -c "$SWIFT_CONFIGURATION"

echo "Creating app bundle (${APP_BUNDLE}, id ${BUNDLE_ID})..."
mkdir -p "$APP_BUNDLE/Contents/MacOS"
mkdir -p "$APP_BUNDLE/Contents/Resources"

SWIFT_BIN=".build/$SWIFT_CONFIGURATION/Always"
ARCH_SWIFT_BIN=".build/$(uname -m)-apple-macosx/$SWIFT_CONFIGURATION/Always"
if [ -f "$SWIFT_BIN" ]; then
    echo "✓ Executable found at $SWIFT_BIN"
elif [ -f "$ARCH_SWIFT_BIN" ]; then
    SWIFT_BIN="$ARCH_SWIFT_BIN"
    echo "✓ Executable found at $SWIFT_BIN"
else
    echo "✗ Executable NOT found for Swift configuration '$SWIFT_CONFIGURATION'"
    echo "  Checked: .build/$SWIFT_CONFIGURATION/Always"
    echo "           $ARCH_SWIFT_BIN"
    exit 1
fi

cp Info.plist "$APP_BUNDLE/Contents/"
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $CRATE_VERSION" "$APP_BUNDLE/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleVersion $CRATE_VERSION" "$APP_BUNDLE/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleIdentifier $BUNDLE_ID" "$APP_BUNDLE/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleName $APP_NAME" "$APP_BUNDLE/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleDisplayName $APP_NAME" "$APP_BUNDLE/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleURLTypes:0:CFBundleURLSchemes:0 $URL_SCHEME" "$APP_BUNDLE/Contents/Info.plist"
if [ "$SPARKLE_ENABLED" = "0" ]; then
    for key in SUFeedURL SUPublicEDKey SUEnableAutomaticChecks SUScheduledCheckInterval; do
        /usr/libexec/PlistBuddy -c "Delete :$key" "$APP_BUNDLE/Contents/Info.plist" 2>/dev/null || true
    done
fi
cp "$SWIFT_BIN" "$APP_BUNDLE/Contents/MacOS/"
cp Resources/AlwaysIcon.icns "$APP_BUNDLE/Contents/Resources/"

# Daemon binary: ALWAYS_BUILD_PROFILE=release|debug, else pick newest build.
RELEASE_BIN="../target/release/always"
DEBUG_BIN="../target/debug/always"
DAEMON_PATH="${ALWAYS_DAEMON_PATH:-}"
case "${ALWAYS_BUILD_PROFILE:-}" in
    release) DAEMON_PATH="${DAEMON_PATH:-$RELEASE_BIN}" ;;
    debug)   DAEMON_PATH="${DAEMON_PATH:-$DEBUG_BIN}" ;;
    *)
        if [ -n "$DAEMON_PATH" ]; then
            :
        elif [ -f "$RELEASE_BIN" ] && [ -f "$DEBUG_BIN" ]; then
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
    # Ship as always-daemon: APFS is case-insensitive; MacOS/always overwrites MacOS/Always.
    echo "Copying daemon binary to app bundle ($DAEMON_PATH → MacOS/always-daemon)..."
    mkdir -p "$APP_BUNDLE/Contents/MacOS"
    cp "$DAEMON_PATH" "$APP_BUNDLE/Contents/MacOS/always-daemon"
    echo "✓ Daemon binary copied"
else
    echo "✗ Daemon binary not found at $DAEMON_PATH"
    echo "  Build the daemon first: cd .. && cargo build --bin always (or --release)"
    exit 1
fi

# Bundle Sparkle.framework and add @executable_path/../Frameworks rpath for dyld.
# Sparkle.framework is always bundled when present — the binary links it
# via @rpath, so omitting it crashes at dyld. For dev builds the updater
# stays inert anyway: plist keys are stripped above and UpdateService
# refuses to start under a .dev bundle id.
SPARKLE_SRC=".build/artifacts/sparkle/Sparkle/Sparkle.xcframework/macos-arm64_x86_64/Sparkle.framework"
if [ -d "$SPARKLE_SRC" ]; then
    echo "Bundling Sparkle.framework..."
    mkdir -p "$APP_BUNDLE/Contents/Frameworks"
    rm -rf "$APP_BUNDLE/Contents/Frameworks/Sparkle.framework"
    cp -R "$SPARKLE_SRC" "$APP_BUNDLE/Contents/Frameworks/Sparkle.framework"
    if ! otool -l "$APP_BUNDLE/Contents/MacOS/Always" \
            | grep -A2 LC_RPATH | grep -q "@executable_path/../Frameworks"; then
        install_name_tool -add_rpath "@executable_path/../Frameworks" \
            "$APP_BUNDLE/Contents/MacOS/Always"
        echo "✓ Added @executable_path/../Frameworks rpath"
    fi
    echo "✓ Sparkle.framework copied"
else
    echo "⚠️  Sparkle.framework not found at $SPARKLE_SRC — auto-update will not work"
fi

echo "Code signing app..."
# Signing identity resolution — STABILITY over pedigree. macOS TCC keys
# Accessibility/Input Monitoring grants to the signature: a real cert
# gives a designated requirement that survives rebuilds; ad-hoc keys to
# the per-build cdhash, so EVERY rebuild silently voids every grant and
# the user gets re-prompted forever. Preference order:
#   1. Explicit ALWAYS_CODESIGN_IDENTITY
#   2. "Always Local Signing" — persistent self-signed local dev cert
#      (create once; grants then survive all local rebuilds)
#   3. Developer ID / Apple Development certs
#   4. Ad-hoc (last resort — expect permission re-prompts per build)
SIGN_IDENTITY="${ALWAYS_CODESIGN_IDENTITY:-}"
if [ -z "$SIGN_IDENTITY" ]; then
    SIGN_IDENTITY="$(security find-identity -v -p codesigning 2>/dev/null | sed -n 's/.*"\(Always Local Signing\)".*/\1/p' | head -1)"
fi
if [ -z "$SIGN_IDENTITY" ]; then
    SIGN_IDENTITY="$(security find-identity -v -p codesigning 2>/dev/null | sed -n 's/.*"\(Developer ID Application: [^"]*\)".*/\1/p' | head -1)"
fi
if [ -z "$SIGN_IDENTITY" ]; then
    SIGN_IDENTITY="$(security find-identity -v -p codesigning 2>/dev/null | sed -n 's/.*"\(Apple Development: [^"]*\)".*/\1/p' | head -1)"
fi
if [ -z "$SIGN_IDENTITY" ]; then
    SIGN_IDENTITY="-"
fi
echo "Using signing identity: ${SIGN_IDENTITY}"
codesign --force --deep --sign "$SIGN_IDENTITY" --identifier "$BUNDLE_ID" --entitlements Always.entitlements "$APP_BUNDLE"

# Notarize when ALWAYS_NOTARIZE_TEAM_ID, ALWAYS_NOTARIZE_APPLE_ID, and ALWAYS_NOTARIZE_APP_PWD are set.
if [ "$SIGN_IDENTITY" != "-" ] \
        && [ -n "${ALWAYS_NOTARIZE_TEAM_ID:-}" ] \
        && [ -n "${ALWAYS_NOTARIZE_APPLE_ID:-}" ] \
        && [ -n "${ALWAYS_NOTARIZE_APP_PWD:-}" ]; then
    echo "Notarizing app..."

    if /usr/libexec/PlistBuddy -c "Print :SUPublicEDKey" \
            "$APP_BUNDLE/Contents/Info.plist" 2>/dev/null \
            | grep -q "REPLACE_WITH_BASE64_EDDSA_PUBLIC_KEY"; then
        echo "✗ Info.plist still contains the Sparkle SUPublicEDKey placeholder."
        echo "  Replace with the real EdDSA public key before publishing — see docs/RELEASE.md."
        exit 1
    fi

    ZIP_PATH="${APP_NAME}.zip"
    ditto -c -k --keepParent "$APP_BUNDLE" "$ZIP_PATH"

    NOTARIZATION_OUTPUT=$(xcrun notarytool submit "$ZIP_PATH" \
        --team-id "$ALWAYS_NOTARIZE_TEAM_ID" \
        --apple-id "$ALWAYS_NOTARIZE_APPLE_ID" \
        --password "$ALWAYS_NOTARIZE_APP_PWD" \
        --wait \
        --timeout 30m \
        --output-format json)

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
    xcrun stapler staple "$APP_BUNDLE"
    xcrun stapler validate "$APP_BUNDLE"
    echo "✓ Notarization ticket stapled + validated"

    rm -f "$ZIP_PATH"
else
    echo "⚠️  Skipping notarization (requires ALWAYS_NOTARIZE_TEAM_ID + ALWAYS_NOTARIZE_APPLE_ID + ALWAYS_NOTARIZE_APP_PWD + a real signing identity)"
fi

# Fail if GUI and always-daemon are missing or collapsed to the same file on APFS.
GUI_BIN="$APP_BUNDLE/Contents/MacOS/Always"
DAEMON_BIN_IN_BUNDLE="$APP_BUNDLE/Contents/MacOS/always-daemon"
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

echo "Deploying to ${DEPLOY_PATH}..."
DEST_APP="$DEPLOY_PATH"
if [ -d "$DEST_APP" ]; then
    if [ ! -w "$DEST_APP" ]; then
        echo "✗ $DEST_APP is not writable by $(id -un)."
        echo "  One-time repair: sudo chown -R $(id -un):admin $DEST_APP"
        echo "  After that, rebuilds update the app in place without sudo prompts."
        exit 1
    fi
    # rsync in place (no rm -rf): preserves GUI inode + TCC grants across daemon-only rebuilds. Do not pass -X.
    rsync -a --checksum --delete "$APP_BUNDLE/Contents/" "$DEST_APP/Contents/"
else
    dest_parent="$(dirname "$DEST_APP")"
    if [ ! -w "$dest_parent" ]; then
        echo "✗ $dest_parent is not writable by $(id -un)."
        echo "  Install ${APP_BUNDLE} once from Finder, or repair permissions."
        exit 1
    fi
    cp -R "$APP_BUNDLE" "$DEST_APP"
fi
echo "✓ Deployed to ${DEPLOY_PATH}"

echo "App bundle ready. Run with: open \"${DEPLOY_PATH}\""
