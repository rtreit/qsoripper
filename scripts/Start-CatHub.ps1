<#
.SYNOPSIS
    Start the qsoripper-cathub multi-client CAT hub daemon.

.DESCRIPTION
    Builds (release) and launches the cathub daemon, which becomes the single owner of the
    radio serial port and fans it out to every configured client (HDSDR/OmniRig, N1MM,
    ARCP-590, WSJT-X, Log4OM, and the QsoRipper engine).

    This replaces the legacy rigctld + Python safe-bridge + rigctlcom chain. Do not run the
    old chain at the same time: only one process may own the radio's COM port.

.PARAMETER Config
    Path to the cathub TOML config. Defaults to config\cathub.toml in the repo.

.PARAMETER DryRun
    Validate and print the config, then exit without opening any ports.

.PARAMETER Debug
    Build and run the debug profile instead of release.

.EXAMPLE
    .\scripts\Start-CatHub.ps1 -DryRun

.EXAMPLE
    .\scripts\Start-CatHub.ps1
#>
[CmdletBinding()]
param(
    [string]$Config,
    [switch]$DryRun,
    [switch]$Debug
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
$manifest = Join-Path $repoRoot 'src\rust\Cargo.toml'

if (-not $Config) {
    $Config = Join-Path $repoRoot 'config\cathub.toml'
}
if (-not (Test-Path $Config)) {
    throw "Config not found: $Config"
}

$logDir = $env:USERPROFILE

$cargoArgs = @('run')
if (-not $Debug) { $cargoArgs += '--release' }
$cargoArgs += @('-p', 'qsoripper-cathub', '--manifest-path', $manifest, '--')
$cargoArgs += @('--config', $Config)
if ($DryRun) { $cargoArgs += '--dry-run' }

Write-Host "Starting cathub with config: $Config" -ForegroundColor Cyan
Write-Host "Rolling log: $logDir\qsoripper-cathub.log.*" -ForegroundColor DarkGray
Write-Host "cargo $($cargoArgs -join ' ')" -ForegroundColor DarkGray

& cargo @cargoArgs
