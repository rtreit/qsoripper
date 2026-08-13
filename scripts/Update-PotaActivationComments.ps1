<#
.SYNOPSIS
    Add visible POTA activation comments to recent QSO records.

.DESCRIPTION
    Finds recent records that contain MY_POTA_REF metadata.
    The script also supports legacy MY_SIG and MY_SIG_INFO metadata.
    It resolves park names from the QuickPOTA park directory.
    Existing comments are preserved unless ReplaceExistingComment is set.
    The default operation shows a preview and does not change records.

.EXAMPLE
    .\scripts\Update-PotaActivationComments.ps1

    Preview changes for the last 10 days.

.EXAMPLE
    .\scripts\Update-PotaActivationComments.ps1 -Days 30 -Apply

    Update matching records from the last 30 days.

.EXAMPLE
    .\scripts\Update-PotaActivationComments.ps1 -Apply -Sync

    Update matching records and then start QRZ synchronization.

.EXAMPLE
    .\scripts\Update-PotaActivationComments.ps1 -ReplaceExistingComment -Apply

    Replace each matching comment with the generated activation comment.
#>
[CmdletBinding()]
param(
    [ValidateRange(1, 3650)]
    [int]$Days = 10,

    [ValidateRange(1, 100000)]
    [int]$Limit = 10000,

    [string]$CliPath,

    [string]$ParkCsvPath,

    [ValidateSet('local-rust', 'local-dotnet')]
    [string]$Engine = 'local-rust',

    [switch]$ReplaceExistingComment,

    [switch]$Apply,

    [switch]$Sync
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Resolve-QsoRipperCliPath {
    param([string]$RequestedPath)

    if (-not [string]::IsNullOrWhiteSpace($RequestedPath)) {
        return (Resolve-Path -LiteralPath $RequestedPath).Path
    }

    $repositoryRoot = Split-Path -Parent $PSScriptRoot
    $executableName = if ($IsWindows -or $env:OS -eq 'Windows_NT') {
        'QsoRipper.Cli.exe'
    }
    else {
        'QsoRipper.Cli'
    }

    $candidate = Join-Path $repositoryRoot "artifacts\publish\qsoripper-cli\Release\$executableName"
    if (Test-Path -LiteralPath $candidate) {
        return (Resolve-Path -LiteralPath $candidate).Path
    }

    throw "QsoRipper CLI was not found. Use -CliPath to select the executable."
}

function Resolve-ParkCsvPath {
    param([string]$RequestedPath)

    if (-not [string]::IsNullOrWhiteSpace($RequestedPath)) {
        return (Resolve-Path -LiteralPath $RequestedPath).Path
    }

    $candidates = [System.Collections.Generic.List[string]]::new()
    $localData = [Environment]::GetFolderPath([Environment+SpecialFolder]::LocalApplicationData)
    if (-not [string]::IsNullOrWhiteSpace($localData)) {
        $candidates.Add((Join-Path $localData 'QuickPOTA\all_parks_ext.csv'))
    }

    $repositoryRoot = Split-Path -Parent $PSScriptRoot
    $repositoryParent = Split-Path -Parent $repositoryRoot
    $candidates.Add((Join-Path $repositoryParent 'QuickPOTA\data\all_parks_ext.csv'))

    foreach ($candidate in $candidates) {
        if (Test-Path -LiteralPath $candidate) {
            return (Resolve-Path -LiteralPath $candidate).Path
        }
    }

    throw "The POTA park directory was not found. Use -ParkCsvPath to select all_parks_ext.csv."
}

function Get-ExtraFieldValue {
    param(
        [object]$Qso,
        [string]$Name
    )

    $extraFieldsProperty = $Qso.PSObject.Properties['extraFields']
    if ($null -eq $extraFieldsProperty -or $null -eq $extraFieldsProperty.Value) {
        return $null
    }

    $property = $extraFieldsProperty.Value.PSObject.Properties[$Name]
    if ($null -eq $property) {
        return $null
    }

    return [string]$property.Value
}

function Get-ParkReferences {
    param([object]$Qso)

    $rawReference = Get-ExtraFieldValue -Qso $Qso -Name 'MY_POTA_REF'
    if ([string]::IsNullOrWhiteSpace($rawReference)) {
        $legacyProgram = Get-ExtraFieldValue -Qso $Qso -Name 'MY_SIG'
        if ([string]::Equals($legacyProgram, 'POTA', [StringComparison]::OrdinalIgnoreCase)) {
            $rawReference = Get-ExtraFieldValue -Qso $Qso -Name 'MY_SIG_INFO'
        }
    }

    if ([string]::IsNullOrWhiteSpace($rawReference)) {
        return @()
    }

    $matches = [regex]::Matches($rawReference.ToUpperInvariant(), '[A-Z0-9]+-[0-9]+(?:@[0-9]+)?')
    return @(
        $matches |
            ForEach-Object { $_.Value.Split('@')[0] } |
            Sort-Object -Unique
    )
}

function New-ActivationComment {
    param(
        [string]$Reference,
        [hashtable]$ParkNames
    )

    $parkName = $ParkNames[$Reference]
    if ([string]::IsNullOrWhiteSpace($parkName)) {
        return "POTA Activation $Reference"
    }

    return "POTA Activation $Reference $parkName"
}

function Merge-ActivationComments {
    param(
        [AllowNull()]
        [string]$ExistingComment,
        [string[]]$ActivationComments
    )

    $segments = [System.Collections.Generic.List[string]]::new()
    if (-not [string]::IsNullOrWhiteSpace($ExistingComment)) {
        $segments.Add($ExistingComment.Trim())
    }

    foreach ($activationComment in $ActivationComments) {
        $reference = ([regex]::Match($activationComment, '(?i)^POTA Activation\s+([A-Z0-9]+-[0-9]+)')).Groups[1].Value
        $referencePattern = [regex]::Escape($reference)
        $alreadyPresent = $segments | Where-Object {
            $_ -match "(?i)(?:^|\|\s*)POTA Activation\s+$referencePattern(?:\s|$)"
        }

        if (-not $alreadyPresent) {
            $segments.Add($activationComment)
        }
    }

    return $segments -join ' | '
}

$resolvedCliPath = Resolve-QsoRipperCliPath -RequestedPath $CliPath
$resolvedParkCsvPath = Resolve-ParkCsvPath -RequestedPath $ParkCsvPath

$parkNames = @{}
foreach ($park in Import-Csv -LiteralPath $resolvedParkCsvPath) {
    $reference = [string]$park.reference
    $name = [string]$park.name
    if (-not [string]::IsNullOrWhiteSpace($reference) -and -not $parkNames.ContainsKey($reference)) {
        $parkNames[$reference] = $name
    }
}

$after = [DateTimeOffset]::UtcNow.AddDays(-$Days).ToString(
    "yyyy-MM-dd'T'HH:mm:ss'Z'",
    [Globalization.CultureInfo]::InvariantCulture)

$listArguments = @('--engine', $Engine, '--json', 'list', '--after', $after, '--limit', $Limit)
$jsonLines = & $resolvedCliPath @listArguments
if ($LASTEXITCODE -ne 0) {
    throw "QsoRipper could not list recent QSO records. Exit code: $LASTEXITCODE."
}

$json = $jsonLines -join [Environment]::NewLine
$qsos = @($json | ConvertFrom-Json)
$changes = [System.Collections.Generic.List[object]]::new()
$activationCount = 0

foreach ($qso in $qsos) {
    $references = @(Get-ParkReferences -Qso $qso)
    if ($references.Count -eq 0) {
        continue
    }

    $activationCount++
    $activationComments = @(
        $references | ForEach-Object {
            New-ActivationComment -Reference $_ -ParkNames $parkNames
        }
    )
    $commentProperty = $qso.PSObject.Properties['comment']
    $existingComment = if ($null -eq $commentProperty) { '' } else { [string]$commentProperty.Value }
    $newComment = if ($ReplaceExistingComment) {
        $activationComments -join ' | '
    }
    else {
        Merge-ActivationComments -ExistingComment $existingComment -ActivationComments $activationComments
    }
    if ([string]::Equals($existingComment, $newComment, [StringComparison]::Ordinal)) {
        continue
    }

    $changes.Add([pscustomobject]@{
        LocalId = [string]$qso.localId
        Utc = ([DateTimeOffset]$qso.utcTimestamp).ToUniversalTime()
        Callsign = [string]$qso.workedCallsign
        Parks = $references -join ', '
        OldComment = $existingComment
        NewComment = $newComment
    })
}

Write-Host "Reviewed $($qsos.Count) QSO records from the last $Days days."
Write-Host "Found $activationCount POTA activation records."
Write-Host "Found $($changes.Count) records that need a comment update."
Write-Host "Comment mode: $(if ($ReplaceExistingComment) { 'replace' } else { 'append' })."

if ($changes.Count -eq 0) {
    return
}

if (-not $Apply) {
    $changes |
        Select-Object Utc, Callsign, Parks, OldComment, NewComment |
        Format-Table -AutoSize -Wrap
    Write-Host "Preview only. Run the script with -Apply to update these records."
    return
}

$updatedCount = 0
foreach ($change in $changes) {
    $updateArguments = @(
        '--engine', $Engine,
        'update', $change.LocalId,
        '--comment', $change.NewComment
    )
    & $resolvedCliPath @updateArguments | Out-Host
    if ($LASTEXITCODE -ne 0) {
        throw "The update failed for QSO $($change.LocalId). Exit code: $LASTEXITCODE."
    }

    $updatedCount++
}

Write-Host "Updated $updatedCount QSO records."

if ($Sync) {
    & $resolvedCliPath --engine $Engine sync
    if ($LASTEXITCODE -ne 0) {
        throw "QRZ synchronization failed. Exit code: $LASTEXITCODE."
    }
}
