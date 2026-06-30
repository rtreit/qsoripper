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
$scriptContent = Get-Content $scriptPath -Raw
$buildAndTestPath = Join-Path $repoRoot 'build-and-test.ps1'
$buildAndTestContent = Get-Content $buildAndTestPath -Raw
$rustWorkflowPath = Join-Path $repoRoot '.github' 'workflows' 'rust-quality.yml'
$rustWorkflowContent = Get-Content $rustWorkflowPath -Raw
$dotnetWorkflowPath = Join-Path $repoRoot '.github' 'workflows' 'dotnet-quality.yml'
$dotnetWorkflowContent = Get-Content $dotnetWorkflowPath -Raw
$win32WorkflowPath = Join-Path $repoRoot '.github' 'workflows' 'win32-quality.yml'
$win32WorkflowContent = Get-Content $win32WorkflowPath -Raw
$powershellWorkflowPath = Join-Path $repoRoot '.github' 'workflows' 'powershell-quality.yml'
$powershellWorkflowContent = Get-Content $powershellWorkflowPath -Raw
$engineConformanceWorkflowPath = Join-Path $repoRoot '.github' 'workflows' 'engine-conformance.yml'
$engineConformanceWorkflowContent = Get-Content $engineConformanceWorkflowPath -Raw
$win32MainPath = Join-Path $repoRoot 'src' 'c' 'qsoripper-win32' 'src' 'main.c'
$win32MainContent = Get-Content $win32MainPath -Raw

# Extract function bodies for targeted checks
function Get-FunctionBody([string]$Content, [string]$FunctionName) {
    $pattern = "(?ms)function\s+$FunctionName\s*\{(.+?)^\}"
    if ($Content -match $pattern) { return $Matches[1] }
    return ''
}

$checkRustBody = Get-FunctionBody $scriptContent 'Check-Rust'
$checkDotnetBody = Get-FunctionBody $scriptContent 'Check-Dotnet'

function Get-CFunctionBody([string]$Content, [string]$FunctionSignature) {
    $escaped = [regex]::Escape($FunctionSignature)
    $pattern = "(?ms)$escaped\s*\{(.+?)^\}"
    if ($Content -match $pattern) { return $Matches[1] }
    return ''
}

$win32MainPath = Join-Path $repoRoot 'src' 'c' 'qsoripper-win32' 'src' 'main.c'
$win32MainContent = Get-Content $win32MainPath -Raw
$logQsoBody = Get-CFunctionBody $win32MainContent 'static void LogQso(void)'

function Assert-Matches([string]$Actual, [string]$Pattern) {
    if ($Actual -notmatch $Pattern) {
        throw "Expected text to match regex '$Pattern'."
    }
}

function Assert-NotMatches([string]$Actual, [string]$Pattern) {
    if ($Actual -match $Pattern) {
        throw "Expected text not to match regex '$Pattern'."
    }
}

function Assert-Equals($Actual, $Expected) {
    if ($Actual -ne $Expected) {
        throw "Expected '$Expected' but got '$Actual'."
    }
}

Describe 'build.ps1 Check-Rust CI parity (Bug #202)' {

    It 'runs tests with coverage via cargo-llvm-cov when available' {
        # Check-Rust must reference cargo-llvm-cov for coverage collection
        Assert-Matches $checkRustBody 'cargo-llvm-cov'
    }

    It 'checks Rust coverage against a threshold' {
        # Must reference a numeric threshold (80) for coverage validation
        Assert-Matches $checkRustBody '80'
    }

    It 'fails if Rust coverage is below threshold' {
        # Must have exit/throw logic tied to coverage check
        Assert-Matches $checkRustBody 'coverage.*threshold|threshold.*coverage|below.*threshold'
    }
}

Describe 'build.ps1 Rust coverage exclusion parity (Bug #269)' {

    It 'excludes qsoripper-ffi during local cargo llvm-cov runs' {
        Assert-Matches $checkRustBody "'--exclude', 'qsoripper-ffi'"
    }

    It 'matches CI ignore-filename-regex for stress and ffi' {
        Assert-Matches $checkRustBody "ignore-filename-regex 'qsoripper-\(stress\|ffi\)'"
    }

    It 'CI workflow still excludes qsoripper-ffi' {
        Assert-Matches $rustWorkflowContent '--exclude qsoripper-ffi'
    }
}

Describe 'build.ps1 Check-Dotnet CI parity (Bug #202)' {

    It 'runs tests with coverage collection' {
        # Check-Dotnet must reference XPlat Code Coverage for coverage collection
        Assert-Matches $checkDotnetBody 'XPlat Code Coverage|Code Coverage'
    }

    It 'checks .NET coverage against a threshold' {
        # Must reference a numeric threshold (50) for coverage validation
        Assert-Matches $checkDotnetBody '50'
    }

    It 'fails if .NET coverage is below threshold' {
        Assert-Matches $checkDotnetBody 'coverage.*threshold|threshold.*coverage|below.*threshold'
    }

    It 'runs vulnerable package check' {
        # Must reference --vulnerable for package vulnerability scanning
        Assert-Matches $checkDotnetBody '--vulnerable'
    }
}

Describe '.github/workflows/dotnet-quality.yml vulnerable package gate (Bug #259)' {

    It 'fails the workflow when vulnerable packages are reported' {
        Assert-Matches $dotnetWorkflowContent 'has the following vulnerable packages'
        Assert-Matches $dotnetWorkflowContent 'exit 1'
    }
}

Describe 'Win32 CLI publish/discovery path contract (WIN32-BUG-2)' {

    It 'publishes CLI to artifacts\publish\qsoripper-cli\<Configuration>' {
        Assert-Matches $scriptContent "'qsoripper-cli'"
    }

    It 'probes the qsoripper-cli directory from FindCliPath candidates' {
        Assert-Matches $win32MainContent 'qsoripper-cli'
        Assert-NotMatches $win32MainContent 'QsoRipper\.Cli\\\\%s\\\\(?:net10\.0\\\\)?QsoRipper\.Cli\.exe'
    }
}

Describe 'Local Visual Studio generator selection' {

    It 'does not fall back to the Visual Studio 2022 generator for local builds' {
        Assert-NotMatches $scriptContent 'Visual Studio 17 2022'
    }

    It 'does not use the Visual Studio 2022 generator in Win32 CI' {
        Assert-NotMatches $win32WorkflowContent 'Visual Studio 17 2022'
    }
}

Describe 'PR test workflow coverage' {

    It 'runs Win32 CI through the shared test script' {
        Assert-Matches $win32WorkflowContent '\./test\.ps1 win32 -Configuration Release'
    }

    It 'runs Pester tests on pull requests' {
        Assert-Matches $powershellWorkflowContent 'pull_request:'
        Assert-Matches $powershellWorkflowContent '\./test\.ps1 pester'
    }

    It 'runs engine conformance on pull requests' {
        Assert-Matches $engineConformanceWorkflowContent 'pull_request:'
        Assert-Matches $engineConformanceWorkflowContent '\./tests/Run-EngineConformance\.ps1'
    }
}

Describe 'Local build-and-test CI coverage' {

    It 'runs build.ps1 check so formatting and coverage gates fail locally' {
        Assert-Matches $buildAndTestContent "build\.ps1'\) check -Configuration"
    }

    It 'runs test.ps1 all so Pester, Win32, and conformance suites fail locally' {
        Assert-Matches $buildAndTestContent "test\.ps1'\) all -Configuration"
    }
}

Describe 'Win32 LogQso shadowing regression (WIN32-BUG-1)' {

    It 'declares exactly one cmd buffer in LogQso' {
        Assert-Equals ([regex]::Matches($logQsoBody, 'char\s+cmd\s*\[\s*4096\s*\]\s*;').Count) 1
    }
}
