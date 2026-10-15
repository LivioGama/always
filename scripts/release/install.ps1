# ------------------------------------------------------------------------------
# Always - installer for Windows (PowerShell 5.1+ / 7+)
#
#   irm https://raw.githubusercontent.com/LivioGama/always/main/scripts/install.ps1 | iex
#
# (install.ps1 is published as an asset of every release; that one-liner is a
# thin wrapper, scripts/install.ps1, that fetches and runs it.)
#
# Resolves the latest release tag through the releases/latest redirect, downloads
# always-<tag>-x86_64-pc-windows-msvc.zip and its .sha256 sibling, verifies the
# SHA256, installs to %LOCALAPPDATA%\Programs\Always and adds that directory to
# the user PATH. No admin rights needed.
#
# Environment overrides:
#   ALWAYS_VERSION       pin a release tag (e.g. v0.1.0); default: latest release
#   ALWAYS_BASE_URL      fetch assets from here instead of GitHub (http(s):// URL
#                        or a local directory) -- for testing against local
#                        files; needs ALWAYS_VERSION
#   ALWAYS_INSTALL_DIR   install directory (default: %LOCALAPPDATA%\Programs\Always)
#   ALWAYS_NO_PATH       set to 1 to leave the user PATH untouched
# ------------------------------------------------------------------------------

function Install-Always {
    $ErrorActionPreference = 'Stop'
    $ProgressPreference = 'SilentlyContinue'   # Invoke-WebRequest is very slow with progress on 5.1
    try { [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12 } catch { }

    $repo = 'LivioGama/always'
    $releases = "https://github.com/$repo/releases"
    $target = 'x86_64-pc-windows-msvc'

    # -- Platform check ------------------------------------------------------
    $arch = $env:PROCESSOR_ARCHITEW6432
    if (-not $arch) { $arch = $env:PROCESSOR_ARCHITECTURE }
    if ($arch -eq 'ARM64') {
        Write-Warning 'ARM64 detected: installing the x86_64 build, which runs under emulation.'
    } elseif ($arch -ne 'AMD64') {
        throw "Unsupported architecture: $arch (Always for Windows is x86_64)."
    }

    # -- Tag + base URL ------------------------------------------------------
    $tag = $env:ALWAYS_VERSION
    if ($tag) {
        if (-not $tag.StartsWith('v')) { $tag = "v$tag" }
    } elseif ($env:ALWAYS_BASE_URL) {
        throw 'ALWAYS_BASE_URL needs ALWAYS_VERSION (the tag the local assets are named after).'
    } else {
        # releases/latest redirects to .../releases/tag/<tag>, or to .../releases
        # when there is none. Not api.github.com: its anonymous rate limit.
        Write-Host '>>> Resolving the latest release'
        try {
            $resp = Invoke-WebRequest -Uri "$releases/latest" -Method Head -UseBasicParsing
        } catch {
            throw "Could not reach $releases/latest ($($_.Exception.Message))"
        }
        $final = $null
        if ($resp.BaseResponse.ResponseUri) { $final = $resp.BaseResponse.ResponseUri.AbsoluteUri }
        elseif ($resp.BaseResponse.RequestMessage) { $final = $resp.BaseResponse.RequestMessage.RequestUri.AbsoluteUri }
        if ($final -match '/releases/tag/(v[^/?#]+)') {
            $tag = $Matches[1]
        } else {
            throw "No published release found for $repo.`n    See $releases"
        }
    }
    if ($env:ALWAYS_BASE_URL) { $baseUrl = $env:ALWAYS_BASE_URL.TrimEnd('/', '\') }
    else { $baseUrl = "$releases/download/$tag" }

    $asset = "always-$tag-$target.zip"
    $installDir = $env:ALWAYS_INSTALL_DIR
    if (-not $installDir) { $installDir = Join-Path $env:LOCALAPPDATA 'Programs\Always' }

    $tmp = Join-Path ([IO.Path]::GetTempPath()) ("always-install-" + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $tmp | Out-Null

    try {
        function Get-Asset([string]$name, [string]$dest) {
            try {
                if ($baseUrl -match '^https?://') {
                    Invoke-WebRequest -Uri "$baseUrl/$name" -OutFile $dest -UseBasicParsing
                } else {
                    $dir = $baseUrl
                    if ($dir -match '^file:') { $dir = ([Uri]$dir).LocalPath }
                    Copy-Item -LiteralPath (Join-Path $dir $name) -Destination $dest
                }
            } catch {
                throw "Could not download $baseUrl/$name`n    No release with these assets exists yet (or the version is wrong).`n    See $releases`n    ($($_.Exception.Message))"
            }
        }

        Write-Host ">>> Installing Always $tag on Windows (x86_64)"
        $zip = Join-Path $tmp $asset
        $sha = "$zip.sha256"
        Write-Host ">>> Downloading $asset"
        Get-Asset $asset $zip
        Get-Asset "$asset.sha256" $sha

        # -- Verify SHA256 ---------------------------------------------------
        $line = (Get-Content -LiteralPath $sha -TotalCount 1)
        if ($line -notmatch '^([0-9a-fA-F]{64})\b') { throw "Could not read a SHA-256 digest from $asset.sha256" }
        $want = $Matches[1]
        $got = (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash
        if ($got -ne $want.ToUpperInvariant()) {
            throw "Checksum mismatch for $asset`n    expected $want`n    got      $got"
        }
        Write-Host "OK  SHA256 verified: $asset"

        # -- Install ---------------------------------------------------------
        $extract = Join-Path $tmp 'extract'
        Expand-Archive -LiteralPath $zip -DestinationPath $extract -Force
        $found = Get-ChildItem -LiteralPath $extract -Recurse -Filter 'always.exe' | Select-Object -First 1
        if (-not $found) { throw 'Archive does not contain always.exe' }
        $srcDir = $found.DirectoryName

        New-Item -ItemType Directory -Force -Path $installDir | Out-Null
        $exe = Join-Path $installDir 'always.exe'
        # A running daemon locks the exe; stop only the copy in the install dir.
        Get-Process -Name always -ErrorAction SilentlyContinue |
            Where-Object { $_.Path -eq $exe } |
            ForEach-Object { Write-Host '>>> Stopping running Always'; $_ | Stop-Process -Force }
        Copy-Item -Path (Join-Path $srcDir '*') -Destination $installDir -Recurse -Force
        Write-Host "OK  Installed $exe"

        # -- PATH (user scope) -----------------------------------------------
        if ($env:ALWAYS_NO_PATH -ne '1') {
            $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
            $entries = @()
            if ($userPath) { $entries = $userPath.Split(';') | Where-Object { $_ } }
            $normalized = $installDir.TrimEnd('\')
            $present = $entries | Where-Object { $_.TrimEnd('\') -ieq $normalized }
            if (-not $present) {
                $newPath = (@($entries) + $installDir) -join ';'
                [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
                Write-Host "OK  Added $installDir to your user PATH (open a new terminal to use 'always')"
            }
            if (-not (($env:Path.Split(';')) | Where-Object { $_.TrimEnd('\') -ieq $normalized })) {
                $env:Path = "$env:Path;$installDir"
            }
        }

        & $exe --version
        if ($LASTEXITCODE -ne 0) { throw 'The installed always.exe failed to run.' }
        Write-Host ">>> Run 'always --help' to get started. Docs: https://liviogama.github.io/always/"
    }
    finally {
        Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
    }
}

Install-Always
