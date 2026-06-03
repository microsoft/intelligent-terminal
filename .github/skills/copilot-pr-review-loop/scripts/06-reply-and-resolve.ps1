<#
.SYNOPSIS
    Post a reply on a Copilot review thread and resolve it.

.DESCRIPTION
    Performs the two GraphQL mutations needed to address a Copilot finding:
    1. addPullRequestReviewThreadReply — appends a reply comment.
    2. resolveReviewThread             — marks the thread resolved.

    Use this for both accepted-and-fixed findings and for declined-with-
    rationale findings. See ../references/06-reply-templates.md for body
    patterns.

.PARAMETER ThreadId
    The GraphQL node ID of the review thread (e.g. PRRT_kw...).

.PARAMETER Body
    The reply body. Markdown is supported.

.PARAMETER NoResolve
    If set, posts the reply only and leaves the thread open. Useful when
    you want to start a back-and-forth discussion rather than close out the
    thread.

.EXAMPLE
    pwsh 06-reply-and-resolve.ps1 -ThreadId PRRT_kw... -Body "Fixed in abc1234."

.EXAMPLE
    # Decline with rationale, do not resolve yet
    pwsh 06-reply-and-resolve.ps1 -ThreadId PRRT_kw... -NoResolve `
        -Body "Declining: this would require cross-class plumbing for a hypothetical race."
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$ThreadId,

    [Parameter(Mandatory = $true)]
    [string]$Body,

    [switch]$NoResolve
)

$ErrorActionPreference = 'Stop'

$replyMutation = @'
mutation($tid: ID!, $body: String!) {
  addPullRequestReviewThreadReply(input: {
    pullRequestReviewThreadId: $tid,
    body: $body
  }) {
    comment { id }
  }
}
'@

$replyArgs = @('-f', "query=$replyMutation", '-f', "tid=$ThreadId", '-f', "body=$Body")
gh api graphql @replyArgs | Out-Null
Write-Output "Replied to thread $ThreadId"

if (-not $NoResolve) {
    $resolveMutation = @'
mutation($tid: ID!) {
  resolveReviewThread(input: { threadId: $tid }) {
    thread { isResolved }
  }
}
'@
    $resolveArgs = @('-f', "query=$resolveMutation", '-f', "tid=$ThreadId")
    gh api graphql @resolveArgs | Out-Null
    Write-Output "Resolved thread $ThreadId"
}
