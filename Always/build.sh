#!/bin/bash
set -e

APP_DIR="$(cd "$(dirname "$0")" && pwd)"
CRATE_VERSION="$(grep '^version' "$APP_DIR/../Cargo.toml" | head -1 | cut -d'"' -f2)"

echo "Building Always..."
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

echo "Creating app bundle..."
mkdir -p Always.app/Contents/MacOS
mkdir -p Always.app/Contents/Resources

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

cp Info.plist Always.app/Contents/
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $CRATE_VERSION" Always.app/Contents/Info.plist
/usr/libexec/PlistBuddy -c "Set :CFBundleVersion $CRATE_VERSION" Always.app/Contents/Info.plist
cp "$SWIFT_BIN" Always.app/Contents/MacOS/
cp Resources/AlwaysIcon.icns Always.app/Contents/Resources/

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
    mkdir -p Always.app/Contents/MacOS
    cp "$DAEMON_PATH" Always.app/Contents/MacOS/always-daemon
    echo "✓ Daemon binary copied"
else
    echo "✗ Daemon binary not found at $DAEMON_PATH"
    echo "  Build the daemon first: cd .. && cargo build --bin always (or --release)"
    exit 1
fi

# Bundle Sparkle.framework and add @executable_path/../Frameworks rpath for dyld.
SPARKLE_SRC=".build/artifacts/sparkle/Sparkle/Sparkle.xcframework/macos-arm64_x86_64/Sparkle.framework"
if [ -d "$SPARKLE_SRC" ]; then
    echo "Bundling Sparkle.framework..."
    mkdir -p Always.app/Contents/Frameworks
    rm -rf Always.app/Contents/Frameworks/Sparkle.framework
    cp -R "$SPARKLE_SRC" Always.app/Contents/Frameworks/Sparkle.framework
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
# Prefer Apple Development identity so TCC grants survive debug rebuilds; else ad-hoc.
SIGN_IDENTITY="${ALWAYS_CODESIGN_IDENTITY:-}"
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
codesign --force --deep --sign "$SIGN_IDENTITY" --identifier "com.always.v3" --entitlements Always.entitlements Always.app

# Notarize when ALWAYS_NOTARIZE_TEAM_ID, ALWAYS_NOTARIZE_APPLE_ID, and ALWAYS_NOTARIZE_APP_PWD are set.
if [ "$SIGN_IDENTITY" != "-" ] \
        && [ -n "${ALWAYS_NOTARIZE_TEAM_ID:-}" ] \
        && [ -n "${ALWAYS_NOTARIZE_APPLE_ID:-}" ] \
        && [ -n "${ALWAYS_NOTARIZE_APP_PWD:-}" ]; then
    echo "Notarizing app..."

    if /usr/libexec/PlistBuddy -c "Print :SUPublicEDKey" \
            Always.app/Contents/Info.plist 2>/dev/null \
            | grep -q "REPLACE_WITH_BASE64_EDDSA_PUBLIC_KEY"; then
        echo "✗ Info.plist still contains the Sparkle SUPublicEDKey placeholder."
        echo "  Replace with the real EdDSA public key before publishing — see docs/RELEASE.md."
        exit 1
    fi

    ZIP_PATH="Always.zip"
    ditto -c -k --keepParent "Always.app" "$ZIP_PATH"

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
    xcrun stapler staple "Always.app"
    xcrun stapler validate "Always.app"
    echo "✓ Notarization ticket stapled + validated"

    rm -f "$ZIP_PATH"
else
    echo "⚠️  Skipping notarization (requires ALWAYS_NOTARIZE_TEAM_ID + ALWAYS_NOTARIZE_APPLE_ID + ALWAYS_NOTARIZE_APP_PWD + a real signing identity)"
fi

# Fail if GUI and always-daemon are missing or collapsed to the same file on APFS.
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
DEST_APP="/Applications/Always.app"
if [ -d "$DEST_APP" ]; then
    if [ ! -w "$DEST_APP" ]; then
        echo "✗ $DEST_APP is not writable by $(id -un)."
        echo "  One-time repair: sudo chown -R $(id -un):admin $DEST_APP"
        echo "  After that, dev rebuilds update the app in place without sudo prompts."
        exit 1
    fi
    # rsync in place (no rm -rf): preserves GUI inode + TCC grants across daemon-only rebuilds. Do not pass -X.
    rsync -a --checksum --delete Always.app/Contents/ "$DEST_APP/Contents/"
else
    if [ ! -w /Applications ]; then
        echo "✗ /Applications is not writable by $(id -un)."
        echo "  Install Always.app once from Finder, or repair /Applications permissions."
        exit 1
    fi
    cp -R Always.app "$DEST_APP"
fi
echo "✓ Deployed to /Applications/Always.app"

echo "App bundle ready. Run with: open -a Always"
