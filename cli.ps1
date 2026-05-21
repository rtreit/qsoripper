#!/usr/bin/env pwsh
<#
.SYNOPSIS
    Runs the prebuilt QsoRipper.Cli for instant startup.

.DESCRIPTION
    Invokes the compiled QsoRipper.Cli.dll under
    src\dotnet\QsoRipper.Cli\bin\<Config>\net10.0 directly with `dotnet`, so
    there is no msbuild evaluation overhead. If the binary is missing (or
    -Rebuild is passed) the script builds it first with 'dotnet build'.

    Pass -Dev to use the Debug configuration. Arguments after '--' are
    forwarded to the CLI.

.EXAMPLE
    .\cli.ps1 status

.EXAMPLE
    .\cli.ps1 -Rebuild -- --help

.EXAMPLE
    .\cli.ps1 -Dev log list
#>

[CmdletBinding()]
param(
    [switch]$Dev,
    [switch]$Rebuild,
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$Forward
)

$ErrorActionPreference = 'Stop'

$projectPath = Join-Path $PSScriptRoot 'src\dotnet\QsoRipper.Cli\QsoRipper.Cli.csproj'
if (-not (Test-Path -LiteralPath $projectPath)) {
    throw "QsoRipper.Cli project not found at $projectPath"
}

$config = if ($Dev) { 'Debug' } else { 'Release' }
$tfm = 'net10.0'
$dllPath = Join-Path $PSScriptRoot "src\dotnet\QsoRipper.Cli\bin\$config\$tfm\QsoRipper.Cli.dll"

if ($Rebuild -or -not (Test-Path -LiteralPath $dllPath)) {
    $buildArgs = @('build', $projectPath, '-c', $config, '--nologo', '-v', 'minimal')
    & dotnet @buildArgs
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}

if (-not (Test-Path -LiteralPath $dllPath)) {
    throw "CLI assembly not found at $dllPath after build"
}

if ($Forward) {
    & dotnet $dllPath @Forward
} else {
    & dotnet $dllPath
}
exit $LASTEXITCODE
