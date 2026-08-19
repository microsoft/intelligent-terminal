# Conflict Triage — Resolution Tiers

When a cherry-pick conflicts, apply tiers **in order**. Stop at the first
tier that fully resolves the conflict.

## Tier 0 — Known take-{upstream,ours} files

Some files have a fixed correct resolution that never changes. Examples:

- `.github/workflows/spelling2.yml` — always take upstream (verified on sister repo `agentic-terminal`).

The list of these paths lives in [`03-known-conflicts.md`](./03-known-conflicts.md).

**Algorithm:**

```pwsh
$conflictingPaths = git diff --name-only --diff-filter=U
$tier0List = Get-KnownConflicts   # parses 03-known-conflicts.md
foreach ($p in $conflictingPaths) {
    $entry = $tier0List | Where-Object { $_.Path -eq $p }
    if (-not $entry) { return $false }  # Tier 0 doesn't cover this commit
    switch ($entry.Strategy) {
        'take-upstream' { git checkout --theirs -- $p; git add -- $p }
        'take-ours'     { git checkout --ours    -- $p; git add -- $p }
        'union'         { <# escalates — Tier-3 #> }
    }
}
git cherry-pick --continue --no-edit
```

If `git status` is now clean and the cherry-pick continued, **Tier 0 fully resolved** — record the file(s) auto-resolved and move on.

> **Note:** `union` is recognized by [`03-known-conflicts.md`](./03-known-conflicts.md) parsing for forward compatibility but is **not implemented** in [`scripts/03-cherry-pick-one.ps1`](../scripts/03-cherry-pick-one.ps1) — a `union` entry currently falls through to Tier-3 with the path listed under `conflict_paths`. Author Tier-0 entries with `take-upstream` or `take-ours` only.

## Tier 1 — Empty after staging

After Tier 0 (or with no conflicts to begin with), if the staged diff is
empty, the commit has already been applied to the fork in some prior
form. Skip it without recording a commit:

```pwsh
if ((git diff --cached --quiet; $LASTEXITCODE) -eq 0) {
    git cherry-pick --skip   # equivalent to reset + advance
    return @{ status = 'skipped-empty' }
}
```

## Tier 2 — LLM-assisted trivial textual (opt-in)

Disabled by default. The orchestrator in [`SKILL.md`](../SKILL.md) does
not invoke Tier-2 — if you want it, an agent walking the cherry-pick loop
can opt in per-conflict using the rubric below. Even when invoked, this
tier only fires when **all** of the following hold:

- No more than 3 conflicting files.
- Each file has fewer than 5 conflict hunks.
- Each hunk has fewer than 30 lines on either side.
- No conflicting file is in `src/cascadia/TerminalProtocol/`,
  `src/cascadia/WindowsTerminal/TerminalProtocolComServer.cpp`, or
  `tools/wta/**` (these are fork-only and shouldn't conflict; if they
  somehow do, that's a Tier-3 signal).

**Delegation:**

Spawn a fresh sub-agent (Memory Assistant rules require fresh — never
self-review). Prompt template:

> You are resolving a git cherry-pick conflict mechanically. Below are
> the conflict markers in `<path>`. The fork ("ours") adds AI-agent
> integration; upstream ("theirs") is microsoft/terminal. Produce ONLY
> the resolved file content — no commentary, no markers. If you cannot
> resolve with high confidence (≥0.9), respond with the single token
> `LOW_CONFIDENCE` and nothing else.
>
> Confidence rubric:
> - **High**: changes are non-overlapping in intent (e.g., upstream
>   added a new function near our edit; merge order is obvious).
> - **Low**: both sides modified the same logic / same lines / same
>   public API — semantic decision needed.

**Acceptance:** If the agent returns `LOW_CONFIDENCE`, escalate to
Tier 3. If it returns content, **verify with a second fresh agent**:

> Compare the resolved file against the "ours" version and the "theirs"
> version. Does the resolution preserve all behavioral intent from both
> sides? Respond `OK` or `NOT_OK: <reason>`.

Stage only if both agents agree `high`/`OK`. Otherwise, route to Tier 3.

## Tier 3 — Stop and escalate (cherry-pick conflict)

Anything not resolved by Tier 0–2:

~~~pwsh
git cherry-pick --abort
# Push the branch, ensure the 'upstream-sync-stuck' label exists, and
# file a plain-markdown issue with the stuck SHA, upstream URL, author,
# branch name, and conflicting paths. See SKILL.md step 5a for the
# exact recipe. Surface the issue URL + branch to the operator and exit.
~~~

The issue body **must** include:

- The conflicting commit SHA, subject, author, and upstream URL.
- The list of conflicting paths.
- The exact local branch name where the human picks up.
- The exact resume action: (1) check out the stuck branch,
  (2) **re-run the cherry-pick to reproduce the conflict** — the script
  calls `git cherry-pick --abort` before returning, so a fresh checkout
  has no `MERGE_MSG` / conflict markers to resolve. Use
  `git cherry-pick -x <stuck_sha>` (the `-x` preserves the
  `(cherry picked from commit <sha>)` trailer — critical for the
  watermark). (3) Resolve, `git add`, `git cherry-pick --continue`.
  (4) Push, open a PR, merge keeping the trailer (no squash).
  (5) CLOSE the stuck issue (that's the lock-clear signal — no script).

No fenced YAML metadata block is needed. Closing the labeled issue IS
the lock-clear signal; nothing parses the body back.

## Tier 4 — Post-pick build failed

The cherry-picks all applied cleanly, but [`scripts/04-try-build.ps1`](../scripts/04-try-build.ps1)
said NO before the PR could be finalized. The build runs before finalize
on purpose — see [`run-a-sync.md` step 7](run-a-sync.md#7-build) for the
build-then-finalize ordering and the one-focused-fix-commit rule.

| Sub-kind | Trigger | Action |
|---|---|---|
| **build-failed** | `bz no_clean` exited non-zero within timeout | Try ONE focused build-fix commit per [run-a-sync.md step 7](run-a-sync.md#7-build). If that fails or scope is too large → surface the failure to the operator and exit (no issue is filed). |
| **build-inconclusive** | Wall-clock cap (default 45 min) hit | Surface the timeout to the operator and exit (don't guess at fixing a hang). |

No stuck issue is filed for build failures. The operator either fixes
the underlying defect on `main` and re-runs (the next sync re-attempts
the same range and re-validates), or pushes a manual fix commit on top
of the sync branch and finishes step 8 by hand.

## Line endings

If any Tier-2 resolution touches a file with CRLF line endings (most
`.csproj`, `.xml`, winget manifests, and many `.yml` files on this repo),
re-normalize before staging:

```pwsh
# Inside Tier-2, after writing the resolved content:
$bytes = [System.IO.File]::ReadAllBytes($p)
# Preserve the file's original BOM presence — UTF-8-with-BOM is right
# for .resw / .csproj on this repo, but UTF-8-without-BOM is right for
# many .yml / .md files. Adding a BOM where one wasn't there before
# introduces unrelated encoding diffs and can break tooling.
$hasBom = $bytes.Length -ge 3 -and $bytes[0] -eq 0xEF -and $bytes[1] -eq 0xBB -and $bytes[2] -eq 0xBF
$text   = [System.Text.Encoding]::UTF8.GetString($bytes) -replace "`r?`n", "`r`n"
[System.IO.File]::WriteAllText($p, $text, (New-Object System.Text.UTF8Encoding($hasBom)))
```

(Skipping this is how the winget-pkgs submission broke last time —
LF mid-file fails CI even though the rest of the file is CRLF.)

## What is NOT a conflict for our purposes

- **Upstream renamed a file we never touched** — git follows the rename
  automatically. No conflict.
- **Upstream deleted a file we never touched** — git removes it. No conflict.
- **Upstream modified a file in a fork-only directory** (e.g., upstream
  somehow touched `tools/wta/`) — impossible by construction since
  upstream doesn't know those files exist. If it ever happens, it's a
  Tier-3 signal that the fork-only directory is misnamed.
