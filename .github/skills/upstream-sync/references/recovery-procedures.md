# Recovery procedures (rarely needed)

Three off-the-happy-path procedures that the agent does **not** drive
itself — they require explicit operator decisions. Linked from
[`SKILL.md`](../SKILL.md) so the main runbook stays focused on the
common case.

## Direct-to-main (admin-only escape hatch)

If the operator explicitly says "skip the PR, push straight to main":
after step 7 succeeds, skip step 8 and instead:

```pwsh
git switch main
git merge --ff-only $branch
git push origin main
git branch -D $branch
```

Requires bypass-branch-protection rights. No PR, no review checkpoint —
use only for explicit admin runs.

## First-time sync (seeding the watermark)

If the fork has no `(cherry picked from commit <sha>)` trailer on
`origin/main` yet, `02-compute-pending.ps1` will throw. To seed,
commit an **empty seed commit** carrying the trailer in its message —
the same format `cherry-pick -x` would emit, written by hand exactly
once:

```pwsh
git remote add upstream https://github.com/microsoft/terminal.git
git fetch upstream main --no-tags
git switch main
# Pick an upstream SHA you consider "already in this fork" (typically
# the commit the fork was originally branched from). Write the trailer
# EXACTLY as cherry-pick -x would so the next sync picks it up.
$seedSha = '<40-char-upstream-sha-already-in-fork>'
git commit --allow-empty -m "chore(upstream): seed upstream-sync watermark" -m "(cherry picked from commit $seedSha)"
git push origin main
```

That one merged trailer becomes the watermark. From then on, every
run extends it. No script needed — the bootstrap is one
`commit --allow-empty` ever.

## Squash-merge recovery (don't do this, but if you did)

The PR banner shouts "do not squash" and `-AutoMergeStrategy rebase`
locks in the safe path. If a reviewer squash-merges anyway:

- The squash commit's body usually concatenates every cherry-picked
  message, so it contains MANY `(cherry picked from commit <sha>)`
  trailers. The watermark resolver matches the FIRST one it sees,
  which is the oldest cherry-pick in the squashed batch. That means
  the watermark moves backward, and the next sync re-picks everything
  between the oldest and newest commits of the squashed batch.
- The recovery is the same as a first-time seed: commit an empty
  watermark commit on `main` carrying the trailer for the upstream
  HEAD that was actually merged, and push.
