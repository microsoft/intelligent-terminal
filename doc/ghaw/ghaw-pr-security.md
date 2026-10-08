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

Workflow-root permissions are empty. The analysis/dispatch job has only
`actions: write`, `contents: read`, and `pull-requests: read`; it processes
untrusted Git objects without repository/issue write authority. After trusted
validation, a canonical same-run handoff selects mutually exclusive publisher
jobs. Repair publication has only `contents: write` plus PR read; guidance
publication has only `issues: write` plus PR read. Publisher code is
rematerialized from the trusted base, not supplied by the handoff artifact.
Matching run/attempt/revision/source jobs, detector outcome, scope and patch
identity, applicable native attestation and live-head checks remain mandatory.

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

Same-repository reports without a repair remain visible in the blocking/advice
check card and retained artifacts; they do not add routine PR comments. Fork
findings use SHA-labelled idempotent guidance comments. A published repair never
also produces a guidance comment. This preserves the existing non-noisy
publication behavior while separating token authority.

## Direct result artifact

The agent submits one structured security report through the native report
tool. Findings are not reconstructed from console logs or CLI transcripts.
Trusted validation and canonical publication emit `security-summary.md` directly
from that report, and the controller uses the file for the PR check and fork
guidance.

The artifact has a findings table with severity, fix status, location/rule,
confidence and evidence/reason columns, followed by a separate validation table.
Unresolved HIGH findings come first, followed by verified HIGH fixes and
medium/low advice; ties have stable location/rule ordering. Empty findings still
produce an explicit result row. Failed, blocked and skipped validation remain
distinct. Only matching-head native validation can authorize a Fixed row.

Before inference, the trusted script validates the immutable observed-base/head
commits, resolves their merge base, normalizes changed paths, classifies
affected trust boundaries, and records a scope hash. After inference, a fresh
API read must still return the same head.
Rename/copy classification includes both source and destination paths, so moving
a sensitive file into an unrelated directory does not suppress review. All
`.github/**` automation, including policies and instructions, is included in
both triggering and classification. The `tools/**` surface also covers build
entrypoints such as `tools/razzle.cmd`, not only WTA.
Root `AGENTS.md` and `.agents/**`, a configured trusted restore root, are also
triggered and classified as `workflow-credentials`.
Instruction-only changes and instruction/WTA mixed scopes cannot enter automatic
Rust repair. These roots match the workers' `GH_AW_AGENT_FILES: AGENTS.md` and
`GH_AW_AGENT_FOLDERS: ".agents .github"`; other possible CLI roots are not
implicitly added to this coverage.
Root `.cargo/**` configuration is included in both the controller trigger and
build-tooling classification; `.cargo`-only and mixed scopes are guidance-only,
never eligible for automatic Rust repair. Report validation rejects common bearer
and labeled session-capability forms without claiming universal secret detection.
The `installer/**` tree is likewise included in triggering and build/package
classification, covering packaged/unpackaged installation scripts and bootstrap
source; bootstrap Cargo manifests/locks also receive supply-chain classification.
Installer-only and mixed installer/WTA scopes remain outside automatic repair.
Each worker rematerializes the validator from `github.workflow_sha`, not from
agent-edited bytes.

Both workers import the same mode-aware
`.github/agents/ghaw-pr-security.agent.md` and install the same
`.github/skills/ghaw-pr-security/SKILL.md`; the skill's bundled script owns
mechanical scope/report validation. Both also select that native primary with
`engine.agent: ghaw-pr-security`; importing its text alone does not replace
the CLI's default primary or its generic security-review delegation. Both pin
CLI 1.0.90, matching the actual restricted-model validation rather than relying
on the compiler fallback or a changing compatibility-map selection.
The primary profile explicitly lists only trusted-input reads, skill
invocation, the five native capabilities and `safeoutputs/noop`. Selecting a
custom profile without a tool list would otherwise expose the CLI's default
shell/edit tools, even when the workflow supplies only native MCP approvals.
The workflow selects `guide` or `repair`
mode and supplies the corresponding tools, so review-only and repair behavior
do not require duplicate primary agents. Neither primary nor reviewer exposes
generic delegation. The guide also excludes `task`, `read_agent`, `write_agent`,
and `list_agents` in the native CLI invocation.

Repair uses a trusted sequential driver inside the existing agent sandbox.
After the primary exits, the driver validates its pending candidate and alone
launches the fixed `ghaw-pr-security-reviewer` profile. The reviewer has only
trusted reads and the three read-only native inspection capabilities; it cannot
edit, dispatch another agent, submit reports, or emit safe outputs. The driver
requires successful native diff/source/candidate reads and a structured
`SOURCE_PASS` matching the immutable head and final patch digest before stamping
source approval. Model-authored approval is rejected by report submission.
For every proposed finding, successful bounded native head reads must cover its
reported line interval. Native base reads must cover the corresponding original
hunk/context, accounting for insertion and deletion offsets. Read fragments may
form a complete interval union; gaps, wrong revisions, mismatched immutable
source bytes, and `view`-only substitutes do not satisfy this gate.
Missing evidence, failed review, malformed output, or subprocess failure stops
the repair rather than accepting a prompt-only reviewer restriction.

The driver's code and validator are rematerialized from the trusted workflow
revision into the sandbox's read-only native mount. `engine.command` disables
the compiler's ordinary CLI installation, so both workers explicitly
reuses the same pinned official installer and stages its executable before
inference. The compiler's detector remains on its ordinary CLI execution path,
not the security driver. Installing the agents and skill through gh-aw keeps
them tied to the trusted workflow revision rather than PR-edited copies.
Each driver subprocess has a nine-minute bound. The outer harness inactivity
watchdog is ten minutes. A fixed, non-secret phase-transition diagnostic resets
its activity timer before each child launch, so the primary's accepted terminal
`noop` cannot prematurely end an independent review within its bound.
Harness retries are disabled to avoid replaying a completed primary and
exhausting its one-output capability.

Guide mode uses the same driver without invoking a repair reviewer. Complete
native transcripts are checked in memory, not forwarded or retained in public
driver logs. Public output contains a validated report-derived summary and
bounded status/usage metadata, rather than raw tool results or model analysis.
Native Git subprocess diagnostics are captured; failures expose static messages.

Before native services or inference, both workers verify the exact hashes and
replacement counts of three installed assets from the pinned gh-aw-actions
revision. The preparation disables raw CLI session copying and routes native
MCP server diagnostics outside summary/artifact collection roots. Hash, path,
or preparation failures stop the ordinary inference steps. All asset checks
precede any writes. The third asset is `safe_outputs_handlers.cjs`: its canonical
default handler constructs noop entries using only the trusted fixed message
before any JSONL persistence, retaining native count/max enforcement and the
ordinary framework output row. All model-supplied noop arguments, including
reserved or unknown fields, are discarded at that point. The pinned runtime
has no supported fixed-message configuration; this worker-local adjustment
does not fork the upstream service or replace the noop capability. Late queue
validation asserts the exact fixed payload as a postcondition, not as the
privacy boundary. Public review content still comes from the validated native
report. Raw RPC inputs remain in private service diagnostics. MCP gateway logs
use a literal private-directory override through supported `sandbox.mcp.env`.
The generated `safeoutputs` stdio service is a separate third logging service.
Its `GH_AW_MCP_LOG_DIR` is overridden through the same supported gateway
transport to `${RUNNER_TEMP}/gh-aw/safeoutputs/private-security-logs`. Preparation
validates and creates that owner-only directory before startup. This path is in
the service's existing read-write mount, outside every configured log collector
root; routing only the gateway or moving an existing public `mcp-logs` directory
does not stop the service from recreating public logs.
Generated directory preparation and pinned gateway startup still recreate empty
public log directory trees. The acceptance test must distinguish those regular
empty directories from payloads: require exactly zero public files recursively,
rejecting even zero-byte files, symlinks/reparse escapes, and irregular nodes.
Do not infer failure from directory existence alone or remove evidence after
inference. This is a test-threshold correction, not a new production quarantine
or a benign-log-file exception.
Each driver phase has a private configuration home and debug-log directory;
compiler-provided configuration/log overrides cannot redirect those logs back
into collected roots. Original session stores and historical evidence are not
deleted. Updating the runtime pin requires re-verifying these asset hashes and
collector paths.

This ordering assumes the supported fresh GitHub-hosted `ubuntu-latest` model
jobs, without retained HOME/session restoration before preparation. The pinned
collector still runs after a failed preparation; a retained-image deployment
could therefore expose pre-existing sessions. Changing runner or restore inputs
requires a separate fail-closed collection review.

The native report, repair patch and noop outputs remain available to trusted
validation and publication. Conversational diagnostics are intentionally
reduced; this is not a claim of unchanged detector visibility, universal secret
detection, or a separate confidentiality boundary against the same process.
Native `noop.message` is still free-form public output, retained by canonical
JSONL and downstream staged summaries. Logger routing does not sanitize it:
source/secret-bearing noop prose would be a separate output-validation gap.
Completion messages must not include raw source or secrets. Canonical noop,
report, patch, collector, and controller semantics remain unchanged.

Both workers clear the configured telemetry credential/composite variables
`OTEL_EXPORTER_OTLP_HEADERS`, `GH_AW_OTLP_ENDPOINTS`, and
`GH_AW_OTLP_ALL_HEADERS` through supported `engine.env` overrides. These become
empty step-level values before the host shell and AWF sandbox start, so they
are absent as credentials from the wrapper's initial environment, not just a
filtered model-child environment. Pinned AWF's API-proxy exclusion set already
removes GitHub token variables from the model container while preserving
host-side inference authentication and model transport. Native runner/gateway
telemetry steps remain unchanged; authenticated AWF inference-side telemetry
export may be unavailable. This covers the configured credential paths, not
every possible unknown secret variable. Local direct-inference proofs use an
explicit caller-supplied inference credential and are not evidence of the
hosted proxy's token-free model environment.

Fork object fetch authentication is supplied through process-scoped
`GIT_CONFIG_*` environment, not token-bearing Git arguments. The trusted fetch
step disables command tracing and clears its temporary header/config variables
on success and failure before model launch. This prevents this fetch's argv
disclosure without claiming universal environment secrecy.

The same-repository trusted fetch installs EXIT cleanup before using its
askpass helper. Both explicit success cleanup and failure cleanup clear the
credential environment and remove the exact helper, preserving the original
failure exit status.

Native Windows validation pins the Windows Docker pipe and clears inherited
Docker context selection. Before dependency/image work, the trusted
`build/scripts/Wait-WindowsDocker.ps1` helper verifies the installed Windows
container feature, client and registered service, starts only a stopped Docker
service, and waits for a Windows-engine response. Process operations and
readiness have explicit bounds and retain raw diagnostic output. A terminated
Docker-info probe timeout is retryable only within the same readiness deadline;
service-start and child-termination failures remain fatal. Cleanup queries
Docker only when this run recorded a container-creation attempt and still
checks exact name/labels before removal.

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
- unknown top-level report properties are rejected before content validation or
  serialization; native output uses an explicit envelope, so unvalidated
  extension payloads cannot bypass the known-field secret checks.
- the job summary separates blocking HIGH findings from considerations and the
  validated JSON is retained for 14 days.

Automatic repair is allowed only when the original finding is HIGH with high
confidence and strong evidence, the patch is minimal and inside existing
`tools/wta/src/**/*.rs` files, applicable final validation passes, no check
is failed/blocked, an independent reviewer returns `SOURCE_PASS` for the immutable
head and exact final patch digest, and the live PR head still equals the
reviewed SHA. Trusted scope generation derives immutable head hunk anchors and
base deletion mappings directly from Git and binds them into the scope hash.
Common candidate/proposal/final validation requires every proposed or fixed
repair anchor to overlap those changes; changing a file alone does not authorize
repairing an unrelated unchanged line. Missing hunk authority fails closed.
Actual repair additions/deletions are separately constrained to authorized
immutable HEAD ranges, including permitted insertion/deletion boundaries.
Every change segment must satisfy the range guard; a large hunk merely touching
an authorized line cannot carry unrelated same-file edits. Prospective writer
output is checked before any source byte changes, and staging/canonical
publication enforce the same rule. Nonempty patch validation requires trusted
scope explicitly; no unscoped exported-API fallback is accepted.
Blocked guidance may still explain unchanged context. This structural gate is
not a claim that native code proves security causality; independent reasoning
must still establish a regression introduced by the diff.
Model reasoning remains defense in depth, not a separate
credential principal; trusted invocation, native checks and safe-output policy
provide the mechanical boundaries. The primary reports only pending
`proposed` candidates. A pending candidate cannot enter proposal validation,
native test attestation, or publication until the trusted driver records
independent source approval. That approval occurs before trusted test execution
and does not claim test success. Native attestation alone promotes an exact, source-approved proposal
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
Generated skill installation stamps `metadata.local-path` into the tracked
skill header. After inline skills are restored, a native pre-agent step restores
that file byte-for-byte from the trusted workflow revision, before the guide
baseline or inference. Runtime metadata must not contaminate the candidate
diff or invalidate an otherwise authorized Rust repair; the inspector still
checks the entire diff rather than silently ignoring instruction changes.

## Hosted-trial readiness

The native setup now initializes `/tmp/gh-aw/agent/security-findings.json`
directly from immutable scope metadata. The agent fills review content and
preserves identity fields; an untouched template is invalid. Both workers use
the shared fixed-capability MCP report writer. Unlike a general PowerShell
wildcard, it accepts only bounded report data, validates immutable identity,
and writes one preselected regular file. Immutable Git inspection also uses
fixed native read tools, with external diff/textconv, pagers, Git replacement
refs, and filesystem-monitor hooks disabled. No model shell execution is
granted. The inline reviewer has file read and three specific read-only native
MCP capabilities, but no shell, writer, report-submission, or agent tools.
The default GitHub MCP server is explicitly disabled; an omitted `tools.github`
key would otherwise auto-add mutable API reads. `bash: []` and
`cli-proxy: false` select native MCP transport without a model shell. Native
preparation/post-validation API checks retain their separate read token; it is
not exposed as an agent GitHub tool.
Generic editing is disabled in repair mode too. `write-security-repair` accepts
only 1-8 unique exact-text replacements in `edits_json`, totaling at most 8 KiB,
for existing modified WTA Rust files from protected same-repository scope.
All replacements are checked before any bytes are written; whole-file source
payloads are not accepted. The writer verifies an immutable regular Git blob
and rejects symlink escapes. It cannot alter reports, workflows, Git metadata
or unrelated files. Report JSON is limited to 10 KiB, matching the actual native
MCP string limit. The compiler does not preserve custom input maxLength fields,
so these limits do not depend on a schema override.
Proposed code still requires independent source approval and native tests.
The independent reviewer fetches the complete native original/candidate diff
and relevant immutable source itself through read-only native tools. Parent
copies or paraphrased source do not substitute for those reads. Its approval
must bind the final native digest and immutable head, stored separately in
`review.patchSha256` and `review.headSha`.
The agent must not execute PR-controlled Cargo, formatting, build, or test
commands. The trusted reconstruction fetch is complete rather than blob-filtered
so later base-worktree materialization cannot require a removed authenticated
remote.
The custom pre-agent cleanup is fail-closed and checks the resulting Git
configuration for retained helpers, authentication headers and authenticated
remote URLs. It does not rely only on the compiler's later best-effort cleanup.

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
checksum-pinned rustup installer.
Host dependency preparation likewise uses the checksum-pinned public rustup
1.29.1 installer, isolated Cargo/Rustup directories, and explicit installed
executables rather than the mutable preinstalled runner toolchain.
The verified host bootstrapper keeps its required `rustup-init.exe` basename:
rustup uses executable-name dispatch, and a prefixed filename enters proxy mode
and exits before installation. Host installer stdout/stderr are retained with
native proof so setup failures are diagnosable.
The executor uses its immutable local image ID, not a mutable tag. Do not
substitute the Linux image, guess a Windows Rust tag, copy arbitrary
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
