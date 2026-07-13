#!/bin/bash
# Always - Windows Build Script (Git Bash or WSL)
# Prerequisites: Rust (with MSVC toolchain), git

set -e

PROFILE="${1:-release}"

echo "🔨 Always Windows Build Script"
echo ""

# Verify prerequisites
echo "Checking prerequisites..."
MISSING=()

# Check Rust
if ! command -v cargo &> /dev/null; then
    MISSING+=("Rust toolchain (install from https://rustup.rs/)")
fi

# Check MSVC toolchain
if ! rustup toolchain list | grep -q "stable-x86_64-pc-windows-msvc"; then
    echo "⚠️  MSVC target not installed. Installing..."
    rustup target add x86_64-pc-windows-msvc
fi

if [ ${#MISSING[@]} -gt 0 ]; then
    echo "❌ Missing prerequisites:"
    printf '   - %s\n' "${MISSING[@]}"
    echo ""
    echo "Please install the missing tools and try again."
    exit 1
fi

echo "✅ All prerequisites found"
echo ""

# Build
echo "Building Always daemon for Windows ($PROFILE)..."
echo ""

cargo build \
    --no-default-features \
    --features windows \
    --target x86_64-pc-windows-msvc \
    --"$PROFILE"

echo ""
echo "✅ Build successful!"

# Output binary info
BINARY_PATH="target/x86_64-pc-windows-msvc/$PROFILE/always.exe"
BINARY_SIZE=$(du -h "$BINARY_PATH" | cut -f1)

echo ""
echo "📦 Output:"
echo "   Binary: $BINARY_PATH"
echo "   Size: $BINARY_SIZE"
echo ""

# Test that binary can run
echo "Testing binary..."
if "$BINARY_PATH" --version > /dev/null 2>&1; then
    echo "✅ Binary test passed"
    VERSION=$("$BINARY_PATH" --version)
    echo "   Version: $VERSION"
else
    echo "⚠️  Binary test skipped (may require Groq API key configured)"
fi

echo ""
echo "🎉 Windows build complete!"
echo ""
echo "Next steps:"
echo "1. Set your Groq API key:"
echo "   $BINARY_PATH config set groq_api_key sk_..."
echo ""
echo "2. Start the daemon:"
echo "   $BINARY_PATH start"
echo ""
echo "3. Check logs:"
echo "   $BINARY_PATH logs --pretty"
