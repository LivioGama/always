# Always -- installer entry point for Windows.
#
#   irm https://raw.githubusercontent.com/LivioGama/always/main/scripts/install.ps1 | iex
#
# Thin wrapper: the installer is scripts/release/install.ps1, published as the
# `install.ps1` asset of every release. This fetches the asset of the latest
# release (or of ALWAYS_VERSION) and runs it. The ALWAYS_* overrides are
# documented in that script.
$repo = 'LivioGama/always'
$ProgressPreference = 'SilentlyContinue'
try { [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12 } catch { }

$sibling = if ($PSScriptRoot) { Join-Path $PSScriptRoot 'release\install.ps1' } else { $null }
if ($sibling -and (Test-Path -LiteralPath $sibling)) {
    . $sibling
} else {
    if ($env:ALWAYS_VERSION) {
        $t = $env:ALWAYS_VERSION
        if (-not $t.StartsWith('v')) { $t = "v$t" }
        $url = "https://github.com/$repo/releases/download/$t/install.ps1"
    } else {
        $url = "https://github.com/$repo/releases/latest/download/install.ps1"
    }
    try {
        # Release assets are served as application/octet-stream, so go through a
        # file: irm/Content would hand back bytes, not text.
        $tmpScript = Join-Path ([IO.Path]::GetTempPath()) ("always-install-" + [Guid]::NewGuid().ToString('N') + '.ps1')
        Invoke-WebRequest -Uri $url -OutFile $tmpScript -UseBasicParsing
        $script = Get-Content -LiteralPath $tmpScript -Raw
        Remove-Item -LiteralPath $tmpScript -Force -ErrorAction SilentlyContinue
    } catch {
        throw "Could not download $url`n    No release exists yet (or the version is wrong). See https://github.com/$repo/releases"
    }
    Invoke-Expression $script
}
