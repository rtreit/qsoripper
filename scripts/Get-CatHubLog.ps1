<#
.SYNOPSIS
    Tail the latest qsoripper-cathub rolling log.

.DESCRIPTION
    The daemon writes a daily-rolling tracing log (qsoripper-cathub.log.YYYY-MM-DD) to the
    user profile directory. This helper shows the most recent log file (optionally live).

.PARAMETER Tail
    Number of trailing lines to show. Default 80.

.PARAMETER Follow
    Follow the log live (like tail -f).

.EXAMPLE
    .\scripts\Get-CatHubLog.ps1 -Follow
#>
[CmdletBinding()]
param(
    [int]$Tail = 80,
    [switch]$Follow
)

$ErrorActionPreference = 'Stop'
$logDir = $env:USERPROFILE

if (-not (Test-Path $logDir)) {
    Write-Host "No log directory: $logDir" -ForegroundColor Yellow
    return
}

$latest = Get-ChildItem -Path $logDir -Filter 'qsoripper-cathub.log*' -File -ErrorAction SilentlyContinue |
    Sort-Object LastWriteTime -Descending |
    Select-Object -First 1

if (-not $latest) {
    Write-Host "No cathub log files in $logDir" -ForegroundColor Yellow
    return
}

Write-Host "Log: $($latest.FullName)" -ForegroundColor Cyan
if ($Follow) {
    Get-Content -Path $latest.FullName -Tail $Tail -Wait
} else {
    Get-Content -Path $latest.FullName -Tail $Tail
}
