# PR globalization workflow

`Globalization Review` is a separate PR check for functional globalization
defects. Same-repository PRs can receive constrained HIGH-only repairs; fork
PRs remain read-only. It does not replace `Localization Review`, translate
resources, or claim that localization file checks prove RTL or Unicode behavior.

## Architecture

The trusted
`.github/workflows/ghaw-pr-globalization-controller.yml` runs on
`pull_request_target`, checks out only the base revision, fetches the immutable
PR object, resolves the merge base, and dispatches one of two mutually exclusive
workers:

- Same-repository PRs use
  `.github/workflows/ghaw-pr-globalization-repair.lock.yml`, which may publish
  one validated HIGH-only repair with `push-to-pull-request-branch` or noop.
- Fork PRs use `.github/workflows/ghaw-pr-globalization.lock.yml`, which checks
  out the trusted workflow revision and may publish one guidance comment or
  noop without executing or editing fork code.

This mirrors the existing localization controller's separation because gh-aw
PR safe output must not combine a branch push and a comment in one worker.

The fork worker publishes at most one hidden-on-rerun comment; the repair worker
publishes at most one branch push. The fork agent has only read-only Git
commands and carries its structured report inside the native comment output;
it cannot write a separate report through an unrestricted shell. Before
publication, `Test-GlobalizationFindings.ps1` rejects symlinked or
out-of-directory evidence, malformed reports, stale SHAs, paths outside the
immutable PR change set, invalid severity/disposition combinations, and more
than 50 findings. Each worker re-reads the live PR head immediately before
publication.

The repair worker can edit only strongly evidenced HIGH findings with a small,
behavior-preserving patch to the exact immutable changed file named by the
finding. Medium/low findings remain suggestions. Forks are always read-only.
Automatic globalization repair does not modify RESW or localization YAML;
resource findings are handed to the localization workflow or a maintainer. The
privileged safe-output job applies the generated mailbox patch in a fresh,
configuration-isolated clone, independently derives the final manifest, runs
`git diff --check`, validates the structured report with trusted scripts, and
rechecks the live PR head immediately before publication. A later head race is
rejected as a non-fast-forward push because fallback PR creation is disabled.
Globalization and localization repairs share a PR-scoped, non-cancelling
concurrency group; the worker never performs both branch push and comment
publication.

## Repository-specific checks

| Surface | Required review |
| --- | --- |
| C++/WinRT and XAML | `FlowDirection` inheritance, mirroring exceptions, focus/order, mixed-direction content, UTF-16 and customer-facing formatting |
| RESW | Message composition, placeholders, translator context and clipping; defer translation parity/repair to `ensure-localization` |
| Rust UI and locale YAML | UTF-8/grapheme boundaries, ratatui width/alignment, message composition, and the shared locale-test lock |
| Settings, ACP, COM and VT | Locale-invariant persistence/wire tokens and round trips; never localize machine tokens |
| Buffer, parser and renderer | Existing terminal grapheme/cell-width/protocol semantics take precedence over generic UI advice |

Local anchors include `src/cascadia/ut_app/RtlHelperTests.cpp` (OS-backed RTL
and `qps-plocm`), `src/cascadia/TerminalApp/FreOverlay.cpp` (`FlowDirection`
cascade), `src/buffer/out/textBuffer.cpp` (surrogate/grapheme and cell-width
handling), `src/inc/til/string.h` (ordinal system strings versus linguistic
human sorting), and `tools/wta/src/test_support.rs` (serialized global locale
tests).

## References and rejected mismatches

- Unicode UAX #9, [Bidirectional Algorithm](https://www.unicode.org/reports/tr9/),
  defines logical versus display order and mixed-direction isolation.
- Unicode UAX #29, [Text Segmentation](https://www.unicode.org/reports/tr29/),
  defines grapheme clusters as user-perceived characters.
- Microsoft Learn, [Globalization and localization for Windows
  apps](https://learn.microsoft.com/windows/apps/design/globalizing/globalizing-portal),
  distinguishes culture-aware display from invariant application data.
- The separate trusted `Localization Review` owns the `ensure-localization`
  skill's six deterministic file checks. Globalization Review may report
  functional resource findings, but rejects agent-authored checker bundles and
  never treats localization checks as proof of RTL layout, grapheme behavior,
  customer-facing reachability, or invariant protocol serialization.
- Generic web-only `dir`/CSS guidance is not used as the implementation model:
  this repository uses WinUI/XAML, C++/WinRT, ratatui, DirectWrite, and terminal
  protocol semantics.

## Validation and enablement

Run:

```powershell
Invoke-Pester .github/scripts/ghaw-pr-globalization/GlobalizationWorkflow.Tests.ps1
gh aw compile .\.github\workflows\ghaw-pr-globalization.md
gh aw compile .\.github\workflows\ghaw-pr-globalization-repair.md
gh aw validate ghaw-pr-globalization ghaw-pr-globalization-repair
```

When an actionlint runtime is available, append `--actionlint` to the compile
commands for the additional GitHub Actions lint pass. The compile and strict
validation commands above do not require actionlint.

### Historical PR agent evaluation

The local evaluation uses microsoft/intelligent-terminal#45 because it crosses
C++/XAML and Rust UI RTL behavior. Clone the user's fork into a disposable
automation directory, fetch immutable base
`6281d4b64c8a587dd47a249a350b84c3442c8d3b` and head
`558c20bf0a9df333e9dead4c2b1e12a83778279d`, and create local branches without
pushing them. Run the fixture only inside a disposable container or VM with no
host credentials, no network, a read-only repository mount, and a writable
scratch directory limited to report output. Bound the process and kill only
that process tree on timeout. Require no
worktree/index changes afterward, then pass the JSON through
`Test-GlobalizationFindings.ps1 -Mode guide`. This tests actual agent reasoning,
schema adherence, false-positive exclusions, and immutable evidence; it does
not substitute for a hosted safe-output publication test.

The controller becomes active when merged to the default branch. Repository
Actions must allow `actions: write` for controller dispatch and
`copilot-requests: write` for compiled workers. The same-repository safe-output
job also needs generated `contents: write` permission to push its constrained
patch. No deployment, custom secret, label, CODEOWNERS map, or assignee
configuration is required.
