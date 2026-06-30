#!/usr/bin/env pwsh
<#
.SYNOPSIS
    Cross-platform test script for QsoRipper.

.DESCRIPTION
    Runs the repository's automated test suites without the heavier build,
    formatting, coverage, and vulnerability gates from build.ps1 check.

.PARAMETER Command
    The test command to run. Default: all.

.PARAMETER Configuration
    Build configuration for .NET and Win32 test runs. Default: Debug.

.PARAMETER Win32Generator
    CMake generator for local Win32 tests. Default: Visual Studio 18 2026.

.EXAMPLE
    ./test.ps1
    ./test.ps1 dotnet
    ./test.ps1 win32 -Configuration Release
#>

param(
    [Parameter(Position = 0)]
    [ValidateSet('all', 'rust', 'dotnet', 'win32', 'pester', 'help')]
    [string]$Command = 'all',

    [ValidateSet('Release', 'Debug')]
    [string]$Configuration = 'Debug',

    [string]$Win32Generator = 'Visual Studio 18 2026'
)

$ErrorActionPreference = 'Stop'

$RustManifest = Join-Path $PSScriptRoot 'src' 'rust' 'Cargo.toml'
$DotnetSolution = Join-Path $PSScriptRoot 'src' 'dotnet' 'QsoRipper.slnx'
$Win32SourceDir = Join-Path $PSScriptRoot 'src' 'c' 'qsoripper-win32'
$Win32BuildDir = Join-Path $PSScriptRoot 'build' 'win32-tests'
$PesterTestsDir = Join-Path $PSScriptRoot 'tests'

function Write-Step([string]$Message) {
    Write-Host "`n=== $Message ===" -ForegroundColor Cyan
}

function New-TestCounts([int]$Passed = 0, [int]$Failed = 0, [int]$Skipped = 0) {
    [pscustomobject]@{
        Passed  = $Passed
        Failed  = $Failed
        Skipped = $Skipped
    }
}

function Add-TestResult([string]$Suite, [pscustomobject]$Counts) {
    if (-not $script:TestResults) { $script:TestResults = [System.Collections.Generic.List[object]]::new() }
    $script:TestResults.Add([pscustomobject]@{
            Suite   = $Suite
            Passed  = [int]$Counts.Passed
            Failed  = [int]$Counts.Failed
            Skipped = [int]$Counts.Skipped
        }) | Out-Null
}

function Invoke-TestStep([string]$Step, [string]$Executable, [string[]]$Arguments) {
    Write-Step $Step
    if (-not $script:TestTimings) { $script:TestTimings = [System.Collections.Generic.List[object]]::new() }
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $entry = [pscustomobject]@{ Step = $Step; Status = 'RUN'; Seconds = 0.0 }
    $code = 0
    try {
        & $Executable @Arguments
        $code = $LASTEXITCODE
    }
    finally {
        $sw.Stop()
        $entry.Seconds = [Math]::Round($sw.Elapsed.TotalSeconds, 2)
        $script:TestTimings.Add($entry) | Out-Null
    }

    if ($code -ne 0) {
        $entry.Status = 'FAIL'
        Write-Host "FAILED: $Step" -ForegroundColor Red
        exit $code
    }

    $entry.Status = 'OK'
}

function Invoke-TestStepWithCounts([string]$Step, [string]$Executable, [string[]]$Arguments, [string]$Suite, [scriptblock]$Parser) {
    Write-Step $Step
    if (-not $script:TestTimings) { $script:TestTimings = [System.Collections.Generic.List[object]]::new() }
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $entry = [pscustomobject]@{ Step = $Step; Status = 'RUN'; Seconds = 0.0 }
    $lines = [System.Collections.Generic.List[string]]::new()
    $code = 0
    try {
        & $Executable @Arguments 2>&1 | ForEach-Object {
            $line = $_.ToString()
            $lines.Add($line) | Out-Null
            Write-Host $line
        }
        $code = $LASTEXITCODE
    }
    finally {
        $sw.Stop()
        $entry.Seconds = [Math]::Round($sw.Elapsed.TotalSeconds, 2)
        $script:TestTimings.Add($entry) | Out-Null
    }

    $counts = & $Parser @($lines)
    if ($null -eq $counts) {
        $counts = New-TestCounts
    }

    if ($code -ne 0 -and [int]$counts.Failed -eq 0) {
        $counts.Failed = 1
    }

    Add-TestResult -Suite $Suite -Counts $counts

    if ($code -ne 0) {
        $entry.Status = 'FAIL'
        Write-Host "FAILED: $Step" -ForegroundColor Red
        exit $code
    }

    $entry.Status = 'OK'
}

function Measure-TestStep([string]$Step, [scriptblock]$Body) {
    Write-Step $Step
    if (-not $script:TestTimings) { $script:TestTimings = [System.Collections.Generic.List[object]]::new() }
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $entry = [pscustomobject]@{ Step = $Step; Status = 'RUN'; Seconds = 0.0 }
    $code = 0
    try {
        & $Body
        $code = $LASTEXITCODE
    }
    finally {
        $sw.Stop()
        $entry.Seconds = [Math]::Round($sw.Elapsed.TotalSeconds, 2)
        $script:TestTimings.Add($entry) | Out-Null
    }

    if ($code -ne 0) {
        $entry.Status = 'FAIL'
        Write-Host "FAILED: $Step" -ForegroundColor Red
        exit $code
    }

    $entry.Status = 'OK'
}

function Format-TestSeconds([double]$Seconds) {
    if ($Seconds -ge 60) {
        $m = [Math]::Floor($Seconds / 60)
        $s = $Seconds - ($m * 60)
        return ('{0}m{1:N1}s' -f $m, $s)
    }
    return ('{0:N2}s' -f $Seconds)
}

function Get-RustTestCounts([string[]]$Lines) {
    $passed = 0
    $failed = 0
    $skipped = 0
    foreach ($line in $Lines) {
        if ($line -match 'test result:\s+\w+\.\s+(?<passed>\d+) passed;\s+(?<failed>\d+) failed;\s+(?<ignored>\d+) ignored;') {
            $passed += [int]$Matches.passed
            $failed += [int]$Matches.failed
            $skipped += [int]$Matches.ignored
        }
    }
    New-TestCounts -Passed $passed -Failed $failed -Skipped $skipped
}

function Get-DotnetTestCounts([string[]]$Lines) {
    $passed = 0
    $failed = 0
    $skipped = 0
    foreach ($line in $Lines) {
        if ($line -match 'Failed:\s+(?<failed>\d+),\s+Passed:\s+(?<passed>\d+),\s+Skipped:\s+(?<skipped>\d+),') {
            $passed += [int]$Matches.passed
            $failed += [int]$Matches.failed
            $skipped += [int]$Matches.skipped
        }
    }
    New-TestCounts -Passed $passed -Failed $failed -Skipped $skipped
}

function Get-CtestCounts([string[]]$Lines) {
    foreach ($line in $Lines) {
        if ($line -match '(?<failed>\d+) tests failed out of (?<total>\d+)') {
            $failed = [int]$Matches.failed
            $total = [int]$Matches.total
            return New-TestCounts -Passed ($total - $failed) -Failed $failed
        }
    }
    New-TestCounts
}

function Get-ObjectIntProperty($Object, [string[]]$Names) {
    foreach ($name in $Names) {
        $property = $Object.PSObject.Properties[$name]
        if ($null -ne $property -and $null -ne $property.Value) {
            return [int]$property.Value
        }
    }
    return 0
}

function Get-PesterCounts($Result) {
    $passed = Get-ObjectIntProperty -Object $Result -Names @('PassedCount', 'Passed')
    $failed = Get-ObjectIntProperty -Object $Result -Names @('FailedCount', 'Failed')
    $skipped = Get-ObjectIntProperty -Object $Result -Names @('SkippedCount', 'Skipped')
    $skipped += Get-ObjectIntProperty -Object $Result -Names @('PendingCount', 'Pending')
    $skipped += Get-ObjectIntProperty -Object $Result -Names @('InconclusiveCount', 'Inconclusive')
    New-TestCounts -Passed $passed -Failed $failed -Skipped $skipped
}

function Write-TestResultSummary {
    if (-not $script:TestResults -or $script:TestResults.Count -eq 0) { return }
    $suiteGroups = @($script:TestResults | Group-Object Suite)
    $maxLen = ($suiteGroups | ForEach-Object { $_.Name.Length } | Measure-Object -Maximum).Maximum
    $maxLen = [Math]::Max([int]$maxLen, 5)
    $bar = '=' * ($maxLen + 45)
    Write-Host ''
    Write-Host $bar -ForegroundColor Cyan
    Write-Host (' TEST RESULT SUMMARY ({0} suites)' -f $suiteGroups.Count) -ForegroundColor Cyan
    Write-Host $bar -ForegroundColor Cyan
    $headerFmt = '  {0,-' + $maxLen + '} {1,8} {2,8} {3,8} {4,8}'
    Write-Host ($headerFmt -f 'SUITE', 'PASSED', 'FAILED', 'SKIPPED', 'TOTAL') -ForegroundColor Gray
    $totalPassed = 0
    $totalFailed = 0
    $totalSkipped = 0
    foreach ($group in $suiteGroups) {
        $passed = ($group.Group | Measure-Object -Property Passed -Sum).Sum
        $failed = ($group.Group | Measure-Object -Property Failed -Sum).Sum
        $skipped = ($group.Group | Measure-Object -Property Skipped -Sum).Sum
        $suiteTotal = $passed + $failed + $skipped
        $totalPassed += $passed
        $totalFailed += $failed
        $totalSkipped += $skipped
        $color = if ($failed -gt 0) { 'Red' } else { 'Green' }
        Write-Host ($headerFmt -f $group.Name, $passed, $failed, $skipped, $suiteTotal) -ForegroundColor $color
    }

    $grandTotal = $totalPassed + $totalFailed + $totalSkipped
    Write-Host $bar -ForegroundColor Cyan
    Write-Host ($headerFmt -f 'TOTAL', $totalPassed, $totalFailed, $totalSkipped, $grandTotal) -ForegroundColor Cyan
    Write-Host $bar -ForegroundColor Cyan
}

function Write-TestSummary {
    if (-not $script:TestTimings -or $script:TestTimings.Count -eq 0) { return }
    $total = ($script:TestTimings | Measure-Object -Property Seconds -Sum).Sum
    $maxLen = ($script:TestTimings | ForEach-Object { $_.Step.Length } | Measure-Object -Maximum).Maximum
    $maxLen = [Math]::Max([int]$maxLen, 4)
    $bar = '=' * ($maxLen + 30)
    Write-Host ''
    Write-Host $bar -ForegroundColor Cyan
    Write-Host (' TEST TIMING SUMMARY ({0} steps)' -f $script:TestTimings.Count) -ForegroundColor Cyan
    Write-Host $bar -ForegroundColor Cyan
    $headerFmt = '  {0,-6} {1,-' + $maxLen + '} {2,10}  {3,6}'
    Write-Host ($headerFmt -f 'STATUS', 'STEP', 'TIME', 'SHARE') -ForegroundColor Gray
    foreach ($e in $script:TestTimings) {
        $share = if ($total -gt 0) { ('{0,5:N1}%' -f (100.0 * $e.Seconds / $total)) } else { '    -' }
        $color = if ($e.Status -eq 'OK') { 'Green' } elseif ($e.Status -eq 'FAIL') { 'Red' } else { 'Yellow' }
        Write-Host ($headerFmt -f $e.Status, $e.Step, (Format-TestSeconds $e.Seconds), $share) -ForegroundColor $color
    }
    Write-Host $bar -ForegroundColor Cyan
    Write-Host (('  TOTAL  {0,-' + $maxLen + '} {1,10}') -f '', (Format-TestSeconds $total)) -ForegroundColor Cyan
    Write-Host $bar -ForegroundColor Cyan
}

function Test-Rust {
    Invoke-TestStepWithCounts 'Rust tests' cargo @('test', '--manifest-path', $RustManifest) 'Rust' ${function:Get-RustTestCounts}
}

function Test-Dotnet {
    Invoke-TestStepWithCounts ".NET tests ($Configuration)" dotnet @('test', $DotnetSolution, '-c', $Configuration) '.NET' ${function:Get-DotnetTestCounts}
}

function Test-Win32 {
    if (-not $IsWindows) {
        Write-Step 'Win32 tests'
        Write-Host 'Win32 tests require Windows; skipping on this platform.' -ForegroundColor Yellow
        Add-TestResult -Suite 'Win32' -Counts (New-TestCounts -Skipped 1)
        return
    }

    if (-not (Get-Command cmake -ErrorAction SilentlyContinue)) {
        Write-Step 'Win32 tests'
        Write-Host 'CMake not found. Install CMake and Visual Studio Build Tools 2026.' -ForegroundColor Red
        exit 1
    }

    if (-not (Test-Path -LiteralPath (Join-Path $Win32SourceDir 'CMakeLists.txt'))) {
        Write-Step 'Win32 tests'
        Write-Host "Source not found at $Win32SourceDir." -ForegroundColor Red
        exit 1
    }

    Measure-TestStep "Configuring Win32 tests ($Win32Generator)" {
        cmake -S $Win32SourceDir -B $Win32BuildDir -G $Win32Generator
    }

    Invoke-TestStep "Building Win32 tests ($Configuration)" cmake @(
        '--build', $Win32BuildDir,
        '--config', $Configuration,
        '--parallel'
    )

    Invoke-TestStepWithCounts "Running Win32 CTest ($Configuration)" ctest @(
        '--test-dir', $Win32BuildDir,
        '-C', $Configuration,
        '--output-on-failure'
    ) 'Win32' ${function:Get-CtestCounts}
}

function Test-Pester {
    $pester = Get-Module -ListAvailable Pester | Sort-Object Version -Descending | Select-Object -First 1
    if (-not $pester) {
        Write-Step 'Pester tests'
        Write-Host 'Pester not installed; skipping PowerShell tests. Install-Module Pester -Scope CurrentUser' -ForegroundColor Yellow
        Add-TestResult -Suite 'PowerShell' -Counts (New-TestCounts -Skipped 1)
        return
    }

    Measure-TestStep 'Pester tests' {
        Import-Module Pester -MinimumVersion $pester.Version -Force
        $result = Invoke-Pester -Path $PesterTestsDir -PassThru
        $counts = Get-PesterCounts -Result $result
        Add-TestResult -Suite 'PowerShell' -Counts $counts
        $failedCount = if ($null -ne $result.FailedCount) { $result.FailedCount } elseif ($null -ne $result.Failed) { $result.Failed } else { 0 }
        if ($failedCount -gt 0) {
            exit 1
        }
    }
}

function Test-All {
    Test-Rust
    Test-Dotnet
    Test-Win32
    Test-Pester
}

function Show-Help {
    Write-Host @"

QsoRipper Test Script

Usage: ./test.ps1 [command] [-Configuration Release|Debug] [-Win32Generator <generator>]

Commands:
  all       Run Rust, .NET, Win32, and Pester tests (default)
  rust      Run Rust workspace tests
  dotnet    Run .NET solution tests
  win32     Configure, build, and run Win32 CTest tests
  pester    Run PowerShell/Pester tests under tests/
  help      Show this help

Examples:
  ./test.ps1
  ./test.ps1 dotnet -Configuration Release
  ./test.ps1 win32

"@
}

try {
    switch ($Command) {
        'all'    { Test-All }
        'rust'   { Test-Rust }
        'dotnet' { Test-Dotnet }
        'win32'  { Test-Win32 }
        'pester' { Test-Pester }
        'help'   { Show-Help }
    }
}
finally {
    Write-TestResultSummary
    Write-TestSummary
}

Write-Host "`nDone." -ForegroundColor Green
