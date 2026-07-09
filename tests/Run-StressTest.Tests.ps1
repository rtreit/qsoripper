#!/usr/bin/env pwsh
<#
.SYNOPSIS
    Pester tests for Run-StressTest.ps1 quality gates.
.DESCRIPTION
    Validates that the stress test harness properly checks build exit codes
    and includes all error categories in pass/fail decisions.
#>

$scriptPath = Join-Path $PSScriptRoot 'Run-StressTest.ps1'
$global:StressTestsScriptContent = Get-Content $scriptPath -Raw

function global:Assert-StressMatches([string]$Actual, [string]$Pattern) {
    if ($Actual -notmatch $Pattern) {
        throw "Expected text to match regex '$Pattern'."
    }
}

function global:Assert-StressEquals($Actual, $Expected) {
    if ($Actual -ne $Expected) {
        throw "Expected '$Expected' but got '$Actual'."
    }
}

function global:Assert-StressNotNullOrEmpty($Actual) {
    if ($null -eq $Actual -or ($Actual -is [string] -and [string]::IsNullOrEmpty($Actual))) {
        throw 'Expected value to be non-null and non-empty.'
    }
}

Describe 'Run-StressTest.ps1 build exit-code checks (Bug #203)' {

    It 'checks LASTEXITCODE after cargo build -p qsoripper-server' {
        Assert-StressMatches $global:StressTestsScriptContent 'cargo build -p qsoripper-server'
        $lines = $global:StressTestsScriptContent -split "`n"
        $match = $lines | Select-String -SimpleMatch 'cargo build -p qsoripper-server' | Select-Object -First 1
        $cargoBuildIdx = $match.LineNumber - 1
        Assert-StressNotNullOrEmpty $cargoBuildIdx
        # Search the next 5 lines for a LASTEXITCODE check
        $found = $false
        for ($i = $cargoBuildIdx + 1; $i -le [Math]::Min($cargoBuildIdx + 5, $lines.Count - 1); $i++) {
            if ($lines[$i] -match '\$LASTEXITCODE') { $found = $true; break }
        }
        Assert-StressEquals $found $true
    }

    It 'checks LASTEXITCODE after cargo test --no-run' {
        $lines = $global:StressTestsScriptContent -split "`n"
        $match = $lines | Select-String -SimpleMatch 'cargo test --test stress_test --no-run' | Select-Object -First 1
        $cargoTestIdx = $match.LineNumber - 1
        Assert-StressNotNullOrEmpty $cargoTestIdx
        $found = $false
        for ($i = $cargoTestIdx + 1; $i -le [Math]::Min($cargoTestIdx + 5, $lines.Count - 1); $i++) {
            if ($lines[$i] -match '\$LASTEXITCODE') { $found = $true; break }
        }
        Assert-StressEquals $found $true
    }

    It 'checks LASTEXITCODE after dotnet build' {
        $lines = $global:StressTestsScriptContent -split "`n"
        $dotnetBuildIdx = ($lines | Select-String -SimpleMatch 'dotnet build --nologo').LineNumber - 1
        Assert-StressNotNullOrEmpty $dotnetBuildIdx
        $found = $false
        for ($i = $dotnetBuildIdx + 1; $i -le [Math]::Min($dotnetBuildIdx + 5, $lines.Count - 1); $i++) {
            if ($lines[$i] -match '\$LASTEXITCODE') { $found = $true; break }
        }
        Assert-StressEquals $found $true
    }
}

Describe 'Run-StressTest.ps1 pass/fail logic (Bug #204)' {

    It 'includes grpcInternalCount in pass/fail decision' {
        # The final pass/fail block must reference grpcInternalCount
        $passFailBlock = ($global:StressTestsScriptContent -split 'Report written to')[1]
        Assert-StressMatches $passFailBlock 'grpcInternalCount'
    }

    It 'parses Other errors count from client output' {
        # Must have a regex matching "Other errors:" to extract the count
        Assert-StressMatches $global:StressTestsScriptContent "Other errors:"
    }

    It 'includes transport/client error count in pass/fail decision' {
        $passFailBlock = ($global:StressTestsScriptContent -split 'Report written to')[1]
        Assert-StressMatches $passFailBlock 'clientErrorCount'
    }

    It 'includes dotnet exit code in pass/fail decision' {
        $passFailBlock = ($global:StressTestsScriptContent -split 'Report written to')[1]
        Assert-StressMatches $passFailBlock 'dotnetExitCode'
    }
}
