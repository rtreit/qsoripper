#!/usr/bin/env pwsh
<#
.SYNOPSIS
    Runs the prebuilt qsoripper-launcher TUI for instant startup.

.DESCRIPTION
    Invokes the release binary at src\rust\target\release\qsoripper-launcher
    directly, bypassing cargo so there is no manifest-resolve overhead. If the
    binary is missing (or -Rebuild is passed) the script builds it first with
    'cargo build --release'.

    Pass -Dev to use the unoptimized debug binary. Arguments after '--' are
    forwarded to the launcher.

.EXAMPLE
    .\launcher.ps1

.EXAMPLE
    .\launcher.ps1 -Rebuild

.EXAMPLE
    .\launcher.ps1 -- --help
#>

[CmdletBinding()]
param(
    [switch]$Dev,
    [switch]$Rebuild,
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$Forward
)

$ErrorActionPreference = 'Stop'

$rustRoot = Join-Path $PSScriptRoot 'src\rust'
if (-not (Test-Path -LiteralPath $rustRoot)) {
    throw "Rust workspace not found at $rustRoot"
}

$profileDir = if ($Dev) { 'debug' } else { 'release' }
$exeName = if ($IsWindows -or $env:OS -eq 'Windows_NT') { 'qsoripper-launcher.exe' } else { 'qsoripper-launcher' }
$exePath = Join-Path $rustRoot "target\$profileDir\$exeName"

if ($Rebuild -or -not (Test-Path -LiteralPath $exePath)) {
    $buildArgs = @('build', '-p', 'qsoripper-launcher')
    if (-not $Dev) { $buildArgs += '--release' }
    Push-Location $rustRoot
    try {
        & cargo @buildArgs
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    }
    finally {
        Pop-Location
    }
}

if (-not (Test-Path -LiteralPath $exePath)) {
    throw "Launcher binary not found at $exePath after build"
}

if ($Forward) {
    & $exePath @Forward
} else {
    & $exePath
}
exit $LASTEXITCODE
