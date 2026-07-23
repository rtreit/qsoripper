<#
.SYNOPSIS
    Start an installed standalone CatHub for QsoRipper integration.

.DESCRIPTION
    Resolves CatHub from CATHUB_EXECUTABLE, a bundled distribution artifact, or PATH.
    QsoRipper no longer builds the CatHub daemon from this repository.
#>
[CmdletBinding()]
param(
    [string]$Config,
    [switch]$DryRun
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
$binaryName = if ($IsWindows -or $env:OS -eq 'Windows_NT') { 'cathub.exe' } else { 'cathub' }

function Find-CatHubExecutable {
    if ($env:CATHUB_EXECUTABLE -and (Test-Path -LiteralPath $env:CATHUB_EXECUTABLE)) {
        return $env:CATHUB_EXECUTABLE
    }
    $bundled = Join-Path $repoRoot "artifacts\publish\cathub\Release\$binaryName"
    if (Test-Path -LiteralPath $bundled) {
        return $bundled
    }
    $sibling = Join-Path (Split-Path -Parent $repoRoot) "cathub\target\release\$binaryName"
    if (Test-Path -LiteralPath $sibling) {
        return $sibling
    }
    $command = Get-Command cathub -ErrorAction SilentlyContinue
    if ($command) {
        return $command.Source
    }
    throw 'CatHub is not installed. Set CATHUB_EXECUTABLE, add cathub to PATH, build it in a sibling cathub checkout, or bundle it under artifacts\publish\cathub\Release.'
}

function Get-QsoRipperConfigPath {
    if ($env:QSORIPPER_CONFIG_PATH) { return $env:QSORIPPER_CONFIG_PATH }
    if ($IsWindows -or $env:OS -eq 'Windows_NT') {
        if ($env:APPDATA) { return (Join-Path $env:APPDATA 'qsoripper\config.toml') }
    }
    elseif ($env:XDG_CONFIG_HOME) {
        return (Join-Path $env:XDG_CONFIG_HOME 'qsoripper/config.toml')
    }
    elseif ($env:HOME) {
        return (Join-Path $env:HOME '.config/qsoripper/config.toml')
    }
    return $null
}

if (-not $Config) {
    $unified = Get-QsoRipperConfigPath
    if ($unified -and (Test-Path -LiteralPath $unified) -and
        (Select-String -LiteralPath $unified -Pattern '^\s*\[\[?cat_hub' -Quiet)) {
        $Config = $unified
    }
    elseif ($env:CATHUB_CONFIG_PATH) {
        $Config = $env:CATHUB_CONFIG_PATH
    }
}

$managed = $Config -and (Test-Path -LiteralPath $Config) -and
    (Select-String -LiteralPath $Config -Pattern '^\s*\[\[?cat_hub' -Quiet)

$executable = Find-CatHubExecutable
$arguments = @()
if ($managed) { $arguments += @('--section', 'cat_hub') }
if ($Config) { $arguments += @('--config', $Config) }
if ($DryRun) { $arguments += '--dry-run' }
& $executable @arguments
