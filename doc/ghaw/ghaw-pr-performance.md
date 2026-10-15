# Performance PR review

Review changes affecting rendering, typing, tab/pane interactions, WTA sessions,
repeated work, memory and CI cost. Classify each finding HIGH, MEDIUM or LOW
before deciding whether to edit it.

## Ownership

| Owner | Responsibility |
| --- | --- |
| `performance-review` agent and shared skill | Source investigation, severity, minimal corrections and the Markdown summary template. |
| Repair worker | One separate proposal commit per eligible HIGH finding; native gh-aw branch push. |
| Fork worker | Read-only guidance; no execution, editing or branch push. |
| Controller | Immutable PR routing and delivery of the agent-written Markdown. |
| Original reviewer | Build/test, acceptance and optional rollback of a proposed fix commit. |

The agent writes the findings table, including each proposal's actual commit
link. No script builds a second findings summary, and no JSON report/card
protocol is required.

## Autofix policy

Only high-confidence HIGH findings supported by relevant scenario evidence
qualify for small, localized, behavior-preserving proposals. Easy replacement
does not make an uncertain-impact optimization HIGH. MEDIUM/LOW remain advice;
unsafe or broad changes require manual handoff.

Each HIGH finding gets one minimal commit with a distinct stable identifier:
`Propose HIGH PERF-<id>: <remedy>`. Do not combine findings, rebase, merge, amend
or include unrelated cleanup. Existing inline tests are preserved.

Automatic edits remain limited to original candidate WTA Rust files: at most
three files, 100 added-plus-deleted lines and 16 KiB of aggregate zero-context
diff. Dedicated tests and dependency/toolchain configuration are not repaired.
Budgets sum every parent-to-fix-commit diff and count the union of touched files,
not merely the final net change; later commits cannot hide an oversized rewrite.
C++ source can be reviewed, but corrections require manual handoff.

**The workflow does not build or test proposals before push.** Neither a
compiler, native toolchain, existing focused selector nor completed analyzer run
is a publication prerequisite. Available analyzer/measurement evidence can
support review, but missing coverage is recorded honestly.

The summary contains one shared notice below the findings table:

> Proposed fixes committed without build/test. The original reviewer owns
> validation and may revert the fix commit.

That notice is not repeated per finding. Committed proposals are not described
as verified or tested fixes.

## Workflow

1. The controller fetches the immutable PR head, resolves the merge base and
   skips agent execution when no performance-sensitive source changed.
2. The repair worker inspects source and callers, creates a separate local
   commit for each eligible HIGH finding, then writes the shared summary
   template with those commit links. No eligible fix means `noop`.
3. Trusted post-processing checks the actual captured bundle's commit range
   from the immutable reviewed head, not just the current checkout. Its tip
   must match the recorded proposal. Existing authored C++/Cargo/documentation
   changes stay outside this new-fix range. It does not parse Rust test items;
   the agent preserves tests and the original reviewer checks the diff.
4. gh-aw's existing `push-to-pull-request-branch` handler pushes one bundle.
   `signed-commits: false` preserves the agent's separate commit IDs; fallback
   PRs and extra CI-trigger commits are not enabled.
5. The controller delivers that same Markdown with execution links. It does
   not create a new findings table or insert per-finding validation claims.

The native push handler is fixed to the originating PR, uses its Actions token,
restricts files to WTA source and blocks protected files. The reviewed head is
checked before handoff; an incompatible branch advance fails the push rather
than being automatically rebased.

Fork source is fetched only for immutable comparison and read through permitted
tools. The fork worker can capture Markdown and request `noop`; its output job
is read-only/staged, and the framework conclusion job is disabled to avoid
automatic issue creation.

## Reporting

The controller updates only its own marked `github-actions[bot]` comment.
Numeric run/attempt markers prevent an older rerun from replacing a newer
report, including when duplicate marked comments exist: the greatest recorded
run/attempt determines freshness and the update target. It checks the live head against the worker's recorded head before
delivery and suppresses mentions in the Conversation copy.

Summary artifacts must be bounded, regular UTF-8 files, not symlinks/reparse
paths. Missing or unreadable required summaries and failed delivery do not
produce a green check. A successful workflow check means execution/delivery
completed; action-required delivery also fails the controller's publishing step.
It does **not** mean the proposed changes were built or tested.

Comment mutation is not atomic compare-and-swap: concurrency and final
freshness checks reduce races but do not constitute atomicity.

## Local workflow-contract checks

```powershell
node --test .github\workflows\ghaw-pr-performance\tests\*.test.mjs
gh aw compile ghaw-pr-performance ghaw-pr-performance-fork-guidance --validate --no-check-update
actionlint -shellcheck= -pyflakes= -ignore 'unknown permission scope "copilot-requests"' -ignore 'unexpected key "queue" for "concurrency" section' .github\workflows\ghaw-pr-performance-controller.yml .github\workflows\ghaw-pr-performance.lock.yml .github\workflows\ghaw-pr-performance-fork-guidance.lock.yml
```

These are development checks of the automation, not builds/tests performed on
an autofix before push. No new tooling or paid model trial is required by them.

## Historical evidence

The [October 14 trial](https://github.com/yeelam-gordon/ghaw-pr-performance-private-20261014/pull/1)
proved the previous native-tested implementation's agent, three Windows gates,
exact-tree publication, bot result and cleanup. The model also promoted
uncalled allocation advice to HIGH; that observation remains recorded.

That trial used a synthetic one-test library and Luna test-model overrides.
It does not prove production performance, Auto selection or this simplified
commit-without-build/test policy. Historical artifacts are not rewritten.
