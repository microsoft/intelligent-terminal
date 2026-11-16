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
