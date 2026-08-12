#!/usr/bin/env pwsh

$global:CatHubBootstrapRepoRoot = Split-Path -Parent $PSScriptRoot
$global:CatHubBootstrapHelperPath = Join-Path $global:CatHubBootstrapRepoRoot 'scripts' 'CatHubBootstrap.ps1'
$global:CatHubBootstrapLauncherPath = Join-Path $global:CatHubBootstrapRepoRoot 'launcher.ps1'

Describe 'launcher CatHub bootstrap' {
    BeforeAll {
        . $global:CatHubBootstrapHelperPath
    }

    It 'accepts only the supported CatHub version range' {
        if (-not (Test-CompatibleCatHubVersion 'cathub 0.2.0')) {
            throw 'CatHub 0.2.0 must be compatible.'
        }
        if (-not (Test-CompatibleCatHubVersion 'cathub 0.2.19-beta.1')) {
            throw 'CatHub 0.2 prerelease versions must be compatible.'
        }
        if (Test-CompatibleCatHubVersion 'cathub 0.1.2') {
            throw 'CatHub 0.1.2 must be incompatible.'
        }
        if (Test-CompatibleCatHubVersion 'cathub 0.3.0') {
            throw 'CatHub 0.3.0 must be incompatible.'
        }
        if (Test-CompatibleCatHubVersion 'unexpected output') {
            throw 'Malformed version output must be incompatible.'
        }
    }

    It 'maps a sibling repository to the selected Cargo profile' {
        $repositoriesRoot = Join-Path ([System.IO.Path]::GetTempPath()) 'repos'
        $qsoRoot = Join-Path $repositoriesRoot 'qsoripper'
        $info = Get-SiblingCatHubBuildInfo -QsoRipperRoot $qsoRoot -Profile Release
        $expectedName = if ($IsWindows -or $env:OS -eq 'Windows_NT') { 'cathub.exe' } else { 'cathub' }
        $catHubRoot = Join-Path $repositoriesRoot 'cathub'

        $expectedManifest = Join-Path $catHubRoot 'Cargo.toml'
        $expectedExecutable = Join-Path (Join-Path (Join-Path $catHubRoot 'target') 'release') $expectedName
        if ($info.ManifestPath -ne $expectedManifest) {
            throw "Expected manifest path $expectedManifest, but received $($info.ManifestPath)."
        }
        if ($info.ExecutablePath -ne $expectedExecutable) {
            throw "Expected executable path $expectedExecutable, but received $($info.ExecutablePath)."
        }
    }

    It 'wires the bootstrap into launcher startup' {
        $launcher = Get-Content $global:CatHubBootstrapLauncherPath -Raw

        if ($launcher -notmatch 'CatHubBootstrap\.ps1') {
            throw 'launcher.ps1 must load the CatHub bootstrap helper.'
        }
        if ($launcher -notmatch 'Initialize-CatHubExecutable') {
            throw 'launcher.ps1 must initialize the CatHub executable.'
        }
    }

    It 'builds a missing sibling executable and selects it for this process' {
        $repositoriesRoot = Join-Path $TestDrive 'repos'
        $qsoRoot = Join-Path $repositoriesRoot 'qsoripper'
        $catHubRoot = Join-Path $repositoriesRoot 'cathub'
        $manifestPath = Join-Path $catHubRoot 'Cargo.toml'
        $previousExecutable = $env:CATHUB_EXECUTABLE

        $null = New-Item -ItemType Directory -Path $qsoRoot -Force
        $null = New-Item -ItemType Directory -Path $catHubRoot -Force
        $null = New-Item -ItemType File -Path $manifestPath -Force

        Mock Test-CompatibleCatHubExecutable { $false }
        Mock Invoke-SiblingCatHubBuild {}
        Mock Get-CatHubVersionOutput { 'cathub 0.2.0' }

        try {
            Remove-Item Env:CATHUB_EXECUTABLE -ErrorAction SilentlyContinue
            $resolved = Initialize-CatHubExecutable -QsoRipperRoot $qsoRoot -Profile Release
            $expected = (Get-SiblingCatHubBuildInfo -QsoRipperRoot $qsoRoot -Profile Release).ExecutablePath

            if ($resolved -ne $expected) {
                throw "Expected resolved path $expected, but received $resolved."
            }
            if ($env:CATHUB_EXECUTABLE -ne $expected) {
                throw 'The process CatHub executable path was not set.'
            }
            Assert-MockCalled Invoke-SiblingCatHubBuild -Times 1 -Exactly -Scope It
        }
        finally {
            if ($null -eq $previousExecutable) {
                Remove-Item Env:CATHUB_EXECUTABLE -ErrorAction SilentlyContinue
            }
            else {
                $env:CATHUB_EXECUTABLE = $previousExecutable
            }
        }
    }
}
