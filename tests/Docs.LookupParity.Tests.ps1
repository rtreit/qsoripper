#!/usr/bin/env pwsh
<#
.SYNOPSIS
    Pester tests asserting docs stay in sync with the LookupService implementation.
.DESCRIPTION
    Guards against contract drift between the docs (README, docs/api/lookup-service.md,
    docs/architecture/data-model.md) and the actual Rust / .NET engine hosts for
    `BatchLookup` and `GetDxccEntity`.

    Run with:
        Invoke-Pester -Path tests/Docs.LookupParity.Tests.ps1
#>

$repoRoot = Split-Path -Parent $PSScriptRoot
$readmeContent = Get-Content (Join-Path $repoRoot 'README.md') -Raw
$lookupDocContent = Get-Content (Join-Path $repoRoot 'docs' 'api' 'lookup-service.md') -Raw
$dataModelContent = Get-Content (Join-Path $repoRoot 'docs' 'architecture' 'data-model.md') -Raw
$rustServerContent = Get-Content (Join-Path $repoRoot 'src' 'rust' 'qsoripper-server' 'src' 'main.rs') -Raw
$dotnetServicesContent = Get-Content (Join-Path $repoRoot 'src' 'dotnet' 'QsoRipper.Engine.DotNet' 'GrpcServices.cs') -Raw

function Assert-Matches([string]$Actual, [string]$Pattern) {
    if ($Actual -notmatch $Pattern) {
        throw "Expected text to match regex '$Pattern'."
    }
}

function Assert-NotMatches([string]$Actual, [string]$Pattern) {
    if ($Actual -match $Pattern) {
        throw "Expected text not to match regex '$Pattern'."
    }
}

Describe 'LookupService doc/implementation parity' {

    Context 'Rust engine host implements the advertised RPCs' {
        It 'has a batch_lookup implementation' {
            Assert-Matches $rustServerContent 'async\s+fn\s+batch_lookup\b'
        }

        It 'has a get_dxcc_entity implementation' {
            Assert-Matches $rustServerContent 'async\s+fn\s+get_dxcc_entity\b'
        }
    }

    Context '.NET engine host implements the advertised RPCs' {
        It 'has a BatchLookup override' {
            Assert-Matches $dotnetServicesContent 'override\s+(?:\S+\s+){1,3}BatchLookup\s*\('
        }

        It 'has a GetDxccEntity override' {
            Assert-Matches $dotnetServicesContent 'override\s+(?:\S+\s+){1,3}GetDxccEntity\s*\('
        }
    }

    Context 'docs/api/lookup-service.md status table reflects implementation' {
        It 'marks BatchLookup as Implemented' {
            Assert-Matches $lookupDocContent '\|\s*`BatchLookup`\s*\|\s*✅\s*Implemented'
        }

        It 'marks GetDxccEntity by dxcc_code as Implemented' {
            Assert-Matches $lookupDocContent '\|\s*`GetDxccEntity`\s*\(by\s*`dxcc_code`\)\s*\|\s*✅\s*Implemented'
        }

        It 'marks GetDxccEntity by prefix as Unimplemented' {
            Assert-Matches $lookupDocContent '\|\s*`GetDxccEntity`\s*\(by\s*`prefix`\)\s*\|\s*⚠️\s*Unimplemented'
        }
    }

    Context 'Other docs do not describe BatchLookup / DXCC-by-code as planned' {
        It 'README does not describe BatchLookup as future/planned/reserved' {
            Assert-NotMatches $readmeContent 'BatchLookup[^\.\n]*(planned|reserved for|future)'
        }

        It 'data-model.md does not describe batch lookup as reserved for later' {
            Assert-NotMatches $dataModelContent 'batch lookup[^\.\n]*reserved for later'
        }
    }
}
