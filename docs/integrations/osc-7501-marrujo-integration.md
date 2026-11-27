# OSC 7501 Integration — Marrujo Global Legal Enterprises LLC

Status: design-only integration scaffold
Upstream reference: https://github.com/microsoft/intelligent-terminal/pull/1388
Upstream protocol notes: https://github.com/microsoft/intelligent-terminal/blob/fef550e3f7b92382fcdd861f0fe768d9747dc003/doc/program-status.md

## Repository and change controls

- Repository: `jmarrujo91-lab/intelligent-terminal`
- Base branch: `main`
- Integration branch: `feature/osc-7501-marrujo-integration`
- Preserve repository owner, fork relationship, commit history, existing branches, and open pull requests.
- This branch is an isolated proposal. Do not merge it, deploy it, change repository permissions, or modify live services without separate approval.
- Do not cherry-pick or copy the entire upstream PR as an unreviewed bulk change.

## Integration decision

Treat OSC 7501 as an additive terminal-status signal. Keep terminal protocol parsing and native Windows Terminal integration in the terminal layer. Any Marrujo service-side consumer must receive only normalized, validated status events through an explicit adapter boundary; it must not attempt to parse raw terminal escape sequences from arbitrary application logs.

The status signal is observational only. It must never authorize commands, approve deployments, grant credentials, change payment state, or trigger external side effects.

## Protocol facts to preserve

The upstream PR documents revision 0.4 and the following states: `idle`, `working`, `blocked`, `done`, `error`, plus `clear`.

- The wire field for explanatory text is `msg` (base64 UTF-8); internal snapshots expose it as `message`.
- Task records are scoped to the actual terminal connection, not inferred from a title, executable, or conversation.
- Reports replace a record rather than patching optional fields.
- Blocked/input-required and canonical hook/ACP activity must take precedence over conflicting program-reported states.
- Completion is a task result, not proof that the process or agent session ended.
- Reject stale/duplicate revisions within the relevant connection epoch.
- Bound message size, task count, hierarchy depth, identifiers, and decoded text.
- Treat producer text as untrusted plain text; never execute it, open links from it, or interpret it as authorization.
- Do not infer completion from a timeout or missing heartbeat.

## Marrujo adapter boundary

If a separate service adapter is implemented later, accept a normalized event shape similar to:

```ts
type ProgramState = "idle" | "working" | "blocked" | "done" | "error" | "clear";

interface ProgramStatusEvent {
  schemaVersion: 1;
  protocolRevision: "0.4";
  connectionId: string;
  epoch: number;
  taskId: string;
  parentTaskId?: string;
  state: ProgramState;
  message?: string;
  title?: string;
  progress?: number;
  observedAt: string;
  sequence?: number;
}
```

This is a proposed internal contract, not a claim that the upstream wire protocol uses this exact JSON structure. Validate every field at the adapter boundary (Zod in TypeScript). Reject invalid UTF-8/base64, control characters, out-of-range progress, oversized strings, invalid IDs, and events from stale connection epochs.

## Data handling

- Persist only the minimum normalized operational status needed for diagnostics.
- Do not persist raw terminal output, shell commands, secrets, payment details, identity documents, or credit-monitoring identifiers.
- Keep operational status records separate from Stripe/payment records and identity/credit-monitoring records.
- Use access-controlled audit records for administrative access and state changes.
- Encrypt any sensitive persisted fields using the approved KMS-managed encryption design.
- Set retention limits and document deletion behavior before enabling persistence.
- Do not send terminal message text to Notion or third-party systems by default; publish only approved, redacted summaries.

## Implementation phases and acceptance gates

1. **Upstream review:** inspect PR #1388 files, review comments, check runs, licensing, and unresolved review items. Do not assume the PR is approved or production-ready.
2. **Native terminal validation:** build the exact fork revision and run parser, adapter, lifecycle, and presentation tests. Add live tests for real supported CLI versions and verify PowerShell, Bash, WSL, SSH, tmux, and screen behavior separately.
3. **Local-only prototype:** add a disabled-by-default status adapter; no network forwarding or persistence by default.
4. **Security validation:** fuzz malformed and fragmented OSC sequences; test size limits, base64/UTF-8 validation, stale epochs, duplicate revisions, task-tree limits, untrusted text rendering, and prompt/connection cleanup.
5. **Service integration:** only after explicit approval, expose normalized status events to a sandbox service with authentication, authorization, redaction, and audit logging.
6. **Release review:** require human review of test evidence, security findings, data retention, and operational rollback before any merge or deployment.

## Current limitations and open checks

- The upstream documentation states that tmux/screen may filter OSC sequences and that WSL/SSH/multiplexer paths require separate live verification.
- The upstream PR's optional AI security review was reported unavailable; that is not a security approval.
- Upstream review feedback identified reused checklist IDs that should be corrected before treating the upstream change as release-ready.
- This scaffold does not implement a parser, modify terminal runtime behavior, create credentials, configure CI/CD, or deploy anything.

## Approval boundary

A separate explicit approval is required before:
- implementing or enabling raw OSC ingestion beyond local tests;
- forwarding status outside the local terminal process;
- persisting terminal status events;
- changing GitHub permissions or branch protection;
- opening a non-draft PR if that implies readiness for review;
- merging, tagging, releasing, or deploying.


## Upstream PR #1388 audit — 2026-11-16

Audit source: https://github.com/microsoft/intelligent-terminal/pull/1388

Observed upstream state at audit time:
- PR is open, not merged, and targets `main`.
- Head: `fef550e3f7b92382fcdd861f0fe768d9747dc003`; base: `1048e7c38569056fe25a5055e26a5ea26fbffdcf`.
- 24 commits; 242 changed files; 8,239 additions and 203 deletions.
- The PR contains native C++ parser/adapter/ConPTY/Terminal UI changes, Rust WTA integration, shell fallback, E2E tests, and broad localization changes. It is not a small isolated protocol patch.
- The PR's description reports focused parser/adapter/WTA tests passing, but explicitly says full E2E and live WSL/SSH/multiplexer paths were not run.
- Review threads currently report no unresolved inline threads. Recent review summaries still call for human Windows lifecycle/accessibility validation, especially acknowledgment/attention behavior. A prior review identified an unread-state reconciliation concern; a later summary says no open issues but still requests human validation.
- Earlier review findings included checklist ID/title mapping, ABI slot ordering, session-identity arbitration, and localization correctness. Follow-up commits are recorded in the thread history as fixes; they still require verification against the final source tree before adoption.

Local compatibility check:
- Local `main` is `acc38beb5eead22d5dc0a01e93ad2dd89957e478`.
- Comparing local `main` to upstream PR head reports 144 commits ahead, with local main as the merge base.
- Therefore the upstream PR is not based on the current local fork head. Applying it wholesale would import 144 upstream commits and a large set of unrelated project changes, not merely OSC 7501 support.

Decision:
- **Do not cherry-pick or merge PR #1388 wholesale into this fork.**
- Keep this branch documentation-only until a compatibility strategy is approved: either update the fork from upstream in a separately reviewed operation, or isolate the minimum OSC 7501 parser/adapter changes and port them against the current fork APIs with focused tests.
- The current ChatGPT/GitHub connector session can inspect and edit text files, but cannot build the native Windows Terminal solution or execute the Windows/TAEF, Rust target, and live terminal integration test matrix. No build/test pass is claimed here.
- No upstream branch, local `main`, permissions, workflows, runtime settings, or deployment was changed by this audit.
