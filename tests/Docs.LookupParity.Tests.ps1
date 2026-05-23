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

BeforeAll {
    $script:repoRoot = Split-Path -Parent $PSScriptRoot

    $script:readmeContent = Get-Content (Join-Path $script:repoRoot 'README.md') -Raw
    $script:lookupDocContent = Get-Content (Join-Path $script:repoRoot 'docs' 'api' 'lookup-service.md') -Raw
    $script:dataModelContent = Get-Content (Join-Path $script:repoRoot 'docs' 'architecture' 'data-model.md') -Raw
    $script:rustServerContent = Get-Content (Join-Path $script:repoRoot 'src' 'rust' 'qsoripper-server' 'src' 'main.rs') -Raw
    $script:dotnetServicesContent = Get-Content (Join-Path $script:repoRoot 'src' 'dotnet' 'QsoRipper.Engine.DotNet' 'GrpcServices.cs') -Raw
}

Describe 'LookupService doc/implementation parity' {

    Context 'Rust engine host implements the advertised RPCs' {
        It 'has a batch_lookup implementation' {
            $script:rustServerContent | Should -Match 'async\s+fn\s+batch_lookup\b'
        }

        It 'has a get_dxcc_entity implementation' {
            $script:rustServerContent | Should -Match 'async\s+fn\s+get_dxcc_entity\b'
        }
    }

    Context '.NET engine host implements the advertised RPCs' {
        It 'has a BatchLookup override' {
            $script:dotnetServicesContent | Should -Match 'override\s+(?:\S+\s+){1,3}BatchLookup\s*\('
        }

        It 'has a GetDxccEntity override' {
            $script:dotnetServicesContent | Should -Match 'override\s+(?:\S+\s+){1,3}GetDxccEntity\s*\('
        }
    }

    Context 'docs/api/lookup-service.md status table reflects implementation' {
        It 'marks BatchLookup as Implemented' {
            $script:lookupDocContent | Should -Match '\|\s*`BatchLookup`\s*\|\s*✅\s*Implemented'
        }

        It 'marks GetDxccEntity by dxcc_code as Implemented' {
            $script:lookupDocContent | Should -Match '\|\s*`GetDxccEntity`\s*\(by\s*`dxcc_code`\)\s*\|\s*✅\s*Implemented'
        }

        It 'marks GetDxccEntity by prefix as Unimplemented' {
            $script:lookupDocContent | Should -Match '\|\s*`GetDxccEntity`\s*\(by\s*`prefix`\)\s*\|\s*⚠️\s*Unimplemented'
        }
    }

    Context 'Other docs do not describe BatchLookup / DXCC-by-code as planned' {
        It 'README does not describe BatchLookup as future/planned/reserved' {
            $script:readmeContent | Should -Not -Match 'BatchLookup[^\.\n]*(planned|reserved for|future)'
        }

        It 'data-model.md does not describe batch lookup as reserved for later' {
            $script:dataModelContent | Should -Not -Match 'batch lookup[^\.\n]*reserved for later'
        }
    }
}
