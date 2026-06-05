# Build verification

Post-batch hard gate. Runs after the static scan passes and **before**
push / PR creation. If the build fails, the run is marked Tier-4 stuck.

## Why this exists

Static scan catches a specific set of content drifts. The compiler
catches everything else (missing includes, type mismatches,
vcxproj drift, MIDL/winrt projection errors, ...) with zero false
positives. A scheduler that opens PRs without proof the codebase still
builds is opening broken PRs — exactly what PR #220 risked.

## Pipeline

```
toolchain preflight  ─→  static scan  ─→  try-build  ─→  push / PR
       │                      │                │
   (infra-stuck)          (Tier-4)         (Tier-4)
```

## Toolchain preflight (`scripts/09-toolchain-preflight.ps1`)

Runs first. Discovers the required `PlatformToolset` from
`src/common.build.pre.props` (and other props files) and verifies it
is installed under any Visual Studio install on the host.

Outcomes:

| Outcome             | Behavior                                               |
|---------------------|--------------------------------------------------------|
| All toolsets found  | Continue to static scan + build.                       |
| Required missing    | Tier-4 **infra-stuck** — separate kind from code-stuck. Does NOT open a stuck issue, does NOT set a lock (no labeled issue to gate on). The next scheduler tick simply retries; provision the host before then. |
| Skipped (`-SkipBuild`) | Preflight not run. Caller accepts risk.             |

**The preflight does NOT auto-bump v143→v145.** That recipe is
intentionally kept as a *local-only* developer workaround (see the
`v143-v145` memory notes). Auto-bump risks silently changing the
toolset for everyone, which would break the rest of the team.
Schedulers should be provisioned with the correct VS install instead.

## Try-build (`scripts/10-try-build.ps1`)

Default invocation:

```cmd
cmd.exe /c "tools\razzle.cmd && bz no_clean"
```

(`bz no_clean` = incremental Debug build of the full solution.)

Configurable via the orchestrator's `-BuildCommand` parameter. The
default is verified on the maintainer host; if validation blocks the run,
the generated Tier-4 diagnostics include the build log path and tail.

Output:

- Full build log → `Generated Files/upstream-sync/<YYYY-MM-DD>/build-logs/<timestamp>.log`
  (gitignored — these are big and noisy; the gitignore is the repo root's
  `**/Generated Files/` rule).
- Last ~200 lines → captured into the run report and any Tier-4 stuck
  issue body.
- Exit code + duration → embedded in the Tier-4 stuck-issue YAML block
  (and the run report) when the build is the failing gate.

Timeout:

- Default 45 minutes (cold builds on a new sync branch with many
  picks can hit ~30 min; 45 gives headroom).
- Configurable via `-BuildTimeoutMinutes N`.
- On timeout the build is killed and classified as
  **build-inconclusive**.

| Outcome            | Default scheduler behavior     | Dev opt-out                          |
|--------------------|--------------------------------|--------------------------------------|
| Build succeeded    | Continue to push / PR.         | n/a                                  |
| Build failed       | Tier-4 stuck, open issue.      | n/a                                  |
| Build inconclusive | Tier-4 stuck (be safe).        | `-AllowInconclusiveBuild` → proceed with warning in report. |
| Skipped            | Reject in scheduler context.   | `-SkipBuild` for fast dev iteration. |

The "be safe" default for inconclusive is deliberate. A hung build is
indistinguishable from a real failure in scheduler mode; opening a PR
on an unproven sync defeats the whole point of this gate.

## When the build fails for fork-unrelated reasons

If a flaky build (unrelated env issue, transient toolchain glitch)
trips the gate:

1. The stuck issue gives a clear log tail.
2. A human can re-run the build locally to confirm it's a transient,
   then **close the stuck issue** to clear the lock.
3. The next scheduler tick will re-attempt the same pick range.

Distinguishing transient-build from real-pick-broke-build is left to
the human reviewing the issue — too noisy to automate, and the cost
of a manual cross-check is small (~once per N runs).

## Build artifacts

`Generated Files/upstream-sync/<YYYY-MM-DD>/build-logs/` is **not**
committed — the repo root's `**/Generated Files/` gitignore rule
covers it. Build artifacts under `bin/`, `obj/`, etc. follow the
repo's existing `.gitignore`.
