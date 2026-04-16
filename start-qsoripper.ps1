#!/usr/bin/env pwsh
<#
.SYNOPSIS
    Starts the selected QsoRipper engine in the background.

.DESCRIPTION
    Builds the selected engine if needed, launches it as a background process,
    and records its PID and log paths under artifacts/run.
#>

param(
    [ValidateSet('rust', 'dotnet')]
    [string]$Engine = 'rust',
    [string]$ListenAddress,
    [ValidateSet('sqlite', 'memory')]
    [string]$Storage,
    [string]$SqlitePath,
    [string]$ConfigPath,
    [int]$StartupTimeoutSeconds = 30,
    [switch]$SkipBuild,
    [switch]$ForceRestart
)

$ErrorActionPreference = 'Stop'

$runtimeDirectory = Join-Path $PSScriptRoot 'artifacts' | Join-Path -ChildPath 'run'
$statePath = Join-Path $runtimeDirectory 'qsoripper-engine.json'
$stdoutPath = Join-Path $runtimeDirectory "qsoripper-$Engine.stdout.log"
$stderrPath = Join-Path $runtimeDirectory "qsoripper-$Engine.stderr.log"
$dotenvPath = Join-Path $PSScriptRoot '.env'
$rustManifestPath = Join-Path $PSScriptRoot 'src' | Join-Path -ChildPath 'rust' | Join-Path -ChildPath 'Cargo.toml'
$dotnetProjectPath = Join-Path $PSScriptRoot 'src' | Join-Path -ChildPath 'dotnet' | Join-Path -ChildPath 'QsoRipper.Engine.DotNet' | Join-Path -ChildPath 'QsoRipper.Engine.DotNet.csproj'
$dotnetDebugDllPath = Join-Path $PSScriptRoot 'src' | Join-Path -ChildPath 'dotnet' | Join-Path -ChildPath 'QsoRipper.Engine.DotNet' | Join-Path -ChildPath 'bin' | Join-Path -ChildPath 'Debug' | Join-Path -ChildPath 'net10.0' | Join-Path -ChildPath 'QsoRipper.Engine.DotNet.dll'
$dotnetReleaseDllPath = Join-Path $PSScriptRoot 'src' | Join-Path -ChildPath 'dotnet' | Join-Path -ChildPath 'QsoRipper.Engine.DotNet' | Join-Path -ChildPath 'bin' | Join-Path -ChildPath 'Release' | Join-Path -ChildPath 'net10.0' | Join-Path -ChildPath 'QsoRipper.Engine.DotNet.dll'
$binaryName = if ($IsWindows) { 'qsoripper-server.exe' } else { 'qsoripper-server' }
$serverBinaryPath = Join-Path $PSScriptRoot 'src' | Join-Path -ChildPath 'rust' | Join-Path -ChildPath 'target' | Join-Path -ChildPath 'debug' | Join-Path -ChildPath $binaryName

function Write-Info([string]$Message) {
    Write-Host $Message -ForegroundColor Cyan
}

function Get-DefaultListenAddress([string]$SelectedEngine) {
    if ($SelectedEngine -eq 'dotnet') {
        return '127.0.0.1:50052'
    }

    return '127.0.0.1:50051'
}

function Resolve-DotNetEngineDllPath {
    if (Test-Path -LiteralPath $dotnetDebugDllPath) {
        return $dotnetDebugDllPath
    }

    if (Test-Path -LiteralPath $dotnetReleaseDllPath) {
        return $dotnetReleaseDllPath
    }

    return $dotnetDebugDllPath
}

function Import-DotEnv([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path)) {
        return
    }

    foreach ($line in Get-Content -LiteralPath $Path) {
        $trimmed = $line.Trim()
        if ([string]::IsNullOrWhiteSpace($trimmed) -or $trimmed.StartsWith('#')) {
            continue
        }

        $parts = $line -split '=', 2
        if ($parts.Count -ne 2) {
            continue
        }

        $name = $parts[0].Trim()
        $value = $parts[1].Trim()
        if (
            ($value.StartsWith('"') -and $value.EndsWith('"')) -or
            ($value.StartsWith("'") -and $value.EndsWith("'"))
        ) {
            $value = $value.Substring(1, $value.Length - 2)
        }

        Set-Item -Path "Env:$name" -Value $value
    }
}

function Get-State {
    if (-not (Test-Path -LiteralPath $statePath)) {
        return $null
    }

    return Get-Content -LiteralPath $statePath -Raw | ConvertFrom-Json
}

function Get-TrackedProcess {
    $state = Get-State
    if ($null -eq $state) {
        return $null
    }

    $process = Get-Process -Id $state.pid -ErrorAction SilentlyContinue
    if ($null -eq $process) {
        Remove-Item -LiteralPath $statePath -Force -ErrorAction SilentlyContinue
        return $null
    }

    [pscustomobject]@{
        State = $state
        Process = $process
    }
}

function Get-ProbeTarget([string]$Address) {
    if ($Address -match '^\[(?<host>.+)\]:(?<port>\d+)$') {
        $probeHost = $Matches.host
        $probePort = [int]$Matches.port
    }
    elseif ($Address -match '^(?<host>[^:]+):(?<port>\d+)$') {
        $probeHost = $Matches.host
        $probePort = [int]$Matches.port
    }
    else {
        throw "Unsupported listen address format: $Address"
    }

    if ($probeHost -in @('0.0.0.0', '::', '[::]', '*', '+')) {
        $probeHost = '127.0.0.1'
    }

    [pscustomobject]@{
        Host = $probeHost
        Port = $probePort
    }
}

function Test-TcpEndpoint([string]$TargetHost, [int]$Port) {
    $client = [System.Net.Sockets.TcpClient]::new()
    try {
        $connectTask = $client.ConnectAsync($TargetHost, $Port)
        if (-not $connectTask.Wait([TimeSpan]::FromMilliseconds(500))) {
            return $false
        }

        return $client.Connected
    }
    catch {
        return $false
    }
    finally {
        $client.Dispose()
    }
}

function Get-LogTail([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path)) {
        return @()
    }

    return Get-Content -LiteralPath $Path -Tail 20
}

function Stop-TrackedProcess([int]$ProcessId) {
    $process = Get-Process -Id $ProcessId -ErrorAction SilentlyContinue
    if ($null -eq $process) {
        return
    }

    Stop-Process -Id $ProcessId

    for ($attempt = 0; $attempt -lt 50; $attempt++) {
        Start-Sleep -Milliseconds 200
        if ($null -eq (Get-Process -Id $ProcessId -ErrorAction SilentlyContinue)) {
            return
        }
    }

    throw "Timed out waiting for process $ProcessId to stop."
}

New-Item -ItemType Directory -Path $runtimeDirectory -Force | Out-Null
Import-DotEnv -Path $dotenvPath

if ([string]::IsNullOrWhiteSpace($ListenAddress)) {
    $ListenAddress = Get-DefaultListenAddress -SelectedEngine $Engine
}

if ([string]::IsNullOrWhiteSpace($Storage)) {
    $Storage = if ($Engine -eq 'dotnet') { 'memory' } else { 'sqlite' }
}

if ($Engine -eq 'dotnet' -and $Storage -ne 'memory') {
    throw 'The managed .NET engine helper currently supports only memory storage.'
}

if ($Engine -eq 'dotnet' -and [string]::IsNullOrWhiteSpace($ConfigPath)) {
    $ConfigPath = Join-Path $runtimeDirectory 'dotnet-engine.json'
}

$existing = Get-TrackedProcess
if ($null -ne $existing) {
    if (-not $ForceRestart) {
        Write-Host "QsoRipper is already running (PID $($existing.Process.Id)) at $($existing.State.listenAddress)." -ForegroundColor Yellow
        Write-Host "Stop it first with .\stop-qsoripper.ps1 or rerun with -ForceRestart." -ForegroundColor Yellow
        exit 0
    }

    Write-Info "Stopping existing QsoRipper process $($existing.Process.Id)."
    Stop-TrackedProcess -ProcessId $existing.Process.Id
    Remove-Item -LiteralPath $statePath -Force -ErrorAction SilentlyContinue
}

if ($ForceRestart) {
    # Kill any untracked qsoripper-server processes (e.g. started outside this script)
    $orphans = Get-Process -Name 'qsoripper-server' -ErrorAction SilentlyContinue
    foreach ($orphan in $orphans) {
        Write-Info "Stopping untracked qsoripper-server process $($orphan.Id)."
        Stop-TrackedProcess -ProcessId $orphan.Id
    }

    # On Windows the OS may briefly hold a file lock after process exit; wait for the
    # binary to become writable before starting the build.
    if ((Test-Path -LiteralPath $serverBinaryPath) -and $orphans) {
        for ($lockAttempt = 0; $lockAttempt -lt 20; $lockAttempt++) {
            try {
                [System.IO.File]::Open($serverBinaryPath, 'Open', 'ReadWrite', 'None').Dispose()
                break
            }
            catch {
                Start-Sleep -Milliseconds 250
            }
        }
    }
}

if (-not $SkipBuild) {
    if ($Engine -eq 'dotnet') {
        Write-Info 'Building QsoRipper .NET engine.'
        dotnet build $dotnetProjectPath -c Debug
    }
    else {
        Write-Info 'Building qsoripper-server.'
        cargo build --manifest-path $rustManifestPath -p qsoripper-server
    }

    if ($LASTEXITCODE -ne 0) {
        throw "Build failed with exit code $LASTEXITCODE."
    }
}

if ($Engine -eq 'dotnet') {
    $dotnetDllPath = Resolve-DotNetEngineDllPath
    if (-not (Test-Path -LiteralPath $dotnetDllPath)) {
        throw "Managed engine assembly not found at $dotnetDllPath."
    }
}
elseif (-not (Test-Path -LiteralPath $serverBinaryPath)) {
    throw "Server binary not found at $serverBinaryPath."
}

if ($Engine -eq 'dotnet') {
    $filePath = 'dotnet'
    $argumentList = @(
        $dotnetDllPath,
        '--listen', $ListenAddress
    )
}
else {
    $filePath = $serverBinaryPath
    $argumentList = @(
        '--listen', $ListenAddress,
        '--storage', $Storage
    )

    if ($Storage -eq 'sqlite' -and -not [string]::IsNullOrWhiteSpace($SqlitePath)) {
        $argumentList += @('--sqlite-path', $SqlitePath)
    }
}

if (-not [string]::IsNullOrWhiteSpace($ConfigPath)) {
    $argumentList += @('--config', $ConfigPath)
}

Write-Info "Starting $Engine QsoRipper engine on $ListenAddress."
$startProcessParameters = @{
    FilePath = $filePath
    ArgumentList = $argumentList
    WorkingDirectory = $PSScriptRoot
    RedirectStandardOutput = $stdoutPath
    RedirectStandardError = $stderrPath
    PassThru = $true
}

if ($IsWindows) {
    $startProcessParameters.WindowStyle = 'Hidden'
}

$process = Start-Process @startProcessParameters

$state = [pscustomobject]@{
    engine = $Engine
    pid = $process.Id
    listenAddress = $ListenAddress
    storage = $Storage
    sqlitePath = if ($Storage -eq 'sqlite') { $SqlitePath } else { $null }
    configPath = if ([string]::IsNullOrWhiteSpace($ConfigPath)) { $null } else { $ConfigPath }
    startedAtUtc = [DateTime]::UtcNow.ToString('O')
    stdoutPath = $stdoutPath
    stderrPath = $stderrPath
}
$state | ConvertTo-Json | Set-Content -LiteralPath $statePath

$probeTarget = Get-ProbeTarget -Address $ListenAddress
$deadline = [DateTime]::UtcNow.AddSeconds($StartupTimeoutSeconds)

while ([DateTime]::UtcNow -lt $deadline) {
    $runningProcess = Get-Process -Id $process.Id -ErrorAction SilentlyContinue
    if ($null -eq $runningProcess) {
        $stderrTail = Get-LogTail -Path $stderrPath
        $stdoutTail = Get-LogTail -Path $stdoutPath
        $details = @($stderrTail + $stdoutTail) -join [Environment]::NewLine
        throw "QsoRipper exited during startup.`n$details"
    }

    if (Test-TcpEndpoint -TargetHost $probeTarget.Host -Port $probeTarget.Port) {
        Write-Host "QsoRipper $Engine engine started in the background (PID $($process.Id))." -ForegroundColor Green
        Write-Host "Endpoint: http://$($probeTarget.Host):$($probeTarget.Port)" -ForegroundColor Green
        if ($Storage -eq 'sqlite' -and -not [string]::IsNullOrWhiteSpace($SqlitePath)) {
            Write-Host "SQLite file: $SqlitePath" -ForegroundColor Green
        }
        if ($Engine -eq 'dotnet' -and -not [string]::IsNullOrWhiteSpace($ConfigPath)) {
            Write-Host "Managed config: $ConfigPath" -ForegroundColor Green
        }
        Write-Host "Logs: $stdoutPath" -ForegroundColor Green
        exit 0
    }

    Start-Sleep -Milliseconds 250
}

Stop-TrackedProcess -ProcessId $process.Id
Remove-Item -LiteralPath $statePath -Force -ErrorAction SilentlyContinue
throw "QsoRipper did not open $($probeTarget.Host):$($probeTarget.Port) within $StartupTimeoutSeconds seconds."
