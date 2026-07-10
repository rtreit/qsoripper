<#
.SYNOPSIS
    Stop the qsoripper-cathub daemon.

.DESCRIPTION
    Finds the running qsoripper-cathub process and stops it. The daemon's Ctrl+C / shutdown
    handler attempts to send RX; to the radio so a stop never leaves the transmitter keyed;
    the ptt_max_tx_ms ceiling and the radio's own TX timeout are the ultimate backstops.

.EXAMPLE
    .\scripts\Stop-CatHub.ps1
#>
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'

$procs = Get-Process -Name 'qsoripper-cathub' -ErrorAction SilentlyContinue
if (-not $procs) {
    Write-Host 'cathub is not running.' -ForegroundColor Yellow
    return
}

foreach ($p in $procs) {
    Write-Host "Stopping cathub (PID $($p.Id))" -ForegroundColor Cyan
    Stop-Process -Id $p.Id
}
Write-Host 'cathub stopped.' -ForegroundColor Green
