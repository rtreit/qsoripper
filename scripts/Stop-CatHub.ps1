<#
.SYNOPSIS
    Stop standalone CatHub processes after confirmation.
#>
[CmdletBinding(SupportsShouldProcess, ConfirmImpact = 'High')]
param()

$ErrorActionPreference = 'Stop'
$processes = Get-Process -Name 'cathub' -ErrorAction SilentlyContinue
if (-not $processes) {
    Write-Host 'CatHub is not running.'
    return
}

foreach ($process in $processes) {
    if ($PSCmdlet.ShouldProcess("CatHub PID $($process.Id)", 'Stop process')) {
        Stop-Process -Id $process.Id
    }
}
