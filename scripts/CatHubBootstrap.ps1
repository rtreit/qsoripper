#!/usr/bin/env pwsh

function Test-CompatibleCatHubVersion {
    param([AllowEmptyString()][string]$VersionOutput)

    $match = [regex]::Match(
        $VersionOutput.Trim(),
        '^cathub\s+(?<major>\d+)\.(?<minor>\d+)\.(?<patch>\d+)(?:[-+]\S+)?$',
        [System.Text.RegularExpressions.RegexOptions]::IgnoreCase)
    if (-not $match.Success) {
        return $false
    }

    return [int]$match.Groups['major'].Value -eq 0 -and
        [int]$match.Groups['minor'].Value -eq 2
}

function Get-SiblingCatHubBuildInfo {
    param(
        [Parameter(Mandatory)][string]$QsoRipperRoot,
        [Parameter(Mandatory)][ValidateSet('Release', 'Debug')][string]$Profile
    )

    $repositoriesRoot = Split-Path ([System.IO.Path]::GetFullPath($QsoRipperRoot)) -Parent
    $catHubRoot = Join-Path $repositoriesRoot 'cathub'
    $manifestPath = Join-Path $catHubRoot 'Cargo.toml'
    $profileDirectory = $Profile.ToLowerInvariant()
    $executableName = if ($IsWindows -or $env:OS -eq 'Windows_NT') { 'cathub.exe' } else { 'cathub' }
    $targetDirectory = Join-Path (Join-Path $catHubRoot 'target') $profileDirectory

    [pscustomobject]@{
        RootPath       = $catHubRoot
        ManifestPath   = $manifestPath
        ExecutablePath = Join-Path $targetDirectory $executableName
    }
}

function Get-CatHubVersionOutput {
    param([Parameter(Mandatory)][string]$ExecutablePath)

    if (-not (Test-Path -LiteralPath $ExecutablePath -PathType Leaf)) {
        return $null
    }

    try {
        $output = & $ExecutablePath --version 2>$null
        if ($LASTEXITCODE -ne 0) {
            return $null
        }
        return ($output -join "`n").Trim()
    }
    catch {
        return $null
    }
}

function Test-CompatibleCatHubExecutable {
    param([Parameter(Mandatory)][string]$ExecutablePath)

    $versionOutput = Get-CatHubVersionOutput -ExecutablePath $ExecutablePath
    return $null -ne $versionOutput -and (Test-CompatibleCatHubVersion $versionOutput)
}

function Invoke-SiblingCatHubBuild {
    param(
        [Parameter(Mandatory)][string]$ManifestPath,
        [Parameter(Mandatory)][ValidateSet('Release', 'Debug')][string]$Profile
    )

    $cargo = Get-Command cargo -ErrorAction SilentlyContinue
    if ($null -eq $cargo) {
        throw 'CatHub must be built, but cargo is not available on PATH.'
    }

    $arguments = @(
        'build',
        '--manifest-path', $ManifestPath,
        '--locked',
        '-p', 'cathub'
    )
    if ($Profile -eq 'Release') {
        $arguments += '--release'
    }

    & $cargo.Source @arguments
    if ($LASTEXITCODE -ne 0) {
        throw "CatHub build failed with exit code $LASTEXITCODE."
    }
}

function Initialize-CatHubExecutable {
    param(
        [Parameter(Mandatory)][string]$QsoRipperRoot,
        [Parameter(Mandatory)][ValidateSet('Release', 'Debug')][string]$Profile
    )

    if ($env:CATHUB_EXECUTABLE -and
        (Test-CompatibleCatHubExecutable -ExecutablePath $env:CATHUB_EXECUTABLE)) {
        return $env:CATHUB_EXECUTABLE
    }

    $bundledName = if ($IsWindows -or $env:OS -eq 'Windows_NT') { 'cathub.exe' } else { 'cathub' }
    $bundledPath = Join-Path $QsoRipperRoot "artifacts/publish/cathub/$Profile/$bundledName"
    if (Test-CompatibleCatHubExecutable -ExecutablePath $bundledPath) {
        $env:CATHUB_EXECUTABLE = $bundledPath
        return $bundledPath
    }

    $sibling = Get-SiblingCatHubBuildInfo -QsoRipperRoot $QsoRipperRoot -Profile $Profile
    if (-not (Test-Path -LiteralPath $sibling.ManifestPath -PathType Leaf)) {
        return $null
    }

    if (-not (Test-CompatibleCatHubExecutable -ExecutablePath $sibling.ExecutablePath)) {
        Write-Host "Building compatible CatHub from $($sibling.RootPath)"
        Invoke-SiblingCatHubBuild -ManifestPath $sibling.ManifestPath -Profile $Profile
    }

    $versionOutput = Get-CatHubVersionOutput -ExecutablePath $sibling.ExecutablePath
    if (-not (Test-CompatibleCatHubVersion $versionOutput)) {
        $reported = if ($versionOutput) { $versionOutput } else { 'no version response' }
        throw "CatHub build is incompatible. Expected 0.2.x, but received: $reported"
    }

    $env:CATHUB_EXECUTABLE = $sibling.ExecutablePath
    return $sibling.ExecutablePath
}
