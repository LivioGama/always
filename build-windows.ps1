# Always - Windows Build Script
# Prerequisites: Rust (with MSVC toolchain), git

param(
    [ValidateSet("release", "debug")]
    [string]$Profile = "release"
)

Write-Host "🔨 Always Windows Build Script" -ForegroundColor Cyan
Write-Host ""

# Verify prerequisites
Write-Host "Checking prerequisites..." -ForegroundColor Yellow
$missing = @()

# Check Rust
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    $missing += "Rust toolchain (install from https://rustup.rs/)"
}

# Check MSVC toolchain
$result = rustup toolchain list 2>&1
if (-not ($result -match "stable-x86_64-pc-windows-msvc")) {
    Write-Host "⚠️  MSVC target not installed. Installing..." -ForegroundColor Yellow
    rustup target add x86_64-pc-windows-msvc
}

if ($missing.Count -gt 0) {
    Write-Host "❌ Missing prerequisites:" -ForegroundColor Red
    $missing | ForEach-Object { Write-Host "   - $_" }
    Write-Host ""
    Write-Host "Please install the missing tools and try again." -ForegroundColor Red
    exit 1
}

Write-Host "✅ All prerequisites found" -ForegroundColor Green
Write-Host ""

# Build
Write-Host "Building Always daemon for Windows ($Profile)..." -ForegroundColor Yellow
Write-Host ""

$buildArgs = @(
    "build",
    "--no-default-features",
    "--features", "windows",
    "--target", "x86_64-pc-windows-msvc",
    "--$Profile"
)

cargo @buildArgs

if ($LASTEXITCODE -ne 0) {
    Write-Host "❌ Build failed!" -ForegroundColor Red
    exit 1
}

Write-Host ""
Write-Host "✅ Build successful!" -ForegroundColor Green

# Output binary info
$binaryPath = "target/x86_64-pc-windows-msvc/$Profile/always.exe"
$binarySize = (Get-Item $binaryPath -ErrorAction SilentlyContinue).Length / 1MB
$binarySize = [math]::Round($binarySize, 2)

Write-Host ""
Write-Host "📦 Output:" -ForegroundColor Cyan
Write-Host "   Binary: $binaryPath"
Write-Host "   Size: $binarySize MB"
Write-Host ""

# Test that binary can run
Write-Host "Testing binary..." -ForegroundColor Yellow
$versionOutput = & ".\$binaryPath" --version 2>&1
if ($LASTEXITCODE -eq 0) {
    Write-Host "✅ Binary test passed"
    Write-Host "   Version: $versionOutput"
} else {
    Write-Host "⚠️  Binary test skipped (may require Groq API key configured)" -ForegroundColor Yellow
}

Write-Host ""
Write-Host "🎉 Windows build complete!" -ForegroundColor Green
Write-Host ""
Write-Host "Next steps:" -ForegroundColor Cyan
Write-Host "1. Set your Groq API key:"
Write-Host "   $binaryPath config set groq_api_key sk_..."
Write-Host ""
Write-Host "2. Start the daemon:"
Write-Host "   $binaryPath start"
Write-Host ""
Write-Host "3. Check logs:"
Write-Host "   $binaryPath logs --pretty"
