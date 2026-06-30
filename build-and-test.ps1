#!/usr/bin/env pwsh
<#
.SYNOPSIS
    Build QsoRipper and then run all automated tests.

.DESCRIPTION
    Convenience wrapper around build.ps1 and test.ps1 for local pre-push
    validation when you want the normal build artifacts plus the full test run.

.PARAMETER Configuration
    Build/test configuration. Default: Release.

.PARAMETER Win32Generator
    CMake generator for local Win32 tests. Default: Visual Studio 18 2026.

.EXAMPLE
    ./build-and-test.ps1
    ./build-and-test.ps1 -Configuration Debug
#>

param(
    [ValidateSet('Release', 'Debug')]
    [string]$Configuration = 'Release',

    [string]$Win32Generator = 'Visual Studio 18 2026'
)

$ErrorActionPreference = 'Stop'

& (Join-Path $PSScriptRoot 'build.ps1') build -Configuration $Configuration
if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}

& (Join-Path $PSScriptRoot 'test.ps1') all -Configuration $Configuration -Win32Generator $Win32Generator
exit $LASTEXITCODE
