#!/usr/bin/env pwsh
<#
.SYNOPSIS
    Runs the qsoripper-launcher TUI.

.DESCRIPTION
    Thin wrapper around 'cargo run -p qsoripper-launcher' from the Rust
    workspace at src\rust. Defaults to a release build so startup is fast.
    Pass -Dev to use the dev profile instead. Any extra arguments after
    '--' are forwarded to the launcher binary.

.EXAMPLE
    .\launcher.ps1

.EXAMPLE
    .\launcher.ps1 -Dev

.EXAMPLE
    .\launcher.ps1 -- --help
#>

[CmdletBinding()]
param(
    [switch]$Dev,
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$Forward
)

$ErrorActionPreference = 'Stop'

$rustRoot = Join-Path $PSScriptRoot 'src\rust'
if (-not (Test-Path -LiteralPath $rustRoot)) {
    throw "Rust workspace not found at $rustRoot"
}

$cargoArgs = @('run', '-p', 'qsoripper-launcher')
if (-not $Dev) {
    $cargoArgs += '--release'
}
if ($Forward) {
    $cargoArgs += '--'
    $cargoArgs += $Forward
}

Push-Location $rustRoot
try {
    & cargo @cargoArgs
    $exit = $LASTEXITCODE
}
finally {
    Pop-Location
}

exit $exit
