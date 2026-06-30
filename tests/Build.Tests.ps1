#!/usr/bin/env pwsh
<#
.SYNOPSIS
    Pester tests for build.ps1 quality gate parity with CI.
.DESCRIPTION
    Validates that local check-rust and check-dotnet functions include the
    same quality gates that CI enforces (coverage thresholds, vuln checks).
#>

$repoRoot = Split-Path -Parent $PSScriptRoot
$scriptPath = Join-Path $repoRoot 'build.ps1'
$global:BuildTestsScriptContent = Get-Content $scriptPath -Raw
$buildAndTestPath = Join-Path $repoRoot 'build-and-test.ps1'
$global:BuildTestsBuildAndTestContent = Get-Content $buildAndTestPath -Raw
$rustWorkflowPath = Join-Path $repoRoot '.github' 'workflows' 'rust-quality.yml'
$global:BuildTestsRustWorkflowContent = Get-Content $rustWorkflowPath -Raw
$dotnetWorkflowPath = Join-Path $repoRoot '.github' 'workflows' 'dotnet-quality.yml'
$global:BuildTestsDotnetWorkflowContent = Get-Content $dotnetWorkflowPath -Raw
$win32WorkflowPath = Join-Path $repoRoot '.github' 'workflows' 'win32-quality.yml'
$global:BuildTestsWin32WorkflowContent = Get-Content $win32WorkflowPath -Raw
$powershellWorkflowPath = Join-Path $repoRoot '.github' 'workflows' 'powershell-quality.yml'
$global:BuildTestsPowershellWorkflowContent = Get-Content $powershellWorkflowPath -Raw
$engineConformanceWorkflowPath = Join-Path $repoRoot '.github' 'workflows' 'engine-conformance.yml'
$global:BuildTestsEngineConformanceWorkflowContent = Get-Content $engineConformanceWorkflowPath -Raw
$win32MainPath = Join-Path $repoRoot 'src' 'c' 'qsoripper-win32' 'src' 'main.c'
$global:BuildTestsWin32MainContent = Get-Content $win32MainPath -Raw

# Extract function bodies for targeted checks
function Get-FunctionBody([string]$Content, [string]$FunctionName) {
    $pattern = "(?ms)function\s+$FunctionName\s*\{(.+?)^\}"
    if ($Content -match $pattern) { return $Matches[1] }
    return ''
}

$global:BuildTestsCheckRustBody = Get-FunctionBody $global:BuildTestsScriptContent 'Check-Rust'
$global:BuildTestsCheckDotnetBody = Get-FunctionBody $global:BuildTestsScriptContent 'Check-Dotnet'

function Get-CFunctionBody([string]$Content, [string]$FunctionSignature) {
    $escaped = [regex]::Escape($FunctionSignature)
    $pattern = "(?ms)$escaped\s*\{(.+?)^\}"
    if ($Content -match $pattern) { return $Matches[1] }
    return ''
}

$win32MainPath = Join-Path $repoRoot 'src' 'c' 'qsoripper-win32' 'src' 'main.c'
$global:BuildTestsWin32MainContent = Get-Content $win32MainPath -Raw
$global:BuildTestsLogQsoBody = Get-CFunctionBody $global:BuildTestsWin32MainContent 'static void LogQso(void)'

function global:Assert-BuildMatches([string]$Actual, [string]$Pattern) {
    if ($Actual -notmatch $Pattern) {
        throw "Expected text to match regex '$Pattern'."
    }
}

function global:Assert-BuildNotMatches([string]$Actual, [string]$Pattern) {
    if ($Actual -match $Pattern) {
        throw "Expected text not to match regex '$Pattern'."
    }
}

function global:Assert-BuildEquals($Actual, $Expected) {
    if ($Actual -ne $Expected) {
        throw "Expected '$Expected' but got '$Actual'."
    }
}

Describe 'build.ps1 Check-Rust CI parity (Bug #202)' {

    It 'runs tests with coverage via cargo-llvm-cov when available' {
        # Check-Rust must reference cargo-llvm-cov for coverage collection
        Assert-BuildMatches $global:BuildTestsCheckRustBody 'cargo-llvm-cov'
    }

    It 'checks Rust coverage against a threshold' {
        # Must reference a numeric threshold (80) for coverage validation
        Assert-BuildMatches $global:BuildTestsCheckRustBody '80'
    }

    It 'fails if Rust coverage is below threshold' {
        # Must have exit/throw logic tied to coverage check
        Assert-BuildMatches $global:BuildTestsCheckRustBody 'coverage.*threshold|threshold.*coverage|below.*threshold'
    }
}

Describe 'build.ps1 Rust coverage exclusion parity (Bug #269)' {

    It 'excludes qsoripper-ffi during local cargo llvm-cov runs' {
        Assert-BuildMatches $global:BuildTestsCheckRustBody "'--exclude', 'qsoripper-ffi'"
    }

    It 'matches CI ignore-filename-regex for stress and ffi' {
        Assert-BuildMatches $global:BuildTestsCheckRustBody "ignore-filename-regex 'qsoripper-\(stress\|ffi\)'"
    }

    It 'CI workflow still excludes qsoripper-ffi' {
        Assert-BuildMatches $global:BuildTestsRustWorkflowContent '--exclude qsoripper-ffi'
    }
}

Describe 'build.ps1 Check-Dotnet CI parity (Bug #202)' {

    It 'runs tests with coverage collection' {
        # Check-Dotnet must reference XPlat Code Coverage for coverage collection
        Assert-BuildMatches $global:BuildTestsCheckDotnetBody 'XPlat Code Coverage|Code Coverage'
    }

    It 'checks .NET coverage against a threshold' {
        # Must reference a numeric threshold (50) for coverage validation
        Assert-BuildMatches $global:BuildTestsCheckDotnetBody '50'
    }

    It 'fails if .NET coverage is below threshold' {
        Assert-BuildMatches $global:BuildTestsCheckDotnetBody 'coverage.*threshold|threshold.*coverage|below.*threshold'
    }

    It 'runs vulnerable package check' {
        # Must reference --vulnerable for package vulnerability scanning
        Assert-BuildMatches $global:BuildTestsCheckDotnetBody '--vulnerable'
    }
}

Describe '.github/workflows/dotnet-quality.yml vulnerable package gate (Bug #259)' {

    It 'fails the workflow when vulnerable packages are reported' {
        Assert-BuildMatches $global:BuildTestsDotnetWorkflowContent 'has the following vulnerable packages'
        Assert-BuildMatches $global:BuildTestsDotnetWorkflowContent 'exit 1'
    }
}

Describe 'Win32 CLI publish/discovery path contract (WIN32-BUG-2)' {

    It 'publishes CLI to artifacts\publish\qsoripper-cli\<Configuration>' {
        Assert-BuildMatches $global:BuildTestsScriptContent "'qsoripper-cli'"
    }

    It 'probes the qsoripper-cli directory from FindCliPath candidates' {
        Assert-BuildMatches $global:BuildTestsWin32MainContent 'qsoripper-cli'
        Assert-BuildNotMatches $global:BuildTestsWin32MainContent 'QsoRipper\.Cli\\\\%s\\\\(?:net10\.0\\\\)?QsoRipper\.Cli\.exe'
    }
}

Describe 'Local Visual Studio generator selection' {

    It 'does not fall back to the Visual Studio 2022 generator for local builds' {
        Assert-BuildNotMatches $global:BuildTestsScriptContent 'Visual Studio 17 2022'
    }

    It 'does not use the Visual Studio 2022 generator in Win32 CI' {
        Assert-BuildNotMatches $global:BuildTestsWin32WorkflowContent 'Visual Studio 17 2022'
    }
}

Describe 'PR test workflow coverage' {

    It 'runs Win32 CI through the shared test script' {
        Assert-BuildMatches $global:BuildTestsWin32WorkflowContent '\./test\.ps1 win32 -Configuration Release'
    }

    It 'runs Pester tests on pull requests' {
        Assert-BuildMatches $global:BuildTestsPowershellWorkflowContent 'pull_request:'
        Assert-BuildMatches $global:BuildTestsPowershellWorkflowContent '\./test\.ps1 pester'
    }

    It 'runs engine conformance on pull requests' {
        Assert-BuildMatches $global:BuildTestsEngineConformanceWorkflowContent 'pull_request:'
        Assert-BuildMatches $global:BuildTestsEngineConformanceWorkflowContent '\./tests/Run-EngineConformance\.ps1'
    }
}

Describe 'Local build-and-test CI coverage' {

    It 'runs build.ps1 check so formatting and coverage gates fail locally' {
        Assert-BuildMatches $global:BuildTestsBuildAndTestContent "build\.ps1'\) check -Configuration"
    }

    It 'runs test.ps1 all so Pester, Win32, and conformance suites fail locally' {
        Assert-BuildMatches $global:BuildTestsBuildAndTestContent "test\.ps1'\) all -Configuration"
    }
}

Describe 'Win32 LogQso shadowing regression (WIN32-BUG-1)' {

    It 'declares exactly one cmd buffer in LogQso' {
        Assert-BuildEquals ([regex]::Matches($global:BuildTestsLogQsoBody, 'char\s+cmd\s*\[\s*4096\s*\]\s*;').Count) 1
    }
}
