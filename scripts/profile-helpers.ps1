# PowerShell helpers for qsoripper PR workflows.
#
# Dot-source this file from your $PROFILE to get one-shot PR helpers that
# play nicely with the repository's merge queue and auto-merge settings on
# https://github.com/treitforge/qsoripper.
#
# Example $PROFILE entry:
#   . "C:\path\to\qsoripper\scripts\profile-helpers.ps1"
#
# All helpers assume:
#   * gh CLI is installed and authenticated
#   * The repository's branch ruleset allows only squash merges on main
#   * Auto-merge and the merge queue are enabled on main

function New-AutoPR {
    <#
    .SYNOPSIS
        Create a PR from the current branch and arm auto-merge (squash) in one shot.

    .DESCRIPTION
        Creates a pull request targeting main using either an explicit title or
        the current branch's commit message (via gh's --fill), then arms it for
        auto-merge with the squash strategy. When all required checks pass,
        GitHub's merge queue takes over and merges the PR.

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

    if ($Title) {
        gh pr create --base main --title $Title --body $Body
    }
    else {
        gh pr create --base main --fill
    }

    if ($LASTEXITCODE -ne 0) {
        Write-Error "gh pr create failed; not arming auto-merge."
        return
    }

    gh pr merge --auto --squash
}

function Push-AutoPR {
    <#
    .SYNOPSIS
        Push the current branch, open a PR, and arm auto-merge in one shot.

    .DESCRIPTION
        Combines git push -u origin HEAD with New-AutoPR. The most common entry
        point: edit, commit, then Push-AutoPR.

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

