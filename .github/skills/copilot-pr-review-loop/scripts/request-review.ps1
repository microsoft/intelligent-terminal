<#
.SYNOPSIS
    Re-request a Copilot code review on a pull request.

.DESCRIPTION
    Uses `gh pr edit --add-reviewer copilot-pull-request-reviewer` to trigger
    a fresh Copilot review. This form is idempotent and is the only one that
    currently works for the Copilot reviewer bot — see
    ../references/api-quirks.md for why the GraphQL and REST alternatives
    fail.

.PARAMETER PrNumber
    The pull request number to re-request review on.

.EXAMPLE
    pwsh request-review.ps1 -PrNumber 122
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [int]$PrNumber
)

$ErrorActionPreference = 'Stop'

gh pr edit $PrNumber --add-reviewer copilot-pull-request-reviewer
