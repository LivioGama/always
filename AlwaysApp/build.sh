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

echo "App bundle created at AlwaysApp.app"
echo "Run with: open AlwaysApp.app"
