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
  push one validated automatic repair.
- Fork: `ghaw-pr-security-guide-fork.md` stays on the trusted workflow revision,
  inspects immutable fork objects read-only, and may post one guidance comment.

This split follows the existing localization controller because gh-aw PR safe
output cannot combine a branch commit and a PR comment in one worker.

Before inference, the trusted script validates the immutable observed-base/head
commits, resolves their merge base, normalizes changed paths, classifies
affected trust boundaries, and records a scope hash. After inference, a fresh
API read must still return the same head.
Each worker rematerializes the validator from `github.workflow_sha`, not from
agent-edited bytes.

The thin custom agents delegate the reusable reasoning and report procedure to
the frontmatter-installed `.github/skills/ghaw-pr-security/SKILL.md`; its
bundled script owns mechanical scope/report validation. Installing the skill
through gh-aw keeps the runtime procedure tied to the trusted workflow
revision, rather than trusting a PR-edited copy. The workflows own only
checkout identity, permissions, trusted script materialization, freshness,
safe-output policy, artifacts, and conclusions. This matches the existing
localization separation between controller/worker orchestration, minimal role
agents, and the `ensure-localization` skill.

The report contract limits findings to changed files and assigns stable
`ITSEC-<hash>` IDs from rule/category/path/line. It independently enforces:

- HIGH findings are blocking; medium/low findings are advice-only.
- HIGH/high-confidence findings cannot rely only on hypotheses.
- analyzer/native checks cannot be marked passing without a matching run URL or
  explicit local command record.
- secret-like content, unsafe paths, malformed reports, stale SHAs, more than 20
  findings, and invalid `fixed` claims are rejected.
- the job summary separates blocking HIGH findings from considerations and the
  validated JSON is retained for 14 days.

Automatic repair is allowed only when the original finding is HIGH with high
confidence and strong evidence, the patch is minimal and inside `src/**`,
`tools/wta/src/**`, or `test/**`, applicable final validation passes, no check
is failed/blocked, an independent reviewer returns `PASS`, and the live PR head
still equals the reviewed SHA. The native validator compares every reported
patch path with the actual worktree and rejects CI/security policy, manifests,
unrelated dependencies, and medium/low edits. Unsafe or unvalidated HIGH
findings remain blocking.

The same-repo worker emits exactly one branch push for a non-empty validated
patch or one `noop`; it never comments. The fork worker emits exactly one
idempotent comment when findings exist or one `noop`; it never writes code.
The controller downloads the validated card and fails the public check when
unfixed HIGH findings remain.

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
skipped/blocked, never as a passing native check.

## Local validation

```powershell
node --test .github\skills\ghaw-pr-security\scripts\security-review.test.mjs
gh aw compile ghaw-pr-security
gh aw compile ghaw-pr-security-guide-fork
gh aw validate ghaw-pr-security ghaw-pr-security-guide-fork
```

No repository secrets are required beyond the standard gh-aw Copilot request
configuration. Enabling the workflow requires owner approval for that existing
configuration and branch protection to require the new check. No label or
assignable-user mapping is used.
