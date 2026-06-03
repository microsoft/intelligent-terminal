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

## ❌ REST `requested_reviewers` with bot login — HTTP 422

```bash
gh api -X POST /repos/<owner>/<repo>/pulls/<n>/requested_reviewers \
    -F 'reviewers[]=copilot-pull-request-reviewer'
```

Returns HTTP 422. The REST endpoint enforces that requested reviewers be
repository collaborators, and bots are not collaborators.

Note: some related repositories have reported success with
`-F 'reviewers[]=Copilot'` (capital C, no `-pull-request-reviewer` suffix)
on the REST endpoint. This is inconsistent across orgs — treat it as a
fallback, not a primary path.

## ✅ `gh pr edit --add-reviewer` — WORKS

```bash
gh pr edit <pr-number> --add-reviewer copilot-pull-request-reviewer
```

This is the only consistently working method to trigger a fresh Copilot
review. It is idempotent — re-running it queues another review.

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

## ⚠️ `isOutdated` ≠ `isResolved`

A review thread can be `isOutdated: true` (Copilot's comment points at
lines that have since changed) while still `isResolved: false`. These
threads:

- Are NOT actionable in the current round (the cited code is gone).
- Will still appear in the PR's open conversations until explicitly
  resolved.
- Should be filtered out when listing what needs triage.
- Should be batch-resolved once at convergence (see
  `scripts/09-cleanup-outdated.ps1`).

## ⚠️ The "no new comments" review is not enough to declare convergence

A Copilot review summary that says *"generated no new comments"* is
necessary but not sufficient. You also need the open-thread list (after
filtering for `!isResolved && !isOutdated`) to be empty. Otherwise, an
unresolved thread from an earlier round will keep the PR in review-pending
state.

## ⚠️ `git stash push` argument order pitfall

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
