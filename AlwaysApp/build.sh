#!/bin/bash
set -e

APP_DIR="$(cd "$(dirname "$0")" && pwd)"

echo "Building AlwaysApp..."
cd "$APP_DIR"
swift build

echo "Creating app bundle..."
mkdir -p AlwaysApp.app/Contents/MacOS
mkdir -p AlwaysApp.app/Contents/Resources

# Debug: check if executable exists
if [ -f .build/debug/AlwaysApp ]; then
    echo "✓ Executable found at .build/debug/AlwaysApp"
else
    echo "✗ Executable NOT found at .build/debug/AlwaysApp"
    echo "Trying direct path..."
    if [ -f .build/arm64-apple-macosx/debug/AlwaysApp ]; then
        echo "✓ Found at .build/arm64-apple-macosx/debug/AlwaysApp"
    fi
fi

# Always update bundle contents
cp Info.plist AlwaysApp.app/Contents/
cp .build/debug/AlwaysApp AlwaysApp.app/Contents/MacOS/
cp Resources/AlwaysIcon.icns AlwaysApp.app/Contents/Resources/

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
    mkdir -p AlwaysApp.app/Contents/MacOS
    cp "$DAEMON_PATH" AlwaysApp.app/Contents/MacOS/always
    echo "✓ Daemon binary copied"
else
    echo "⚠️  Warning: Daemon binary not found at $DAEMON_PATH"
    echo "   Build the daemon first: cd .. && cargo build --bin always (or --release)"
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
codesign --force --deep --sign "$SIGN_IDENTITY" --identifier "com.alwaysapp.daemon" --entitlements AlwaysApp.entitlements AlwaysApp.app

# Notarization (only if using proper Apple Developer identity, not ad-hoc)
if [ "$SIGN_IDENTITY" != "-" ] && [ -n "$ALWAYS_NOTARIZE_TEAM_ID" ]; then
    echo "Notarizing app..."
    
    # Create a zip file for notarization
    ZIP_PATH="AlwaysApp.zip"
    ditto -c -k --keepParent "AlwaysApp.app" "$ZIP_PATH"
    
    # Submit for notarization
    NOTARIZATION_OUTPUT=$(xcrun notarytool submit "$ZIP_PATH" \
        --team-id "$ALWAYS_NOTARIZE_TEAM_ID" \
        --apple-id "com.alwaysapp.daemon" \
        --wait \
        --output-format json)
    
    # Extract notarization ID
    NOTARIZATION_ID=$(echo "$NOTARIZATION_OUTPUT" | python3 -c "import sys, json; print(json.load(sys.stdin)['id'])" 2>/dev/null || echo "")
    
    if [ -n "$NOTARIZATION_ID" ]; then
        echo "✓ Notarization submitted (ID: $NOTARIZATION_ID)"
        
        # Staple the notarization ticket
        xcrun stapler staple "AlwaysApp.app"
        echo "✓ Notarization ticket stapled"
        
        # Verify notarization
        xcrun stapler validate "AlwaysApp.app"
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
rm -rf /Applications/AlwaysApp.app
cp -r AlwaysApp.app /Applications/
echo "✓ Deployed to /Applications/AlwaysApp.app"

echo "App bundle ready. Run with: open -a AlwaysApp"
