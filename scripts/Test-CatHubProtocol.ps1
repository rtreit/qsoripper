[CmdletBinding()]
param(
    [string]$CatHubRoot = (Join-Path $PSScriptRoot '..\..\cathub')
)

$ErrorActionPreference = 'Stop'

$qsoRipperRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$catHubRoot = [System.IO.Path]::GetFullPath($CatHubRoot)
$qsoRipperProto = Join-Path $qsoRipperRoot 'proto\services'
$catHubProto = Join-Path $catHubRoot 'crates\cathub-protocol\proto\services'

if (-not (Test-Path -LiteralPath $catHubProto -PathType Container)) {
    throw "CatHub protocol directory not found: $catHubProto"
}

$expected = Get-ChildItem -LiteralPath $catHubProto -Filter '*.proto' -File |
    Sort-Object Name
$actualNames = Get-ChildItem -LiteralPath $qsoRipperProto -Filter '*winkeyer*.proto' -File |
    Select-Object -ExpandProperty Name

$missing = @($expected.Name | Where-Object { $_ -notin $actualNames })
if ($missing.Count -gt 0) {
    throw "QsoRipper is missing CatHub protocol files: $($missing -join ', ')"
}

$different = foreach ($file in $expected) {
    $qsoRipperFile = Join-Path $qsoRipperProto $file.Name
    $catHubHash = (Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash
    $qsoRipperHash = (Get-FileHash -LiteralPath $qsoRipperFile -Algorithm SHA256).Hash
    if ($catHubHash -ne $qsoRipperHash) {
        $file.Name
    }
}

if (@($different).Count -gt 0) {
    throw "QsoRipper's CatHub protocol snapshot differs: $($different -join ', ')"
}

$dependency = Get-Content -LiteralPath (Join-Path $qsoRipperRoot 'config\cathub-dependency.json') -Raw |
    ConvertFrom-Json
Write-Host "CatHub protocol snapshot matches $($dependency.repository) protocol $($dependency.protocolVersion)."
