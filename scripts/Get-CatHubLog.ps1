<#
.SYNOPSIS
    Show the latest standalone CatHub rolling log.
#>
[CmdletBinding()]
param(
    [int]$Tail = 80,
    [switch]$Follow
)

$ErrorActionPreference = 'Stop'
if ($env:CATHUB_LOG_DIR) {
    $logDir = $env:CATHUB_LOG_DIR
}
elseif ($IsWindows -or $env:OS -eq 'Windows_NT') {
    $logDir = Join-Path $env:LOCALAPPDATA 'cathub\logs'
}
elseif ($env:XDG_STATE_HOME) {
    $logDir = Join-Path $env:XDG_STATE_HOME 'cathub'
}
else {
    $logDir = Join-Path $env:HOME '.local/state/cathub'
}

$latest = Get-ChildItem -LiteralPath $logDir -Filter 'cathub.log*' -File -ErrorAction SilentlyContinue |
    Sort-Object LastWriteTime -Descending |
    Select-Object -First 1
if (-not $latest) {
    Write-Host "No CatHub logs found in $logDir."
    return
}
if ($Follow) {
    Get-Content -LiteralPath $latest.FullName -Tail $Tail -Wait
}
else {
    Get-Content -LiteralPath $latest.FullName -Tail $Tail
}
