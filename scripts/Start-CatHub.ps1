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
    Path to the cathub TOML config. When omitted, the unified per-user config.toml is used if
    it contains a [cat_hub] section (the same config the engine and launcher share); otherwise
    the standalone config\cathub.toml sample in the repo is used.

.PARAMETER DryRun
    Validate and print the config, then exit without opening any ports.

.PARAMETER DebugBuild
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
    [switch]$DebugBuild
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
$manifest = Join-Path $repoRoot 'src\rust\Cargo.toml'

function Get-UnifiedConfigPath {
    if ($env:QSORIPPER_CONFIG_PATH) {
        return $env:QSORIPPER_CONFIG_PATH
    }
    if ($IsWindows -or $env:OS -eq 'Windows_NT') {
        if ($env:APPDATA) {
            return (Join-Path $env:APPDATA 'qsoripper\config.toml')
        }
        return $null
    }
    if ($env:XDG_CONFIG_HOME) {
        return (Join-Path $env:XDG_CONFIG_HOME 'qsoripper/config.toml')
    }
    if ($env:HOME) {
        return (Join-Path $env:HOME '.config/qsoripper/config.toml')
    }
    return $null
}

if (-not $Config) {
    # Prefer the unified config.toml when it carries a [cat_hub] section, so cathub, the
    # engine, and the launcher all read one file. Fall back to the repo sample otherwise.
    $unified = Get-UnifiedConfigPath
    if ($unified -and (Test-Path $unified) -and
        (Select-String -Path $unified -Pattern '^\s*\[\[?cat_hub' -Quiet)) {
        $Config = $unified
    }
    else {
        $Config = Join-Path $repoRoot 'config\cathub.toml'
    }
}
if (-not (Test-Path $Config)) {
    throw "Config not found: $Config"
}

$logDir = $env:USERPROFILE

# Prefer the published binary for instant startup; fall back to 'cargo run' when it is missing.
$configuration = if ($DebugBuild) { 'Debug' } else { 'Release' }
$binaryName = if ($IsWindows -or $env:OS -eq 'Windows_NT') { 'qsoripper-cathub.exe' } else { 'qsoripper-cathub' }
$publishedBinary = Join-Path $repoRoot "artifacts\publish\qsoripper-cathub\$configuration\$binaryName"

Write-Host "Starting cathub with config: $Config" -ForegroundColor Cyan
Write-Host "Rolling log: $logDir\qsoripper-cathub.log.*" -ForegroundColor DarkGray

if (Test-Path $publishedBinary) {
    $runArgs = @('--config', $Config)
    if ($DryRun) { $runArgs += '--dry-run' }
    Write-Host "$publishedBinary $($runArgs -join ' ')" -ForegroundColor DarkGray
    & $publishedBinary @runArgs
}
else {
    $cargoArgs = @('run')
    if (-not $DebugBuild) { $cargoArgs += '--release' }
    $cargoArgs += @('-p', 'qsoripper-cathub', '--manifest-path', $manifest, '--')
    $cargoArgs += @('--config', $Config)
    if ($DryRun) { $cargoArgs += '--dry-run' }
    Write-Host "cargo $($cargoArgs -join ' ')" -ForegroundColor DarkGray
    & cargo @cargoArgs
}
