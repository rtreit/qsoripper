# PowerShell helpers for qsoripper PR workflows.
#
# Dot-source this file from your $PROFILE to get one-shot PR helpers that
# play nicely with the repository's merge queue and branch protection on
# https://github.com/treitforge/qsoripper.
#
# Example $PROFILE entry:
#   . "C:\path\to\qsoripper\scripts\profile-helpers.ps1"
#
# All helpers assume:
#   * gh CLI is installed and authenticated
#   * The repository's branch ruleset allows only squash merges on main
#   * main requires at least one approving review before a PR can merge
#   * Auto-merge (autocomplete) is opt-in: arm it explicitly with New-AutoPR
#     or Enable-AutoMerge after a reviewer approves

function New-PR {
    <#
    .SYNOPSIS
        Create a PR from the current branch. Does not arm auto-merge.

    .DESCRIPTION
        Creates a pull request targeting main using either an explicit title or
        the current branch's commit message (via gh's --fill). The PR requires
        at least one approving review before it can merge. Merging is manual
        unless auto-merge is explicitly armed with Enable-AutoMerge.

    .PARAMETER Title
        Optional PR title. If omitted, gh pr create --fill uses the commit message.

    .PARAMETER Body
        Optional PR body. Only used when -Title is supplied.

    .EXAMPLE
        New-PR

    .EXAMPLE
        New-PR "feat: add CW decoder pitch discovery"

    .EXAMPLE
        New-PR -Title "fix: handle empty callsign" -Body "Avoids panic on QRZ miss."
    #>
    [CmdletBinding()]
    param(
        [Parameter(Position = 0)][string]$Title,
        [string]$Body = ""
    )

    if ($Title) {
        gh pr create --base main --title $Title --body $Body
    }
    else {
        gh pr create --base main --fill
    }
}

function Enable-AutoMerge {
    <#
    .SYNOPSIS
        Arm auto-merge (autocomplete) on the current branch's PR.

    .DESCRIPTION
        Arms the open PR for the current branch with squash auto-merge.
        Once the PR has at least one approving review and all required checks
        pass, the merge queue merges it automatically. Use this after creating
        a PR when you want autocomplete behavior rather than a manual merge.

    .EXAMPLE
        Enable-AutoMerge
    #>
    gh pr merge --auto --squash
}

function New-AutoPR {
    <#
    .SYNOPSIS
        Create a PR and immediately arm auto-merge (autocomplete) in one shot.

    .DESCRIPTION
        Creates a pull request targeting main and arms it for squash auto-merge.
        Once the PR has at least one approving review and all required checks
        pass, the merge queue merges it automatically. Use this only when you
        explicitly want autocomplete behavior for the PR.

    .PARAMETER Title
        Optional PR title. If omitted, gh pr create --fill uses the commit message.

    .PARAMETER Body
        Optional PR body. Only used when -Title is supplied.

    .EXAMPLE
        New-AutoPR

    .EXAMPLE
        New-AutoPR "feat: add CW decoder pitch discovery"

    .EXAMPLE
        New-AutoPR -Title "fix: handle empty callsign" -Body "Avoids panic on QRZ miss."
    #>
    [CmdletBinding()]
    param(
        [Parameter(Position = 0)][string]$Title,
        [string]$Body = ""
    )

    New-PR -Title $Title -Body $Body

    if ($LASTEXITCODE -ne 0) {
        Write-Error "gh pr create failed; not arming auto-merge."
        return
    }

    Enable-AutoMerge
}

function Push-PR {
    <#
    .SYNOPSIS
        Push the current branch and open a PR. Does not arm auto-merge.

    .DESCRIPTION
        Combines git push -u origin HEAD with New-PR. The default one-shot
        flow: edit, commit, then Push-PR. A reviewer must approve before the
        PR can merge.

    .PARAMETER Title
        Optional PR title. Forwarded to New-PR.

    .PARAMETER Body
        Optional PR body. Forwarded to New-PR.

    .EXAMPLE
        Push-PR "fix: handle empty callsign"
    #>
    [CmdletBinding()]
    param(
        [Parameter(Position = 0)][string]$Title,
        [string]$Body = ""
    )

    git push -u origin HEAD
    if ($LASTEXITCODE -ne 0) {
        Write-Error "git push failed; not creating PR."
        return
    }

    New-PR -Title $Title -Body $Body
}

function Push-AutoPR {
    <#
    .SYNOPSIS
        Push the current branch, open a PR, and arm auto-merge in one shot.

    .DESCRIPTION
        Combines git push -u origin HEAD with New-AutoPR. Use only when you
        explicitly want autocomplete behavior: the PR merges automatically
        once approved and all checks pass.

    .PARAMETER Title
        Optional PR title. Forwarded to New-AutoPR.

    .PARAMETER Body
        Optional PR body. Forwarded to New-AutoPR.

    .EXAMPLE
        Push-AutoPR "fix: handle empty callsign"
    #>
    [CmdletBinding()]
    param(
        [Parameter(Position = 0)][string]$Title,
        [string]$Body = ""
    )

    git push -u origin HEAD
    if ($LASTEXITCODE -ne 0) {
        Write-Error "git push failed; not creating PR."
        return
    }

    New-AutoPR -Title $Title -Body $Body
}

