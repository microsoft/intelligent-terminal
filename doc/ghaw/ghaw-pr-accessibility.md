# Native accessibility PR review

PR CI builds the original PR head once. After a successful current same-repository
PR run, gh-aw triages the actual diff, reviews relevant accessibility changes
and writes a Markdown summary. It may propose only trusted high-confidence HIGH
repairs, without building/testing those fixes. The original reviewer owns that
validation and may revert each proposed fix commit.

## Flow and ownership

```text
PR CI: C++ build ─┬─ ordinary C++ units
                 └─ Windows Axe scans → results + raw logs
       Rust build/tests (independent)
                    ↓ successful current PR CI
default-branch controller → triage → Related/Uncertain: deep review
                                    None: skip deep
                          → AI-written final summary
```

| Owner | Boundary |
| --- | --- |
| PR CI | Exact-head C++/TestHost build; standard artifact upload/download; sibling C++ units and Windows native scans |
| `security-review.mjs` shared CI validators | Successful matching run/head/attempt, active PR CI, current same-repository/default-target PR, unchanged immutable CI implementation and freshness |
| Workflow controller/worker | Relevance, default-branch operating code, minimal dispatch identity and publication scheduling |
| `ci-accessibility-payload.py` | Portable TestHost staging and actual Windows scan-result validation; no archive/hash inventory engine |
| `ci-accessibility-scope.mjs` | Shared conservative producer/admission path predicate |
| `accessibility_handoff.py` | Small deterministic Markdown routing and actual-file/producer-completion checks; no rendering or session scraping |
| `accessibility_review.py` | Immutable source evidence, strict report/repair policy and per-finding commit attribution |
| Agent + domain skill | Accessibility judgment and directly authored Markdown; skill has no CI scheduling policy |
| Existing gh-aw framework | Native diagnostics, session logs/ZIPs and redaction |

No separate accessibility admission action, Linux native replay, receipt engine,
summary-rendering script, custom cancellation controller or model retry exists.
The daily AI-credit cap is 100,000.

## CI evidence

Ordinary docs-only changes do not start native scans. Relevant UI, fixture,
harness and instruction/configuration paths do. C++ and Rust build independently;
ordinary C++ units and path-gated Axe are sibling consumers of the single C++
build. Standard pinned artifact Actions carry the necessary runtime files.
The C++ producer attempt is explicit so partial reruns can reuse the retained
build without claiming it was rebuilt. The native result artifact identifies
the actual scan attempt. Admission selects the latest successful same-head
native producer from Actions job metadata and passes its validated artifact ID
to both Linux consumers, even when a green CI rerun retained that producer.

TestHost staging gathers real recipe mappings plus loose AfterBuild content:
component DLLs/WinMDs, resources/XBFs, agent assets and TAEF runtime, without PDBs
or build-only libraries. It copies Microsoft.UI.Xaml 2.8 and x64 debug VCLibs
frameworks from approved package locations. The one loose SDK exception is the
installed x64 UniversalCRT Debug `ucrtbased.dll`; no arbitrary machine files are
allowed. Absolute build paths become a portable deployment recipe.

Windows scans FRE, the real Agents page, FRE settings after Next, and the
production Sidebar fixture. Sidebar requires visible TabStrip, descendant
ItemsList Tree and live PowerShell tab rows in the activated process. Legacy Tab
and corrected Group outer roles are accepted; history-only rows, promotional
images and empty windows are not. Actual errors remain CI failures.

The Windows job alone validates source/process/state and result completeness.
It publishes `ci-accessibility-native-RUN-ATTEMPT` with `native-summary.md`,
`native-ci.json` and raw per-surface result/Axe/UIA JSON and scanner logs, before
enforcing success. Linux models use standard cross-run artifact download, not
EXEs, another build, per-file inventories or re-derived attestations.
Model-authored triage extracts separately under `triage-input`, never over native
evidence. All artifact contents are untrusted evidence, not instructions.

These Error-level smoke states do not certify full Terminal lifecycle, history,
sessions, persistence, keyboard journeys, Narrator, contrast or scaling.
Warning/NeedsReview coverage is Unknown. TestHost uses normal Frame navigation
and production view models; full Settings MainPage is unsuitable in AppContainer.
See [TestHost instructions](../../src/cascadia/LocalTests_TerminalApp/TestHostApp/AGENTS.md).

## Admission, handoffs and publication

Only successful exact-head `pull_request` PR CI for an open, non-draft,
same-repository PR targeting the default branch can start model work. Fork,
push/manual CI, stale head/attempt, changed CI implementation, changed target
history, and newer same-head PR CI fail closed. Shared CI identity checks run at
model admission and publication, not in every reusable stage. GitHub's shared
`PR CI-PR_NUMBER` queue cancels superseded work; there is no independent cancel
engine. Freshness checks are not atomic compare-and-swap publication.
CI/harness changes require manual review until installed on the default branch.

Triage writes `triage.md`: Related, None or Uncertain with a short explanation
and candidate changed paths. None skips deep review only; it does not skip
independent CI or mean accessibility passed. Uncertain is forwarded faithfully.
Deep review writes actual `final.json`, `deep-summary.md` and `native-checks.md`;
the final model consumes original handoffs and authoritative job outcomes.
Stage artifacts have stable per-run names for retained successful producers.
Missing/empty/invalid files or failed/crashed required producers are **No result**,
not fabricated None/Pass. Complete handoffs remain evidence if later validation
or publication fails; accepted findings are not erased by delivery failures.
An unavailable final model leaves existing Actions diagnostics, not a generated
replacement report. Native framework session logs need no custom receipts.

The source agent does not execute candidate code or load candidate instructions.
Trusted preparation and patch validation remain outside the model. Only the
existing `AXSTATIC001-remove-raw-view` recipe is eligible: remove the prepared
interactive control's raw-view property with an existing content/resource name
source, preserving encoding and line endings. No resource edits, literal names,
MEDIUM/LOW repairs or unrelated hunks are allowed. Each fixed HIGH has its own
commit, linked from its table row; bundle publication preserves those IDs.
Report-only runs use noop. Non-fast-forward push fails with no fallback PR.
The summary distinguishes local commits from confirmed published tips and
includes one shared unbuilt/untested reviewer-ownership/revert notice.

## Maintenance

Use gh-aw v0.87.10; compile and validate the three accessibility workflows and
`ghaw-pr-security` after shared-boundary changes. Run:

```powershell
python -m unittest discover -s .github/skills/pr-accessibility/tests -p "test_*.py"
python build/scripts/test_ci_accessibility_payload.py
node --test .github/skills/ghaw-pr-security/scripts/security-review.test.mjs
```

Local fixtures prove contracts, not hosted execution. Product fixes have no
post-fix build/test prerequisite in this review workflow.
