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
$global:DocsLookupReadmeContent = Get-Content (Join-Path $repoRoot 'README.md') -Raw
$global:DocsLookupLookupDocContent = Get-Content (Join-Path $repoRoot 'docs' 'api' 'lookup-service.md') -Raw
$global:DocsLookupDataModelContent = Get-Content (Join-Path $repoRoot 'docs' 'architecture' 'data-model.md') -Raw
$global:DocsLookupRustServerContent = Get-Content (Join-Path $repoRoot 'src' 'rust' 'qsoripper-server' 'src' 'main.rs') -Raw
$global:DocsLookupDotnetServicesContent = Get-Content (Join-Path $repoRoot 'src' 'dotnet' 'QsoRipper.Engine.DotNet' 'GrpcServices.cs') -Raw

function global:Assert-DocsLookupMatches([string]$Actual, [string]$Pattern) {
    if ($Actual -notmatch $Pattern) {
        throw "Expected text to match regex '$Pattern'."
    }
}

function global:Assert-DocsLookupNotMatches([string]$Actual, [string]$Pattern) {
    if ($Actual -match $Pattern) {
        throw "Expected text not to match regex '$Pattern'."
    }
}

Describe 'LookupService doc/implementation parity' {

    Context 'Rust engine host implements the advertised RPCs' {
        It 'has a batch_lookup implementation' {
            Assert-DocsLookupMatches $global:DocsLookupRustServerContent 'async\s+fn\s+batch_lookup\b'
        }

        It 'has a get_dxcc_entity implementation' {
            Assert-DocsLookupMatches $global:DocsLookupRustServerContent 'async\s+fn\s+get_dxcc_entity\b'
        }
    }

    Context '.NET engine host implements the advertised RPCs' {
        It 'has a BatchLookup override' {
            Assert-DocsLookupMatches $global:DocsLookupDotnetServicesContent 'override\s+(?:\S+\s+){1,3}BatchLookup\s*\('
        }

        It 'has a GetDxccEntity override' {
            Assert-DocsLookupMatches $global:DocsLookupDotnetServicesContent 'override\s+(?:\S+\s+){1,3}GetDxccEntity\s*\('
        }
    }

    Context 'docs/api/lookup-service.md status table reflects implementation' {
        It 'marks BatchLookup as Implemented' {
            Assert-DocsLookupMatches $global:DocsLookupLookupDocContent '\|\s*`BatchLookup`\s*\|\s*Implemented'
        }

        It 'marks GetDxccEntity by dxcc_code as Implemented' {
            Assert-DocsLookupMatches $global:DocsLookupLookupDocContent '\|\s*`GetDxccEntity`\s*\(by\s*`dxcc_code`\)\s*\|\s*Implemented'
        }

        It 'marks GetDxccEntity by prefix as Unimplemented' {
            Assert-DocsLookupMatches $global:DocsLookupLookupDocContent '\|\s*`GetDxccEntity`\s*\(by\s*`prefix`\)\s*\|\s*Unimplemented'
        }
    }

    Context 'Other docs do not describe BatchLookup / DXCC-by-code as planned' {
        It 'README does not describe BatchLookup as future/planned/reserved' {
            Assert-DocsLookupNotMatches $global:DocsLookupReadmeContent 'BatchLookup[^\.\n]*(planned|reserved for|future)'
        }

        It 'data-model.md does not describe batch lookup as reserved for later' {
            Assert-DocsLookupNotMatches $global:DocsLookupDataModelContent 'batch lookup[^\.\n]*reserved for later'
        }
    }
}
