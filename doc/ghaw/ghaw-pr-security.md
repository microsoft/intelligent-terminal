# Intelligent Terminal PR security review

`ghaw-pr-security` is a separate gh-aw review for pull requests that touch
product code, WTA, build/package logic, or GitHub automation. Its public check
name is **Intelligent Terminal Security Review**.

## Trust model

The ordinary `ghaw-pr-security-controller.yml` runs on `pull_request_target`,
checks out the immutable base SHA from `github.repository`, fetches the PR head
as Git objects, resolves the merge base, classifies scope, and dispatches one
detached worker:

- Same-repository: `ghaw-pr-security.md` checks out the immutable head and may
  produce one validated automatic-repair artifact.
- Fork: `ghaw-pr-security-guide-fork.md` stays on the trusted workflow revision,
  inspects immutable fork objects read-only, and may produce one validated
  guidance artifact.

Both workers accept only a non-mutating `noop` result because generated gh-aw
publication jobs are not ordered after native post-validation. gh-aw always
exposes its system outputs and auto-injects `create-issue` when only system
outputs are configured, so each worker declares a staged check-run output to
suppress that injection, globally stages safe outputs, and explicitly disables
noop, missing-data, missing-tool, incomplete-report, and failure issue
publication. The check-run tool is preview-only and the native validator rejects
it. After a successful worker, the trusted controller performs the mutually
exclusive operation: an index-only, expected-head-leased fast-forward repair push for
same-repository PRs, or an idempotent trusted-renderer guidance comment for fork
PRs.

Before inference, the trusted script validates the immutable observed-base/head
commits, resolves their merge base, normalizes changed paths, classifies
affected trust boundaries, and records a scope hash. After inference, a fresh
API read must still return the same head.
Rename/copy classification includes both source and destination paths, so moving
a sensitive file into an unrelated directory does not suppress review. All
`.github/**` automation, including policies and instructions, is included in
both triggering and classification. Report validation rejects common bearer
and labeled session-capability forms without claiming universal secret detection.
Each worker rematerializes the validator from `github.workflow_sha`, not from
agent-edited bytes.

Both workers import the same mode-aware
`.github/agents/ghaw-pr-security.agent.md` and install the same
`.github/skills/ghaw-pr-security/SKILL.md`; the skill's bundled script owns
mechanical scope/report validation. The workflow selects `guide` or `repair`
mode and supplies the corresponding tools, so review-only and repair behavior
do not require duplicate primary agents. The repair workflow embeds only its
independent read-only repair gate as an inline subagent. Installing the agent
and skill through gh-aw keeps them tied to the trusted workflow revision,
rather than trusting PR-edited copies.

The report contract limits findings to changed files and assigns stable
`ITSEC-<hash>` IDs from rule/category/path/line. It independently enforces:

- HIGH findings are blocking; medium/low findings are advice-only.
- HIGH/high-confidence findings cannot rely only on hypotheses.
- passing checks name the immutable head, and non-scope validation requires an
  explicit local command record from that immutable workspace; arbitrary
  Actions run URLs cannot authorize a repair.
- fork comments must equal the trusted renderer's output for the validated
  report; arbitrary agent-authored comment bodies are rejected.
- secret-like content, unsafe paths, malformed reports, stale SHAs, more than 20
  findings, and invalid `fixed` claims are rejected.
- the job summary separates blocking HIGH findings from considerations and the
  validated JSON is retained for 14 days.

Automatic repair is allowed only when the original finding is HIGH with high
confidence and strong evidence, the patch is minimal and inside existing
`tools/wta/src/**/*.rs` files, applicable final validation passes, no check
is failed/blocked, an independent reviewer returns `SOURCE_PASS` for the immutable
head and exact final patch digest, and the live PR head still equals the
reviewed SHA. This model review is defense in depth, not a separate credential
or authorization principal; native checks and safe-output policy remain the
mechanical boundary. The agent reports only `proposed` repairs. Independent
source approval occurs before trusted test execution and does not claim test
success. Native attestation alone promotes an exact, source-approved proposal
to `fixed` after final-patch validation passes. After inference, the trusted post-step creates a fresh
checkout of the immutable head, recomputes scope from the dispatch SHAs, copies
only reported regular non-executable WTA source files without mode changes,
rejects agent-authored final validation claims, and emits only a source-reviewed
proposal artifact. A reusable
`ghaw-pr-security-validate-windows.yml` job reconstructs the immutable head,
recomputes scope, checks proposal identity/paths/digest, and executes tests only
inside the trusted public Rust 1.93.0/MSVC image as `ContainerUser`. Final Linux
promotion waits for that job, verifies its head and patch digest, and creates
the publication artifact only after native test success. Failed validation or
unpromoted proposals cannot reach the controller's push path. Dependencies
are fetched on the trusted Windows host with `cargo fetch --locked` from a
separate immutable base checkout, without executing PR-controlled build code;
automatic repair is
blocked unless every complete-PR diff entry has Git status `M` and targets
existing WTA Rust source. Additions, copies, deletions, renames, and type changes
remain guidance-only.
The native validator compares every reported patch path and patch digest with
this trusted worktree and rejects symlinks, submodules, mode changes,
CI/security policy, manifests, unrelated dependencies, and medium/low edits.
Unsafe or unvalidated HIGH findings remain blocking. Automatic repair also
rejects untracked files, so the reviewed binary-diff digest covers every
published byte.

Both workers emit exactly one `noop`; all of their gh-aw safe outputs are staged
and issue-reporting paths are disabled, so they never publish. The controller
downloads the validated card and exact binary patch. It publishes a
same-repository repair only as a commit whose parent is the reviewed head and
verifies the candidate descends from that head, and uses an explicit
`--force-with-lease=refs/heads/<branch>:<reviewed-head>` compare-and-swap.
The lease is not permission to rewrite history: the candidate is a child of
the reviewed head, and the ancestry check is mandatory. A rewind race must
also be rejected; a non-force push alone would accept that race and restore
removed commits. Local bare-repository regression coverage verifies the
rewind and concurrent-advance cases. For a
fork finding it publishes only the trusted rendered summary. It fails the
public check when unfixed HIGH findings remain.

## Repository-specific coverage

The reviewer uses `doc/security-model.md`, `tools/wta/AGENTS.md`, protocol IDL,
COM implementation, WTA master/session MCP code, and relevant tests. It treats:

- COM activation, `WT_COM_CLSID`, `Authenticate`, pipe IDs, and session
  variables as non-secret/non-authorizing unless code supplies a real check;
- `session_to_helper` and per-session MCP bearer capabilities as routing and
  identity invariants;
- direct COM/`wtcli` mutation as distinct from confirmation-gated session MCP;
- hook, OSC, ACP, pane, issue, PR, attachment, and log content as untrusted;
- runtime paths, reparse handling, packaged binary/hook provenance, and
  credential/redaction behavior as trust boundaries.

## Analyzer research and limitations

The implementation reuses security architecture, not generic prompt wording:

- github/gh-aw's security reviewer
  ([pinned source](https://github.com/github/gh-aw/blob/ff2eccd10a30d6a7bfaf0da449194e907a206555/.github/workflows/security-review.md))
  demonstrates a distinct gh-aw reviewer and structured safe-output data, but
  is AWF-specific, slash-command driven, and grants broad Bash/GitHub tooling;
  those are rejected here.
- github/gh-aw's DeepSec example
  ([pinned source](https://github.com/github/gh-aw/blob/ff2eccd10a30d6a7bfaf0da449194e907a206555/.github/workflows/deepsec-security-scan.md))
  demonstrates bounded findings export, but installs and executes a third-party
  scanner and creates issues, so it is not suitable for untrusted PR review.
- CodeQL Action
  ([pinned README](https://github.com/github/codeql-action/blob/a7afe0a2d717fe23bf93c8d8239d4f43c4a40f02/README.md))
  currently supports C/C++ and Rust. It is an ordinary analyzer building block,
  not proof that this gh-aw review ran. C/C++ build-mode `none` is preview and
  can miss generated code; the project's faithful native build is Windows.
- RustSec `cargo audit`
  ([pinned README](https://github.com/RustSec/rustsec/blob/ec8c34b0f0b70d8ae999d037a7b46ef3493c20ff/cargo-audit/README.md))
  audits dependency advisories, not WTA source authorization/routing logic.
- Semgrep
  ([pinned README](https://github.com/semgrep/semgrep/blob/0516c0f23a3dceac5c8f5ff3fecd402af4450182/README.md))
  parses C/C++ and Rust, but its community engine documents single-file/
  single-function security limitations. It is not added as an unpinned install
  or treated as authoritative.

Separate CodeQL, AuditMode/CppCoreCheck, TAEF, and explicit-target Cargo checks
remain independent evidence. Missing or mismatched-head evidence is reported as
skipped/blocked, never as a passing native check. A matching-head external
analyzer may substantiate finding evidence but cannot become a passing local
validation check or authorize repair.

Detached workers are dispatched on the base branch but fail in `prepare` unless
`github.workflow_sha` equals the controller-recorded base SHA. Their
`workflow_dispatch` context deliberately omits gh-aw's `pull_request`
`item_type`, so the generated generic `Checkout PR branch` step is ineligible.
The repair worker's explicit checkout is pinned to the immutable head; fork
guidance stays on the trusted workflow checkout and reads only fetched Git
objects. The inline repair gate and skill are restored from gh-aw's trusted
activation artifact after checkout.
The repair worker also restores the entire trusted `.github`/`.agents` snapshot
unconditionally after its explicit immutable-head checkout, before native scope
preparation and inline restoration. It does not rely on the generated
PR-checkout step's conditional restoration, since that step is deliberately
ineligible in this dispatch context. Runtime prompts and shared tool imports
therefore cannot come from PR-edited files.

## Hosted-trial readiness

The native setup now initializes `/tmp/gh-aw/agent/security-findings.json`
directly from immutable scope metadata. The agent fills review content and
preserves identity fields; an untouched template is invalid. Both workers use
the shared fixed-capability MCP report writer. Unlike a general PowerShell
wildcard, it accepts only bounded report data, validates immutable identity,
and writes one preselected regular file. Immutable Git inspection also uses
fixed native read tools, with external diff/textconv, pagers, Git replacement
refs, and filesystem-monitor hooks disabled. No model shell execution is
granted. The inline reviewer has read/search context only.
The agent must not execute PR-controlled Cargo, formatting, build, or test
commands. The trusted reconstruction fetch is complete rather than blob-filtered
so later base-worktree materialization cannot require a removed authenticated
remote.

Compilation and contract tests alone do not establish hosted readiness. The obsolete
Rust 1.90 Linux test container has been removed; WTA's Windows APIs require the
new Windows validation boundary. Successful isolated final-patch validation still needs
an end-to-end proof. Source approval and native promotion are separate gates;
their composition, failed-validation rejection, stale-review rejection, and
digest mismatch are covered locally. Do not spend a
end-to-end repair trial without checking these final authorization surfaces. This limitation
does not authorize disabling repair validation or claiming an untested fix.

A supported candidate is an ordinary `windows-2025-vs2026` validation job using its
preinstalled Windows Docker daemon, while gh-aw reasoning stays on Linux.
The published label means Windows Server 2025 with Visual Studio 2026; it is
not `windows-2026`. The repository's primary and packaging SDK pins are
10.0.26100.0, matching that runner inventory. Its default Rust 1.98.1 does not
establish compatibility with `ms-prod-1.93`; public Rust 1.93.0 can establish
version compatibility but not Microsoft production-toolchain provenance.
The official Rust image inventory does not provide a Windows MSVC image. The
trusted `build/containers/wta-validation` definition builds one from the pinned
Server Core base, checksum/signature-verified public VS2026 bootstrapper, and
checksum-pinned rustup installer. The executor uses its immutable local image
ID, not a mutable tag. Do not substitute the Linux image, guess a Windows Rust tag, copy arbitrary
host toolchain directories, or enable nested Hyper-V as a workaround.
The full image proof below now establishes actual public tool installation and
offline Windows-target execution. Source approval, final-patch digest binding,
and publication remain separate gates; a successful baseline image does not
by itself prove an arbitrary repair or semantic model correctness.

### Verified public environment

The separate native
[`ghaw-environment-probe.yml`](https://github.com/microsoft/intelligent-terminal/blob/1cdd1eef3e298c9da0107e2b877196621a69a95a/.github/workflows/ghaw-environment-probe.yml)
passed in the official repository on a non-main branch:
[run 37208378983](https://github.com/microsoft/intelligent-terminal/actions/runs/37208378983),
commit `1cdd1eef3e298c9da0107e2b877196621a69a95a`, in 14 minutes 20 seconds.
It used `windows-2025-vs2026`, image `20260925.250.1`, SDK 10.0.26100.0,
and public Rust 1.93.0 with the Windows MSVC target, without private ADO setup.

C++ and static-CRT Rust samples compiled and executed. The trusted WTA baseline
passed `cargo build --locked --offline` and the full Windows-target test command:
**2,444 passed, 0 failed, 1 ignored, 0 filtered out**. The pinned Server Core
container exited 0 with process isolation, `network: none`, and a read-only
input mount. Its guest probe checked a denied input write, absent selected
host credential/environment variables, and failed public egress against a
host-positive-control endpoint. Owned-container cleanup passed.

These are actual host/build/base-container results, not full C++ product,
packaging/UI, arbitrary network-containment, or untrusted automatic-repair
proof. Official-repository runs do not consume the three private-test attempts.

The full isolated image
[run 37394030034](https://github.com/microsoft/intelligent-terminal/actions/runs/37394030034)
passed in **22m42s**, at definition commit
`252f7b6e939b4f98075e9ac9f22eadc0e2087330`, against trusted source
`c40ab2727a3c5c498d320ffe90b761f2982c561f`. The built image was
`sha256:b66d36ad51a2c6e1b4dd01129a69f1d36a3eaccb7922d784ff61222eff159462`.
It installed public VS2026/MSVC/SDK, Rust 1.93.0, and PowerShell 7.6.6, then ran
the full WTA suite as `ContainerUser` with network disabled and all source/cache
mounts read-only: **2,444 passed, 0 failed, 1 ignored, 0 filtered out**.
Output stays inside the container. WTA's documented hook-bundle override points
at immutable assets because target output is outside the dev tree; environment
mutation tests are serialized according to their existing CI requirement.
Automatic dev-tree discovery is not claimed tested by that override.

Real hosted guide/report transport also passed:
[run 37390688923](https://github.com/microsoft/intelligent-terminal/actions/runs/37390688923),
test commit `60c408576ee560a0ce9131273c161676c1e92ae6`, against immutable COM
fork PR #1075. Native scope, unchanged-worktree, schema, and exactly-one-noop
checks passed. Model execution took 4m38s; recorded total usage was 94.11423 AIC.
The remaining low-severity notification suggestion was independently declined
as UX noise, not a vulnerability; the skill now explicitly excludes such
suggestions. This evidence proves transport, not universal finding relevance.
That run used the earlier PowerShell report path. The replacement data-only
MCP capability boundary separately passed
[run 37399159887](https://github.com/microsoft/intelligent-terminal/actions/runs/37399159887),
test commit `2130542495c4ad9469a1b48f19de7c59df7816a5`. The real model used the
immutable-read tools and fixed report writer; native unchanged-worktree, scope,
report and noop checks passed. Model execution took 20m33s; recorded total usage
was 243.54923 AIC. Its
remaining LOW robustness suggestion lacked a demonstrated attacker path and
is not accepted as a proved security regression. Transport and finding quality
must remain separate claims; no native post-filter fabricates no findings.

## Local validation

```powershell
node --test .github\skills\ghaw-pr-security\scripts\security-review.test.mjs
gh aw compile ghaw-pr-security
gh aw compile ghaw-pr-security-guide-fork
gh aw validate ghaw-pr-security ghaw-pr-security-guide-fork
```

For preflight, also lint the ordinary controller with actionlint and check the
native Bash step bodies with ShellCheck. Standalone actionlint 1.7.12 does not
recognize `copilot-requests` or `concurrency.queue`; gh-aw documents those exact
compatibility exceptions. Both fields are present in successful localization
runs at their recorded workflow SHAs. Do not remove required fields or suppress
unrelated diagnostics to manufacture a clean result. Validate that `queue: max`
is not combined with `cancel-in-progress: true`.

The three-attempt private-test budget includes setup, image-only/native runs,
failures, cancellations, reruns, and automatic triggers, not just AI execution.
Private-repository Copilot authentication is a separate prerequisite: the
built-in token worked in an existing personal-owned test repository, but that
does not prove a new repository's inference eligibility. The documented PAT
alternative uses the `COPILOT_GITHUB_TOKEN` Actions secret and omits
`copilot-requests: write` in the test workflow source. Keeping that permission
would select built-in-token inference instead. Do not expose token values,
silently change production authentication, or spend a trial guessing access.

No repository secrets are required beyond the standard gh-aw Copilot request
configuration. Enabling the workflow requires owner approval for that existing
configuration and branch protection to require the new check. No label or
assignable-user mapping is used.
