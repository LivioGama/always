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
    echo "Copying daemon binary to app bundle ($DAEMON_PATH)..."
    mkdir -p Always.app/Contents/MacOS
    cp "$DAEMON_PATH" Always.app/Contents/MacOS/always
    echo "✓ Daemon binary copied"
else
    echo "⚠️  Warning: Daemon binary not found at $DAEMON_PATH"
    echo "   Build the daemon first: cd .. && cargo build --bin always (or --release)"
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

# Notarization (only if using proper Apple Developer identity, not ad-hoc)
if [ "$SIGN_IDENTITY" != "-" ] && [ -n "$ALWAYS_NOTARIZE_TEAM_ID" ]; then
    echo "Notarizing app..."
    
    # Create a zip file for notarization
    ZIP_PATH="Always.zip"
    ditto -c -k --keepParent "Always.app" "$ZIP_PATH"
    
    # Submit for notarization
    NOTARIZATION_OUTPUT=$(xcrun notarytool submit "$ZIP_PATH" \
        --team-id "$ALWAYS_NOTARIZE_TEAM_ID" \
        --apple-id "com.always" \
        --wait \
        --output-format json)
    
    # Extract notarization ID
    NOTARIZATION_ID=$(echo "$NOTARIZATION_OUTPUT" | python3 -c "import sys, json; print(json.load(sys.stdin)['id'])" 2>/dev/null || echo "")
    
    if [ -n "$NOTARIZATION_ID" ]; then
        echo "✓ Notarization submitted (ID: $NOTARIZATION_ID)"
        
        # Staple the notarization ticket
        xcrun stapler staple "Always.app"
        echo "✓ Notarization ticket stapled"
        
        # Verify notarization
        xcrun stapler validate "Always.app"
        echo "✓ Notarization validated"
    else
        echo "⚠️  Notarization failed or skipped"
    fi
    
    # Clean up zip file
    rm -f "$ZIP_PATH"
else
    echo "⚠️  Skipping notarization (requires ALWAYS_NOTARIZE_TEAM_ID and proper signing identity)"
fi

echo "Deploying to /Applications..."
rm -rf /Applications/Always.app
cp -r Always.app /Applications/
echo "✓ Deployed to /Applications/Always.app"

echo "App bundle ready. Run with: open -a Always"
