<#
.SYNOPSIS
    Sets up Always to launch automatically on Windows login.

.DESCRIPTION
    Creates (or updates) a registry Run key so that the Always daemon
    starts whenever the user logs in.

    The script:
    1. Locates the `always.exe` binary — first from an explicit argument,
       then from the script's own directory, then from PATH.
    2. Writes the full path to
       HKCU:\Software\Microsoft\Windows\CurrentVersion\Run
       under the key name "Always".
    3. Prints the result and exits 0 on success.

.PARAMETER ExePath
    Full path to the always.exe binary.  If omitted the script
    discovers it automatically (see DESCRIPTION).

.PARAMETER Remove
    Remove the autostart entry instead of creating it.

.EXAMPLE
    # Auto-discover the binary and register it:
    .\scripts\setup-windows-autostart.ps1

    # Register an explicitly located binary:
    .\scripts\setup-windows-autostart.ps1 -ExePath "C:\Program Files\Always\always.exe"

    # Remove the autostart entry:
    .\scripts\setup-windows-autostart.ps1 -Remove

.NOTES
    Requires Administrator elevation to write to HKCU.
    Run "powershell -ExecutionPolicy Bypass -File scripts\setup-windows-autostart.ps1"
    from the repository root.
#>

[CmdletBinding()]
param(
    [Parameter(Mandatory = $false)]
    [string]$ExePath = "",

    [Parameter(Mandatory = $false)]
    [switch]$Remove
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$RunKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run"
$ValueName = "Always"

# ─── Resolve the binary path ─────────────────────────────────────────

if (-not $ExePath) {
    # 1. Try the script's parent directory (repository root).
    $ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Definition
    $Candidate = Join-Path $ScriptDir "target\release\always.exe"
    if (Test-Path $Candidate) {
        $ExePath = (Resolve-Path $Candidate).Path
    } else {
        # 2. Try from PATH.
        $ExePath = Get-Command always.exe -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Source
    }

    if (-not $ExePath) {
        Write-Error @"
Cannot find always.exe.

Please either:
  1. Build the binary first:  cargo build --release
  2. Or specify its location:
     .\scripts\setup-windows-autostart.ps1 -ExePath "C:\path\to\always.exe"
"@
        exit 1
    }
}

# Normalize to a full path.
$ExePath = (Resolve-Path $ExePath).Path

# ─── Registry operation ──────────────────────────────────────────────

try {
    # Ensure the Run key exists.
    if (-not (Test-Path $RunKey)) {
        New-Item -Path $RunKey -Force | Out-Null
    }

    if ($Remove) {
        Remove-ItemProperty -Path $RunKey -Name $ValueName -ErrorAction SilentlyContinue
        Write-Host "Removed Always from autostart (HKCU:\...\Run\$ValueName)."
        exit 0
    } else {
        # Escape the path in case it contains spaces or quotes.
        $SafeExePath = "`"$ExePath`""

        Set-ItemProperty -Path $RunKey -Name $ValueName -Value $SafeExePath
        Write-Host @"
Always has been added to Windows autostart.

  Binary : $ExePath
  Registry: HKCU:\Software\Microsoft\Windows\CurrentVersion\Run\$ValueName
  Value  : $SafeExePath

The daemon will launch on your next login.  To remove:
  .\scripts\setup-windows-autostart.ps1 -Remove
"@
    }
} catch {
    Write-Error "Failed to update the Run key: $_"
    exit 1
}
