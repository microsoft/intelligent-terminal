# GitHub API Quirks (Verified)

These are the API behaviors that matter for the Copilot review loop. They
have all been verified empirically against the current API surface — read
this before reaching for an alternative API.

## ❌ GraphQL `requestReviews` with `botLogins` — REJECTED

```graphql
mutation {
  requestReviews(input: {
    pullRequestId: "PR_xxx",
    botLogins: ["copilot-pull-request-reviewer"]
  }) { ... }
}
```

Returns:

> InputObject 'RequestReviewsInput' doesn't accept argument 'botLogins'

The `botLogins` argument was removed from the GraphQL schema. Do not waste
time trying variants of it. The previous GraphQL bot-id approach (`BOT_kg...`)
also returns `NOT_FOUND` for the Copilot reviewer.

## ⚠️ REST `requested_reviewers` with `reviewers[]=Copilot` — usually accepted, sometimes silently dropped

```bash
gh api -X POST /repos/<owner>/<repo>/pulls/<n>/requested_reviewers \
    -f 'reviewers[]=Copilot'
```

**This is currently the most reliable trigger.** Use `-f` (not `-F`)
so `Copilot` is sent as a string. Capital "C" is required; the bot
login `copilot-pull-request-reviewer` returns HTTP 422 because bots
are not collaborators.

Caveats:

- The endpoint can return HTTP 201 success while **silently dropping**
  the Copilot bot from `requested_reviewers`. Always verify by reading
  the POST response body's `requested_reviewers` field AND/OR polling
  the endpoint for ~10s. Server-side drops have been observed
  immediately after a `review_request_removed` event (quiet-period
  after dismissal), when Copilot Code Review is not enabled on the
  repo, and for some private user-owned repos without a Copilot
  subscription.
- HTTP success is NOT sufficient — wait for a `copilot_work_started`
  event in the issue timeline to confirm the bot actually picked up
  the work. That is the only authoritative signal.

## ⚠️ `gh pr edit --add-reviewer Copilot` — best-effort fallback only

```bash
gh pr edit <pr-number> --add-reviewer Copilot
```

Behavior is inconsistent across `gh` CLI versions and account types.
On current `gh` (verified against intelligent-terminal and personal
repos), this returns:

> 'Copilot' not found
> exit 1

for both `Copilot` and `copilot-pull-request-reviewer`. Older versions
and some account configurations may succeed. Keep this as a fallback
attempted AFTER the REST POST, never as the primary path.

(Previous editions of this document called `gh pr edit --add-reviewer
copilot-pull-request-reviewer` "the only consistently working method".
That claim no longer holds. The REST POST + event-log verification is
the canonical flow.)

## ✅ GraphQL `addPullRequestReviewThreadReply` + `resolveReviewThread` — WORKS

```graphql
mutation($tid: ID!, $body: String!) {
  addPullRequestReviewThreadReply(input: {
    pullRequestReviewThreadId: $tid,
    body: $body
  }) { comment { id } }
}

mutation($tid: ID!) {
  resolveReviewThread(input: { threadId: $tid }) {
    thread { isResolved }
  }
}
```

Both return successfully against the current API.

## ⚠️ Review latency — not improved by polling faster

Copilot reviews typically post 3–6 minutes after the request, occasionally
up to ~10 minutes. There is no progress signal on the in-flight review,
and polling more often than every ~3 minutes wastes API budget without
making the review arrive sooner.

## ⚠️ `isOutdated` ≠ `isResolved` — outdated threads still need replies

A review thread can be `isOutdated: true` (Copilot's comment points at
lines that have since changed) while still `isResolved: false`. These
threads:

- **Still need a reply + resolve in the per-round loop.** A thread can
  start out actionable when Copilot posts it and BECOME outdated
  mid-round when your own fix shifts the cited lines. Filtering on
  `!isOutdated` would silently drop those threads from the per-round
  triage, leaving the PR's open-conversations list non-empty even
  after the underlying code is fixed.
- `scripts/02-list-open-threads.ps1` therefore includes outdated
  threads in its output by default (with an `IsOutdated` column for
  triage), and exposes a `-ExcludeOutdated` opt-out only for the
  legacy "what's actionable on current lines" use case.
- `scripts/09-cleanup-outdated.ps1` remains a safety net for the rare
  case where an outdated thread slips past the per-round loop entirely
  (e.g. it became outdated only AFTER your last `02-list-open-threads`
  fetch). Most loops should reach convergence with nothing for step 9
  to do.

## ⚠️ The "no new comments" review is not enough to declare convergence

A Copilot review summary that says *"generated no new comments"* is
necessary but not sufficient. You also need the open-thread list (filtered
for `!isResolved` — **outdated-but-unresolved threads count**, since they
can still be actionable per the policy above) to be empty. Otherwise, an
unresolved thread from an earlier round will keep the PR in review-pending
state. `-ExcludeOutdated` is an optional view for "what's actionable on
current lines" only; it must not be used to declare convergence.

## ⚠️ `git stash push` argument order gotcha

This works:

```bash
git stash push -m "local-build" -- src/path/a src/path/b
```

This silently does NOT honor the `-m`:

```bash
git stash push -- src/path/a src/path/b -m "local-build"
```

The `-m` MUST come before the `--` path separator.

## ⚠️ `gh api graphql -F` coerces strings — use `-f` for `String!` variables

The `gh` CLI distinguishes its two flag forms:

- `-F key=value` does **type inference** — a value that parses as int, bool, or null is sent as that JSON literal.
- `-f key=value` always sends the value as a raw string.

For any GraphQL variable declared `String!` (e.g. `owner`, `repo`, `body`,
`tid`, `after`), use `-f`. A reply body that happens to be `"true"`,
`"null"`, or all digits will otherwise be silently coerced and the call
fails because the receiver expects a string.

Keep `-F` only for genuinely numeric or boolean variables (e.g. `pr: Int!`).

```powershell
# Wrong — body could be coerced
gh api graphql -f query=$q -F body=$Body

# Right
gh api graphql -f query=$q -f body=$Body
```


## ⚠️ Native `gh` exit codes bypass `$ErrorActionPreference`

`gh` (and any other native executable) is **not** a PowerShell cmdlet, so
a non-zero exit code does **not** throw even when
`$ErrorActionPreference = 'Stop'` is set. Without an explicit check the
script will print misleading success messages (`"Replied to thread X"`,
`"Resolved Y"`) after a failed API call, and the loop will falsely
declare convergence on auth issues, rate limits, or transient 5xx.

GraphQL has an additional trap: `gh api graphql` can exit 0 for an HTTP 200
response whose JSON body contains a top-level `errors` array. Treat that as a
failed call too.

Pattern — wrap every `gh api graphql` call, check the native exit code, then
parse stdout and check `$data.errors` before printing success:

```powershell
function Invoke-GhGraphQL {
    param(
        [Parameter(Mandatory = $true)]
        [string[]]$Args,

        [Parameter(Mandatory = $true)]
        [string]$Context
    )

    $json = gh api graphql @Args
    if ($LASTEXITCODE -ne 0) {
        throw "gh api graphql failed (exit $LASTEXITCODE) [$Context]."
    }

    $data = $json | ConvertFrom-Json
    if ($data.errors) {
        $msgs = ($data.errors | ForEach-Object { $_.message }) -join '; '
        throw "GraphQL errors [$Context]: $msgs"
    }

    return $data
}
```

For non-GraphQL `gh` commands (`gh pr edit`, `gh pr view`, etc.), still check
`$LASTEXITCODE` immediately after each call.
