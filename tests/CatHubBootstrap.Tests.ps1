#!/usr/bin/env pwsh

$repoRoot = Split-Path -Parent $PSScriptRoot
$helperPath = Join-Path $repoRoot 'scripts' 'CatHubBootstrap.ps1'
$launcherPath = Join-Path $repoRoot 'launcher.ps1'

Describe 'launcher CatHub bootstrap' {
    BeforeAll {
        . $helperPath
    }

    It 'accepts only the supported CatHub version range' {
        Test-CompatibleCatHubVersion 'cathub 0.2.0' | Should Be $true
        Test-CompatibleCatHubVersion 'cathub 0.2.19-beta.1' | Should Be $true
        Test-CompatibleCatHubVersion 'cathub 0.1.2' | Should Be $false
        Test-CompatibleCatHubVersion 'cathub 0.3.0' | Should Be $false
        Test-CompatibleCatHubVersion 'unexpected output' | Should Be $false
    }

    It 'maps a sibling repository to the selected Cargo profile' {
        $repositoriesRoot = Join-Path ([System.IO.Path]::GetTempPath()) 'repos'
        $qsoRoot = Join-Path $repositoriesRoot 'qsoripper'
        $info = Get-SiblingCatHubBuildInfo -QsoRipperRoot $qsoRoot -Profile Release
        $expectedName = if ($IsWindows -or $env:OS -eq 'Windows_NT') { 'cathub.exe' } else { 'cathub' }
        $catHubRoot = Join-Path $repositoriesRoot 'cathub'

        $info.ManifestPath | Should Be (Join-Path $catHubRoot 'Cargo.toml')
        $info.ExecutablePath | Should Be (Join-Path (Join-Path (Join-Path $catHubRoot 'target') 'release') $expectedName)
    }

    It 'wires the bootstrap into launcher startup' {
        $launcher = Get-Content $launcherPath -Raw

        $launcher | Should Match 'CatHubBootstrap\.ps1'
        $launcher | Should Match 'Initialize-CatHubExecutable'
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

            $resolved | Should Be $expected
            $env:CATHUB_EXECUTABLE | Should Be $expected
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
