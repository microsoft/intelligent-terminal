---
created on: 2026-09-15
last updated: 2026-09-17
---

# Agent Center Collaboration Protocol v1

**Normative v1 implementation contract for the task-driven Terminal pilot**

This document closes the collaboration boundary described by the
[domain specification](agent-center.md) and exercised by the
[product journey](agent-center-product.md). It specifies the v1 messages,
responses, owners, state transitions and next actions. Names in operation
tables are protocol operations, not illustrative tool suggestions.

For this boundary, this document takes precedence over abbreviated payloads
in the domain specification. Product intent and authority remain unchanged.
This is a design contract, not a claim that the runtime is implemented or
production availability/security qualification is complete.

## 1. Scope and participants

The product north star is a Terminal where the user expresses work goals,
asks about progress and strategy, changes direction and accepts results without
managing internal shells, agents or provider context. V1 is the first
developer-work implementation boundary of that goal, not the complete OS
automation surface. The product specification owns user stories and outcome
metrics; this document owns supported wire semantics.

V1 closes this path:

```text
user message -> intake/approved work -> plan -> dispatch -> acknowledgment
 -> execution/progress/context -> submission -> check/review
 -> acceptance OR structured rework -> integrated candidate
 -> human revision OR acceptance -> usable local delivery
```

The baseline is a local work service, registered runtime adapters, ACP-capable
agents with work MCP tools, a native command-check adapter, and Console/CLI
clients. The delivery baseline is a fixed local code snapshot/commit and
readable reports. External publication, cloud execution and terminal screen
transport are separate adapters, not implicit steps in this protocol. Their
absence cannot turn local delivery into an unexplained pending operation.

Natural-language and slash/CLI entry points must converge on the same scoped
operations, guards and explicit human approvals. Answering "why this approach?"
must use authorized plan reasons and evidence; agreeing to "change approach"
without a recorded plan/change action is not an applied strategy. Ordinary
work discussion must not change the execution contract or suspend eligible
work. Cross-work questions require an appropriately authorized read scope;
a work-bound coordinator cannot infer access to foreign-work tools merely
because the user asked a global question. Missing supported-path capability
is an explicit limitation and an open acceptance gate, not invented status.

In the global Console, a HumanActionProposal is an inline approval card.
Receiving it never takes composer focus or submits a command. F6 opens a
captured review; Enter on its explicitly focused Approve/Approve and start
button submits that exact request after freshness and inspection checks.
Change requirements returns to the unchanged composer draft. Not now defers
only that proposal version in the current UI; neither option changes the Work,
grant, or proposal record. Deferred proposals remain available through F4, and
new versions become visible again. F5 expands the authoritative preview.
Plain Enter in the composer remains conversation input, not approval.

### 1.1 Product targets versus v1 capability

| Product target | V1 boundary / extension obligation |
|---|---|
| No mandatory shell/provider/session administration | Use configured authorized runtimes, dispatch and acknowledged inputs; configuration/consent is distinct from repeated human scheduling |
| Existing-resource adoption or replacement continuity | Qualify each adapter's ownership, settlement and input acknowledgment; visibility of an arbitrary OS process is not authority to control it |
| OS/application state as the actual result | Current draft delivery kinds remain `LocalCode` and `Report`; no new state-result enum, method or adapter is introduced by the product roadmap |
| Verified state-changing work | Before future support, specify exact target, authorization, pre/postconditions, observations/freshness, effect receipts, reconciliation and rollback limits; a report or successful process exit is not proof of the target state |
| Natural-language progress and strategy control | Use authorized domain facts and versioned plan/change operations; track missing read/intake paths against domain AC-91–AC-95 instead of treating command coverage as conversational acceptance |

An unimplemented operation keeps its explicit unsupported disposition. Future
OS capability must be specified, implemented and independently qualified before
being included in a supported-work cohort. Existing v1 payloads, method names,
authority and result validation are not relaxed by this scope clarification.
Historical protocol/adapter passes do not sign off a new product scenario.

### 1.2 Participants

| Actor | Receives | May produce |
|---|---|---|
| Console / human CLI | Read views, events, input requests | Conversation input, work approval/control, decision answers, manual handoff and final acceptance/revision |
| Work service | All domain requests and runtime observations | Authoritative records, responses, events and recorded effects |
| Scheduler / policy controller | Committed state changes | Dispatch, check admission, verdict application and candidate preparation |
| Coordinator driver | Actionable work/intake events | One coordinator invocation per scope and its continuation queue |
| Coordinator agent | Snapshot plus event batch | Answers and typed proposals/actions; never arbitrary authoritative status writes |
| Worker agent | TaskDispatch or Continuation | Acknowledgment, reports, context requests and the required terminal record |
| Checker / reviewer | Exact result and evaluation round | GateResult or TaskReview |
| Runtime adapter | Invocation/continuation/stop requests | Correlated observations, MCP forwarding and settlement receipts |

The scheduler and coordinator driver are service code. "Notify the agent"
means that this code issues an actual runtime invocation or continuation.
Publishing an event by itself does not run a model.

## 2. Transport and common envelopes

### Experimental new-project directory approval

`project.configure` accepts optional `createDirectory: bool` and
`rootPreference: EntityRef`. Omitted/false `createDirectory` retains the
existing absolute-directory contract. True requests one absent direct child
of an existing absolute parent and a local Git repository with an initial
empty commit. No recursive parent creation, existing-target adoption,
overwrite, provider shell invocation, or implicit work execution is permitted.

For a global `conversation.propose_action`, the proposed root must either be
explicitly supplied by captured human input, or, for new-directory creation,
be a direct child of the absolute directory stated in the captured active
User Preference keyed `workspace.code_root`. `rootPreference` pins that
preference's exact kind/id/version. The service compares the captured record
with its current active version at proposal and confirmation; correction or
forgetting invalidates the proposal. This is path provenance, not execution
authority. Other preferences, including the active project, do not grant
permission to create within or reuse another repository.

The frozen confirmation displays the exact canonical target, creation and
Git-initialization intent, approved capabilities, limits, and model destination.
Only human confirmation commits a `project.create` outbox operation. The
response is `pending`, with its `operationId` and reserved `projectId`; no
Project record is exposed until a matching successful filesystem receipt.
The runtime's existing effect intent/receipt journal makes retries idempotent
and reports an interrupted effect without receipt as `OUTCOME_UNKNOWN`.
Failures preserve partial files for reconciliation instead of deleting or
silently recreating the target.

The operation is readable by the owning global conversation. Both success
and failure attach a `completion` view to the submitted HumanActionProposal
and emit/queue `HumanActionCompleted`; the original pending submission receipt
remains unchanged. Pending project creation still emits `HumanActionSubmitted`
for the Console, but does not queue a coordinator turn: its reserved project
does not yet exist in a captured access snapshot. The terminal creation receipt
queues the follow-up exactly once. The next coordinator turn observes refreshed project
access and continues the user's original goal only after actual success.
While pending, it may finish with `WaitingOnRecordedSubject` for the operation;
it must not poll or require another human "continue" message.

### 2.1 Connection and identity

The Windows v1 connection is a duplex named pipe. Frames contain a four-byte
unsigned little-endian UTF-8 byte count followed by one JSON object, with a
maximum payload of 1,048,576 bytes. Large bodies use ArtifactRef. Malformed
JSON/framing receives a protocol error when possible and closes the connection;
unknown methods on a valid connection receive `unsupported`.

Header and payload bytes may arrive in separate writes. Service notifications
must preserve the in-flight frame reader, including a partially consumed length,
while subscription events continue to flow. A new frame starts only after the
previous frame completes; connection/service shutdown may cancel the reader.
Fragmentation does not relax the length/JSON checks or request correlation.

The first client frame is:

```json
{
  "type": "hello",
  "versions": [1],
  "clientInstanceId": "b3682581-cd49-4cef-9186-4c15e2770483",
  "clientKind": "Console"
}
```

`clientKind` is `Console | CLI | Runtime`; it is descriptive, not an authority
claim. The endpoint associates a verified principal/runtime binding using the
domain specification's local access boundary. Agent tools inherit their
service-issued invocation binding through the runtime; caller-supplied actor,
work or attempt fields cannot replace it.

The service returns `welcome` with required `version: 1`, `connectionId`,
`serviceInstanceId`, `storeId`, and `maxFrameBytes: 1048576`. No shared version
returns `protocol_error` with `code: INCOMPATIBLE_VERSION` and
`supportedVersions: [1]`, then closes. Runtime connections must register before
receiving invocations. Console closure removes subscriptions, not work.

Connection-level errors use `{type: "protocol_error", code, message,
supportedVersions?}`. Code is INVALID_FRAME or INCOMPATIBLE_VERSION; only the
latter includes supportedVersions. These are not request-correlated responses.

### 2.2 Value rules

Object keys below are case-sensitive. Fields are required unless marked `?`;
optional values are omitted, not supplied as null. Unknown fields in v1
operation payloads are rejected. Arrays may be empty unless stated otherwise.

| Type | Definition |
|---|---|
| `Id` | Opaque string; service record IDs are UUIDs. Client command/request IDs are UUIDs created before sending |
| `Revision` | Positive integer. Contract/spec/plan revisions are immutable versions; a record's `version` is its mutable-state version |
| `Cursor` | Opaque string containing store/stream position; never a client timestamp |
| `EntityRef` | `{kind: string, id: Id, version: Revision}` where kind is one of the record types named in this document |
| `ArtifactRef` | `{artifactId: Id, digest: string}`; digest is `sha256:` plus lowercase hex for the captured manifest/content |
| `InputRef` | `{slot: string, artifact: ArtifactRef, sourceResultId?: Id, sourceGateIds: Id[]}` |
| `Failure` | `{code: string, message: string, fieldErrors: {path: string, message: string}[], recovery: "none" or "refresh" or "same_command" or "repair", subjects: EntityRef[]}` |

All references are resolved in the connected store. IDs in a request must agree
with its binding and referenced records. State-changing commands do not resolve
"current work" from window focus at execution time.

EntityRef.kind uses these exact wire names where applicable: Work, Task,
TaskResult, Workspace, DeliveryCandidate, ChangeProposal, PlanProposal,
GrantProposal, DecisionRequest, IntakeRequest, ContextRequest, Operation,
Artifact, Conversation, CoordinationTurn, Plan, Attempt, TaskDispatch,
GateResult, TaskReview, AttentionItem and Preference. Work denotes the domain WorkItem.

### 2.3 Request and response

```json
{
  "type": "request",
  "requestId": "1d4c6391-ab29-41bb-b2d1-5af9e3e8e5b9",
  "commandId": "90ece87e-d232-43cc-8d8b-f0b1e5795a08",
  "method": "task.report_progress",
  "ifMatch": [],
  "params": {
    "dispatchId": "dff34d83-858f-492a-b4cd-6ffdeffcc132",
    "taskRevision": 1,
    "activity": "Checking the export boundary",
    "findings": [],
    "artifacts": [],
    "nextStep": "Submit the fix and regression evidence"
  }
}
```

Required request fields are `type`, `requestId`, `method`, `ifMatch: EntityRef[]`,
and `params` as defined by the operation. Mutations additionally require
`commandId`; reads omit it. A fresh transport retry uses a new requestId and
the same commandId and semantic payload. Runtime-directed requests use the
same envelope in the opposite direction on a registered runtime connection.

`ifMatch` contains the mutable subjects required by each operation table.
Worker append/submission calls use their immutable dispatch/task revision
instead of the .
The service still checks current authority and whether the dispatch remains
eligible. Reads and append-only bound calls use an empty ifMatch.

Every request has one correlated response:

```json
{
  "type": "response",
  "requestId": "1d4c6391-ab29-41bb-b2d1-5af9e3e8e5b9",
  "status": "ok",
  "data": {
    "reportId": "7c382d06-34f9-4a03-8c9a-da0114b15fe8",
    "eventId": "579715f6-7ca8-4aa4-a5fb-a78069f6ac11"
  },
  "subjects": [],
  "cursor": "store-stream-position"
}
```

Required response fields are `type`, `requestId`, `status`, `subjects:
EntityRef[]`; `data`, `failure`, `operationId`, `inputRequest`, and `cursor`
are present only as specified below. Cursor is the committed position when
there is one, not a promise that an external action finished.

| Status | Required additional fields | Meaning |
|---|---|---|
| `ok` | data | This operation's defined result is recorded/read; a submitted result can still await review |
| `pending` | operationId, data | Durable effect accepted; read operation.get or subscribe for completion |
| `needs_input` | inputRequest `{kind: "Decision" or "Context" or "Intake", id: Id, version: Revision, responseSchema: object}` | Recorded question; no invisible prompt and no repeated command necessary |
| `conflict` | failure with current subjects | No new requested mutation; refresh and issue a new command if still intended |
| `unsupported` | failure | Method/capability unavailable; no dispatch |
| `error` | failure; operationId if an effect had already been accepted | Explicit failure; consult operation state before any replacement effect |

`responseSchema` uses JSON Schema Draft 2020-12 and is bounded by the frame
limit. Its validated answer is the only open-shaped value in decision payloads.
An `ok` response to result.submit is not an accepted task.

The deduplication key is `(storeId, principalId, logicalInvocationId?, commandId)`;
the logical invocation survives a transport reconnect or binding-generation
change. Connection IDs and transport request IDs are not deduplication scope.
An identical
method, ifMatch and params returns the original response body with the new
requestId, even if the operation has since advanced. Different content returns
`COMMAND_ID_REUSED`. Query the recorded operation/result for its current state.
Events, state changes and deduplication outcome commit together.

### 2.4 Errors and CLI mapping

| Failure code | Status | Required handling |
|---|---|---|
| INVALID_ARGUMENT, INVALID_REFERENCE | error | Correct the named field/reference with a new command |
| FORBIDDEN | error | Obtain appropriate authority; never change an actor field to retry |
| STALE_VERSION, STALE_DISPATCH, STALE_EVALUATION | conflict | Refresh the subject; do not retarget the old answer or submission |
| BAD_STATE, CONTRACT_UNACKNOWLEDGED, WRONG_TERMINAL_RECORD | error | Follow the indicated task/dispatch state; no inferred completion |
| COMMAND_ID_REUSED | error | Preserve the original command; a different intention needs a new ID |
| CAPABILITY_UNAVAILABLE, METHOD_UNSUPPORTED | unsupported | Select a declared supported path or expose the blocker |
| ARTIFACT_UNAVAILABLE, EXECUTION_FAILED, PROTOCOL_INCOMPLETE | error | Retain the failed record and evidence; bounded coordination decides the next action |
| OUTCOME_UNKNOWN | error | Include operationId; reconcile/repair, do not blindly repeat effects |
| CURSOR_EXPIRED, RESYNC_REQUIRED | error | Obtain a fresh snapshot/subscription |

Additional executor-specific detail belongs in message/fieldErrors, not a new
unregistered top-level code. CLI JSON uses the response body; diagnostics use
stderr. Exit codes are 0 for ok, 2 for pending, 3 for needs_input, 4 for conflict,
5 for unsupported, and 1 for error. Pending is distinguishable from completed
success. Event commands stay attached until cancellation, stream end or error.
Agents consume structured MCP results, not CLI exit-code guesses.

## 3. Records passed across the boundary

These shapes augment the domain records; prose in this section constrains
fields as much as the field lists do.

```text
TaskDispatch {
  id, workId, taskId, taskRevision, attemptId, invocationId,
  kind: ProduceResult | EvaluateGate | ReviewResult,
  role: Contribution | Integration | Check | Review,
  subjectResultId?, evaluationRound?, evaluationUnitId?, gateDefinitionId?,
  evidenceManifestDigest?,
  integrationBase?: {headGeneration: nonnegative integer, parentResultId?},
  objective, scope: string[], exclusions: string[],
  inputs: InputRef[], inputManifestDigest, contextSnapshotId, workspaceId,
  outputs: {slot, kind: File | Tree | GitCommit | Report | Code | Evidence, required: boolean}[],
  criteria: {id, description, evidenceRule?: string, requiredEvidence: string[]}[],
  gateDefinitions: GateDefinition[], reviewPolicy: ReviewPolicy,
  effectiveGrantId, contractDigest,
  rework?: ReworkInstruction, reportingContractVersion: 1
}

GateDefinition {
  id, revision, criterionIds: string[],
  kind: Command | HumanCheckpoint,
  required: boolean,
  recipe?: {executable, args: string[], cwdRelative, environmentRef,
            timeoutSeconds: integer, evidenceParserId},
  decisionSchemaRef?
}

ReviewPolicy {
  revision, required: boolean, reviewerCapabilityId?,
  rule: AllRequiredGatesThenReview
}

TaskResultBody {
  dispatchId, taskRevision, inputManifestDigest,
  outputs: {slot, artifact: ArtifactRef}[],
  criterionEvidence: {criterionId, evidence: ArtifactRef[], claim}[],
  summary, knownGaps: string[], supersedesResultId?
}

GateSubmission {
  evaluationUnitId, dispatchId?, decisionApplicationId?,
  taskRevision, subjectResultId, evaluationRound,
  gateDefinitionId, gateDefinitionRevision, inputManifestDigest,
  outcome: Passed | Failed | Inconclusive,
  evidence: ArtifactRef[], explanation
}

ReviewSubmission {
  dispatchId, evaluationUnitId, taskRevision, subjectResultId, evaluationRound,
  evidenceManifestDigest, gateResultIds: Id[],
  recommendation: Accept | NeedsEvidence | ChangesRequested | Reject,
  findings: {criterionId, evidence: ArtifactRef[], explanation,
             requestedChange}[],
  preserveArtifacts: ArtifactRef[]
}

ReworkInstruction {
  resultId, reviewIds: Id[], gateResultIds: Id[],
  reason: NeedsEvidence | ContractViolation | Misunderstood | UserRevision,
  findings: {criterionId, evidence: ArtifactRef[], requestedChange}[],
  preserveArtifacts: ArtifactRef[],
  action: CollectEvidence | ReviseOutput | Replan
}
```

IDs, revisions and references have the types in section 2; unannotated
descriptive scalar fields are strings, IDs end in Id, and digest fields use the
ArtifactRef digest format. TaskDispatch.kind determines its terminal tool:
ProduceResult -> result.submit, EvaluateGate -> gate.submit, ReviewResult ->
review.submit. Check/review dispatches require subjectResultId/evaluationRound/evaluationUnitId;
EvaluateGate also requires gateDefinitionId. They are evaluation units attached
to the producing task, not new contributions requiring recursive review.
ReviewResult requires evidenceManifestDigest. Integration dispatches require
integrationBase; absent parentResultId means the recorded empty initial head,
not "whichever head exists later". Head generation changes only when a new
integration head is installed, not when an attempt acquires a writer reservation.
Manifest digests are SHA-256 of RFC 8785 canonical JSON. Input manifests sort
InputRefs by slot, then artifactId; contract digests cover the immutable task
contract fields, not mutable progress or timestamps.

A command GateSubmission requires dispatchId and forbids decisionApplicationId.
A human-gate submission is generated by the service's decision application,
requires decisionApplicationId and forbids dispatchId. Both name the precreated
evaluationUnitId. Human checkpoints occupy a recorded waiting evaluation unit,
not a runtime/worker concurrency slot or an imaginary ACP invocation.

A command gate requires recipe and forbids decisionSchemaRef; a human gate
requires decisionSchemaRef and forbids recipe. Environment and parser refs
resolve to registered local definitions before plan application. Command
timeout produces Inconclusive with captured logs, not a fabricated test failure.
Evidence parsers map the declared command outcome to Passed/Failed/Inconclusive.
The baseline registered parser `process-exit-v1` records the actual executable,
arguments, cwd, input digest, captured stdout/stderr artifacts and exit code:
0 is Passed, nonzero is Failed, and start failure/timeout/unavailable outcome is
Inconclusive. It proves the declared command's outcome, not an invented coverage
claim. More specific criteria require a correspondingly declared parser/check.
Each required criterion must name at least one concrete check/evidence rule;
advisory model comments cannot stand in for required command/human checks.

ReviewPolicy.required requires a reviewer capability and a completed review
after required gates have passed. A reviewer can request correction with
criterion-bound findings; Accept cannot override a failed/missing gate.
No reviewer and no gates is permitted only for output contracts whose required
criteria are satisfied by the declared artifact/schema checks themselves,
such as delivering a requested readable report, not proving code behavior.

The service adds id, version, createdAt and disposition to result/review
records. Clients never submit their authoritative disposition or reviewer
identity. Timestamps are service UTC strings; they do not order commands.

## 4. Entry, planning and inspection operations

In operation tables, `empty` means no mutable ifMatch subject. S means service,
R means runtime adapter, C means coordinator, H means human client, W means
worker. Descriptive request fields are strings unless a type/reference is
shown. Every listed result is `ok` unless pending/needs_input is explicit.

| Method | Caller -> receiver; ifMatch | Params | Recorded result / next owner |
|---|---|---|---|
| console.open | H -> S; empty | consoleSessionId, conversationId, scope?: Global, projectId? | immutable console/conversation registration; Global has no bound project |
| console.configure | H or trusted S -> S; current policy on replacement | capabilityId, workerCapabilityId, checkCapabilityId, approvedModelDestination, limits | independently approved GlobalConversationPolicy; no Project or model invocation created |
| console.get | H -> S; empty | empty object | current global assistant policy or explicit assistant-unavailable failure |
| project.get | authorized client -> S; empty | projectId | project version, approved context, policy revision, configured capability IDs and finite allowance defaults |
| conversation.submit | H -> S; empty | conversationId, clientMessageId, text, attachments: ArtifactRef[], declaredIntent?, context `{consoleSessionId, contextVersion, scope?: Global, selectedWorkId?, projectId?}` | recorded message/turn; Global accepts project-free intake and selection hints; a canonical WorkExecutor binding queues input for that work's executor |
| conversation.resolve_intents | C -> S; empty, bound turn | turnId, messageId, intents: `{kind, workId?, draftGoal?, referencedMessageIds: Id[], changeProposalId?}[]` | intentIds, affectedWorkIds; display interpretation; no implicit start or scope change |
| conversation.request_input | C -> S; empty, bound intake | turnId, conversationId, messageId, question, responseSchema | needs_input with Intake reference; no WorkItem needed |
| conversation.answer_input | H -> S; IntakeRequest | requestId, action: Answer or Cancel, value? | recorded answer/cancellation; driver resolves the original intake with captured targets |
| conversation.resolve_input | global C -> S; IntakeRequest | requestId, messageId, value | schema-valid interpretation of a new captured human reply to this conversation's open intake question; records provenance, not execution or human permission approval |
| conversation.propose_action | global C -> S; empty | conversationId, messageId, method, params, ifMatch: EntityRef[], summary | immutable HumanActionProposal with server-issued commandId, precise request and authoritative preview; no execution |
| work.create_draft | C or H -> S; empty | projectId, goal, scope: string[], exclusions: string[], criteria: `{id, description, evidenceRule}[]`, context: ArtifactRef[], delivery `{kind: LocalCode or Report, destinationWorkspaceId?}`, sourceMessageIds: Id[] | workId, specRevision, version; Draft view and reviewable brief |
| grant.preview | H or C -> S; Work | workId, specRevision, policyRevision | grantProposalId, version, exact capabilities/scopes and finite allowances; no authority issued |
| work.start | H -> S; Work | workId, specRevision, projectPolicyRevision, grantProposalId | work version and Active/Advance; scheduler uses applied plan or driver plans within approved resource allowance |
| work.propose_change | C or H -> S; Work | workId, replacementSpec, affectedTaskIds: Id[], reason | proposalId, version, impact; no new authority |
| work.apply_change | H -> S; Work and ChangeProposal | proposalId, grantProposalId | new spec/work revision and settlement operationIds; affected dispatch held |
| plan.propose | C -> S; Work | workId, basedOnPlanRevision?, tasks: TaskContract[], edges: DependencyContract[], integrationTaskKey, reason | proposalId, impact, generated task IDs, contract revisions and validation findings |
| plan.apply | C -> S; Work and PlanProposal | proposalId | planRevision, carriedAttemptIds, settlement operationIds; scheduler reevaluates readiness |
| task.get | authorized client -> S; empty | taskId | TaskView including dispatch, current attempt, context requests, result and evaluations |
| task.list | authorized client -> S; empty | workId, afterId?, limit: integer 1..100 | items: TaskView[], nextAfterId? |
| result.get | authorized client -> S; empty | resultId | body, disposition, evaluationRound, gates, reviews, rework and supersession refs |
| artifact.get | authorized client -> S; empty | artifactId | ArtifactRef, kind, manifest, availability and service-issued local read locator |
| artifact.read | authorized client -> S; empty | artifactId, relativePath?, offset?, limit? | verified bounded content or manifest-entry page; explicit encoding/support status and continuation offset |
| progress.get | authorized client -> S; empty | reportId | recorded activity, findings, artifact references and coordination request |
| workspace.get | bound runtime or authorized human -> S; empty | workspaceId | version, local root, repository identity, current head generation and managed input/output scopes |
| work.open | H or trusted S -> S; empty | workId | idempotent durable task-chat binding; `{workView, conversation, context, continuation}`; opens a view without starting execution |
| work.continue | H or trusted S -> S; Work | workId, restartSession?: boolean | attach to live execution or request owned continuation; recovery may be pending; explicit reconstruction only with restartSession:true |
| work.claim_executor | H or trusted S -> S; Work | workId, restartSession?: boolean | freeze legacy scheduling, settle writers and adopt a verified actual-worker session; returns the opening envelope even while settlement or reconstruction consent is pending |
| work.request_input | bound executor -> S; empty | workId, question, responseSchema | needs_input with an Intake request; human resolution queues an answer to the same executor |
| work.execution_check | bound runtime -> S; empty, read-only | workId | allowed:true, invocationId, turnId; expired, paused, cancelled, ungranted or incorrectly bound execution returns an error |
| work.get | authorized client -> S; empty | workId | WorkView with spec, plan, current candidate, obligations, versions and continuation status |
| work.list | authorized client -> S; empty | afterId?, limit: integer 1..100 | items: WorkView[], nextAfterId? |
| operation.get | authorized client -> S; empty | operationId | OperationView with status, subject refs, completed steps, error and next action |

`conversation.resolve_intents.kind` is ImmediateQuestion, WorkDiscussion,
NewWork, ContextAddition or ChangeProposal. NewWork requires draftGoal;
existing-work intents require workId; ChangeProposal requires its proposal
reference. Context additions use approved ArtifactRefs. Ambiguous interpretation
returns a recorded clarification rather than guessing a different target.
Immediate questions finish with a conversation answer and create no WorkItem.
declaredIntent, when supplied by an explicit command, uses those same intent
names and constrains interpretation. `/work new` uses NewWork intake to prepare
the brief, not work.create_draft with invented criteria. `/task show` maps to
task.get; `/result show` to result.get; `/review accept` and `/review revise`
map to delivery.accept and delivery.request_changes. `/work pause`, resume and
cancel map to work.control with Hold, Resume and Cancel. `/work events` maps
to events.subscribe with a Work scope. Aliases do not create additional wire
methods or bypass missing-argument collection.

### 4.1 Task conversations and continuation

`work.open` is a versioned-command mutation, not an execution approval. It
idempotently creates or returns the selected work's durable console/conversation
binding. `context` contains `consoleSessionId`, `conversationId`,
`contextVersion`, `projectId` and `selectedWorkId`. For subsequent non-global
`conversation.submit`, place `conversationId` at the request's top level and
the remaining fields in `context`; do not copy it into that nested context.
Reopening the same work, including
from another task tab, returns the same persisted conversation rather than
creating another work or a global intake. Clients retain their own unsent
drafts. The returned conversation includes the work's recorded main dialogue;
internal worker transcripts are not automatically promoted to human chat.

New work defaults to `executionMode: WorkExecutor`. Messages for its bound
conversation route directly to its executing agent session. Explicit
`LegacyTasks` retains the older coordinator/task pipeline; a historical record
without an execution mode instead requires explicit `work.claim_executor` and
rejects ordinary submission/continuation with `WORK_EXECUTOR_CLAIM_REQUIRED`.
Opening or viewing the task does not dispatch a model. A new message or an
explicit continuation may cause a bounded turn, but cannot retarget another
work or bypass an existing execution grant. Background responses retain their
work and conversation identity across navigation.

The `continuation` projection on WorkView and work.open reports observed
`state`: `Running`, `WaitingForInput`, `Paused`, `NeedsRecovery`, `Ready`,
`Completed`, `Cancelled` or `Unavailable`, with a reason and activity timestamp
when available. Expired or unconfirmed invocations must not be represented as
fresh running work solely because their durable state still says Running.
These display states do not replace authoritative work/attempt lifecycles.
`executionMode` projects `WorkExecutor`, `ClaimingExecutor` or `LegacyTasks`.
`activity: Idle` accompanies a retained idle executor's Ready state.
`pendingInputCount` counts queued `WorkExecutionTurn` records, excluding the
current turn. Inputs are FIFO with a 256-item queue limit.
`canClaimExecutor` is a migration-action hint, not proof of usable authority or
session history. `canRestartSession` requires settled ownership and an available,
approved provider/workspace as well as a genuinely unavailable saved session.
`activeResponses` contains only live primary response `{messageId, deadlineUtc}`
references. A task client also checks the deadline locally; historical
Streaming messages and unrelated worker activity cannot imply live chat.
`recoveryFailure`, when present, carries the current failed stop/release
operation's failure and `operationId`, without changing execution authority.
Clients keep that failure visible outside scrollable conversation history.
Dispatch that has not yet produced a Started observation reports `Ready` with
a startup-pending reason, not `Running`.

`work.continue` requires the exact current Work reference and a stable commandId.
Repeated commands attach to the same recorded outcome. Live execution is not
duplicated; stale or unsettled execution must be reconciled and its writer
settled before replacement. Existing drafts still require work.start approval,
and terminal work is not implicitly reopened. Global conversation may propose
work.continue or work.claim_executor through the same frozen human-action path
rather than confusing them with scheduler-only Resume.
Both mutations return the open-view envelope. An accepted recovery request can
return `ok` with `NeedsRecovery`; that receipt confirms the request, not settled
execution or a successfully resumed session. A missing provider PID alone is
not proof that its writer tree has ended. Unverifiable legacy ownership remains
in recovery rather than silently admitting another writer.

The primary executor session is associated with its work, provider and
working directory. Resuming loads that provider session with a fresh
invocation-scoped work MCP binding and current authority, not stale bearer
credentials. Missing resume support or unavailable history is explicit.
`restartSession:true` is a separate human choice to reconstruct context in a
new session, not a fallback taken automatically after a failed load. Legacy worker attempts retain their independent dispatch and settlement contracts.

An executor retains its ACP connection, owned process tree, workspace writer
reservation and concurrency slot while idle. Normal reply completion leaves
Work Active; it does not settle the process tree or kill a development server.
Each admitted prompt consumes an approved `executionAttempts` allowance and
uses the execution-seconds deadline. Queued input has not yet consumed that
allowance. Continue on a live running/idle executor only attaches, without
admitting another prompt. Hold/Cancel stop the owned tree and fence admission;
authority shutdown closes admission and waits up to 15 seconds for proven
settlement, reporting failure if that cannot be established.

`work.claim_executor` requires the exact Work guard, a stable commandId, an
effective matching grant, an approved worker capability and an available
non-human-owned workspace. It freezes legacy scheduling before reconciling its
old writers. `work.executorClaim.status` is `Settling`, `NeedsConsent`,
`Held`, `Cancelled` or `Claimed`; `executionMode` is `ClaimingExecutor` until adoption/reconstruction.
Missing verified actual-worker history records `EXECUTOR_SESSION_UNAVAILABLE`
and requires explicit `restartSession:true`. Coordinator history never qualifies
as executor provenance. Adoption preserves old conversations/artifacts and
queues a continuation prompt; `Claimed` is not proof that ACP loaded it.
Only the resumed Started observation with `sessionLoaded:true` and the same
provider session identity proves loading, and a real follow-up effect is needed
to prove execution. Intervening Hold/Cancel must prevent delayed claim settlement
from admitting that prompt.
Hold marks the pending claim Held and revokes prior reconstruction consent;
fresh claim/Continue explicitly resumes it, while scheduler-only Resume does not.
Cancel marks it Cancelled. Settling exposes neither a fresh claim nor restart;
Continue only reconciles that existing claim. NeedsConsent exposes restart only
after its grant, provider, workspace and settled ownership are checked.

`work.request_input` binds an `IntakeRequest` to the current Work executor turn.
Its request fields are `workId`, `question` and `responseSchema`; human resolution
uses existing `conversation.answer_input` with the exact IntakeRequest guard,
`requestId`, `action: Answer` plus `value`, or `action: Cancel`. Resolution queues
input to that same executor; answers also become human chat messages.
Runtime calls the read-only `work.execution_check` before native permission
grants and ACP writes. Its success data contains `allowed:true`, `invocationId`
and `turnId`; invalid ownership, grant, expiry, Hold/Cancel or an open question
returns an error instead of successful-looking `allowed:false`.

This bounded implementation deliberately does not add executor-specific formal
delivery acceptance. WorkExecutor delivery reports `EXECUTOR_DELIVERY_UNAVAILABLE`;
ordinary replies never manufacture TaskResults, successful gates, human
acceptance or Completed work. Explicit LegacyTasks retains the existing
delivery/evidence contracts.

A dedicated `wta ui --work <id>` tab is another presentation of this service
binding. It does not independently adopt the workspace or instantiate a
second helper/provider writer.

For LegacyTasks, after service disconnection, a sole remaining coordinator invocation with no
worker dispatch may recover its conversation without asserting physical
process-tree settlement. The internal `runtime.recover_coordinator` effect
verifies the immutable original invocation plus current approved provider
configuration against its saved digest, pins the original provider session and
directory, and records a separate recovered-session association. The authority
then releases only the coordinator binding with
`releaseKind: CoordinatorAuthorityRevoked` and `processSettlement: Unknown`.
Old coordinator tools/reports are rejected; its incomplete reply is marked
Interrupted. Actual worker invocations must already be Released, and their
writer reservations and settlement evidence are never changed by this path.
The next invocation still uses ACP session/load and must report the same
provider session. A later Hold or Cancel prevents resuming execution, even if
session-association recovery completes afterward.

### Preference memory

Preference memory is an Agent Center facility, not part of per-tab WTA or its
session MCP. It uses `Preference` entities in the existing `work.db` records,
without a migration, new database or dependency. No vector database, sync or
history mining is introduced.

```text
Preference {
  id: Id, kind: Preference, version: Revision,
  key: string, scope: User | Project, projectId?: Id,
  content: string, status: Active | Forgotten,
  sourceMessageId?: Id, createdAt: string, updatedAt: string
}
```

Project scope requires projectId; User scope has no projectId. Keys are unique
within their scope (and project for Project scope), lowercase ASCII
dotted/dashed keys of at most 80 bytes. Content is at most 512 Unicode
characters. Each scope is capped at 512 records, including tombstones.
Timestamps are service UTC strings. `sourceMessageId` references
human input, not a quote copied into the preference.

| Method (MCP tool) | Caller -> receiver; ifMatch | Params | Result / semantics |
|---|---|---|---|
| memory.list (`memory_list`) | authorized H or C -> S; empty | projectId?, afterId?, limit?: integer 1..100 | `{items: Preference[], nextAfterId?}`; omitted projectId means user scope only; supplied authorized projectId includes user plus that project |
| memory.store (`memory_store`) | authorized H or conversational intake C -> S; empty for new scoped key, exact Preference for replacement | key, scope: User or Project, projectId?, content, sourceMessageId? | upsert the scoped key; replacement includes a Forgotten tombstone and requires its current version |
| memory.forget (`memory_forget`) | authorized H or conversational intake C -> S; exact Preference | preferenceId, sourceMessageId? | set Forgotten and clear live content; retain the versioned tombstone |

Lists include content-free Forgotten tombstones for `ifMatch` conflict handling
and restoration. The ordinary mutation envelope and command-id replay rules
apply. Both mutation MCP schemas require sourceMessageId even though direct
human protocol callers may omit it. The reference must match the latest human
message captured in that invocation and
an `IntakeMessage` trigger. The source's scope is authorized separately from the
target preference's scope. A real, authorized source proves provenance, not the
semantic truth of the preference. Agents read current entries first and reuse
keys/update existing entries instead of creating synonyms.

Global and project-intake conversational coordinators receive all three tools.
Work coordination with no new human intake receives only `memory_list`.
Workers cannot read or write full preference memory; a coordinator transfers
only relevant constraints through an approved task contract, never broad
personal dumps or a hidden change to scope, permissions or criteria.

Each coordinator snapshot includes
`preferences: {items: Preference[], truncated: boolean, forgetVersion}`.
The serialized items are bounded to at most 8192 bytes. Global snapshots inject
user preferences plus an authorized current-project hint's preferences, if
present; work snapshots inject user preferences plus that work's project.
Use `memory_list` pagination when truncated rather than assuming absence.
Snapshot selection and a project hint do not authorize a wider memory scope.
Explicit current instructions take precedence; project preferences override
user preferences only for that project. Recalled content is data, never
authorization or higher-priority instructions, and cannot change task contracts.

The conversational coordinator automatically learns only durable, non-sensitive
interaction/workflow preferences grounded in current human input. There is no
extra extraction model or background scan. Never persist credentials, personal,
sensitive or third-party information, task progress, transient requests, or
agent/tool/web output as user preferences. Do not infer cross-project scope
from a task; ambiguous scope requires asking or skipping, never broadening.
These extraction semantics are a behavioral policy, not a hard classifier
guarantee. Natural-language corrections and forgetting use the same tools,
without new commands or UI. Claim remembered/forgotten only after a successful
tool response and expose failures.

Forgetting increments the global `forgetVersion` fence, invalidating
`memory.store` from already-captured invocations, including the forgetting
invocation, but not subsequent `memory.forget` calls. Each forget still requires
its exact Preference version and, for agents, the latest captured source checks.
Complete all requested deletions, then finish the turn; do not store again
until fresh human intake. Clearing live content is not
full erasure: old snapshots, chat, command receipts and backups may retain
historical content. A later fresh human request can restore the forgotten key
using its current Preference version; neither the original source message nor
the forgetting source message may be replayed to restore it.

### Global conversation and human-action proposals

Global conversation handles new goals and cross-work discussion; it is not the
conversation used when an existing task is opened. `context.scope: Global`
selects this explicit mode; omitting scope uses the immutable project/work-bound
registration. Global console registration omits projectId.
Message project/work hints may change without replacing the console/conversation
pair. A global registration cannot adopt another window's conversation or a
legacy project-bound identity.

Global coordination has an independently approved assistant policy and finite
allowance, not a synthetic Project. A runtime with one explicitly approved ACP
adapter may bootstrap that policy; multiple adapters require an explicit
conversation capability choice. This does not infer a new model destination,
start model work at launch, replace user-owned settings, or qualify automatic
adoption of Terminal's provider settings. Missing assistant approval is distinct
from missing execution context.

Each global invocation captures the project IDs whose policy permits the
approved assistant capability. Its bounded summaries explicitly mark truncation;
paginated reads remain access-filtered. Another window's conversation is not
project data. A global assistant may read authorized work facts and propose
briefs/changes, but cannot become a work execution coordinator or directly run
plan, worker, grant-approval, control or acceptance mutations.

`HumanActionProposal` contains id, version, status, conversationId, optional
workId, source messageId, explanation, frozen `{method, params, ifMatch,
commandId}` and an authoritative preview. Supported methods are explicitly
allowlisted; an explanation cannot replace actual permissions, destinations,
limits, scope, or fixed-candidate facts. The source is the latest captured human
message in the same conversation. A new proposal supersedes an older open
proposal for the same target, not another work's proposal.

Only explicit human confirmation submits the frozen request. The service checks
the proposal's open state and exact semantic envelope atomically with the normal
operation's target/version guards. Superseded or edited previews cannot execute.
Original committed receipts remain idempotent on exact retry; Submitted/pending
does not mean Completed. Delivery acceptance additionally retains the Console's
exact fixed inspection requirement.

Natural-language clarification can be interpreted through
`conversation.resolve_input`; its source message and
`answerSource: HumanMessageInterpretation` remain recorded. This only resolves
intake information, never a DecisionRequest, permission, work start, or delivery
approval. It does not queue a duplicate turn while the current actor is already
handling the reply.
An IntakeRequest contains id, version, conversationId, source message/turn,
question, responseSchema and `Open | Answered | Cancelled`. Answer requires
value matching that schema; Cancel forbids value. The Console displays which
question receives the answer. A different background question cannot intercept
the current message. Answered/cancelled intake emits IntakeAnswered/IntakeCancelled
and lets the driver complete or abandon the original resolved intent.
Conversation snapshots preserve their message fields and include
`intakeRequests`, the current records associated with that conversation.
Conversation subscriptions also carry requested/answered/cancelled intake
changes by the changed record's `conversationId`, including replay of events
committed before the client subscribed. A pre-work question must remain
discoverable without manufacturing a Work or widening to another conversation.

`replacementSpec` has the same brief fields as work.create_draft plus its
current spec revision. Grant proposals are service-created previews from the
registered project policy; approval binds that exact preview. The coordinator
cannot create an effective grant by constructing params.
The registered policy supplies execution-attempt, evaluation-attempt,
coordination-turn and per-attempt context-round allowances, plus finite execution
and coordination time budgets. grant.preview names the counted categories and
remaining amounts explicitly. A request to increase them is a new preview and
human approval, never an implicit reset during rework.

```text
TaskContract {
  clientKey: string, existingTaskId?: Id,
  role: Contribution | Integration,
  objective: string, scope: string[], exclusions: string[],
  inputSlots: {slot: string, source: InputSource}[],
  outputs: {slot: string, kind: File | Tree | GitCommit | Report | Code | Evidence, required: boolean}[],
  criteria: {id: string, description: string, evidenceRule?: string, requiredEvidence: string[]}[],
  gateDefinitions: GateDefinition[], reviewPolicy: ReviewPolicy,
  capabilityId: Id, requiredForDelivery: boolean,
  resourceRequirements: {workspaceId: Id, mode: ReadOnly | ExclusiveWrite}
}
DependencyContract {
  sourceTaskKey: string, outputSlot: string, consumerTaskKey: string,
  condition: ArtifactAvailable | GatePassed, requiredGateIds: string[]
}
```

InputSource is `{kind: "Artifact", artifact: ArtifactRef}` or
`{kind: "Dependency", sourceTaskKey: string, outputSlot: string}`.
Client task keys and criterion/output/gate identifiers are contract-local
strings, not globally scoped record UUIDs. In particular gateDefinitionId
references a key within its exact task revision; GateResult.id is a service UUID.
New proposed gate definitions use revision 1; changed definitions must advance
the recorded revision and unchanged ones retain it. Plan validation normalizes
task IDs and rejects unknown/duplicate local keys before plan.apply.

Output kinds are case-sensitive and shared between the advertised schema and
admission. An unknown kind returns `INVALID_ARGUMENT` with the indexed
`params.tasks[i].outputs[j].kind` field error and allowed values. It is not an
unsupported method. A corrected proposal uses a new commandId; rejected
proposals do not create tasks or advance the work version. Code, Tree and
GitCommit outputs require a concrete required Command check.

Integration criteria retain the exact approved ID and description.
For a prose work criterion, task `evidenceRule` copies the approved
`work.spec.criteria[].evidenceRule` verbatim, while `requiredEvidence` binds it
to one or more concrete required gate IDs or `artifact:<required-slot>`.
The copied rule is part of the immutable task contract and dispatch, not a
replacement for executable checks or captured evidence. A changed rule,
missing mapping, optional-only evidence, or unknown binding is rejected.
`command:<gate>`, `artifact:<slot>`, and legacy bare ASCII references (no
whitespace) retain their exact required binding even when `evidenceRule` is
present. Existing reference-based contracts may omit the new field.
The service enforces those bindings and content integrity; it does not claim
that passing a command automatically proves the semantic adequacy of an
arbitrary natural-language criterion.

The integrationTaskKey names one Integration task with dependencies on all
required contributions and combined-result checks mapped to the work criteria.
Report-only work may name its report-producing task as the integration task.
No self-edge or hidden required task omission is valid. Proposed graph changes
must remain within the approved spec/grant; otherwise plan.apply returns
needs_input linked to the necessary change proposal.

Task revisions remain unchanged only when inputs, outputs, criteria, review
policy and authority are unchanged. The service computes carry-forward, never
trusts a model-supplied compatibility boolean. Replanning does not mutate
pinned inputs of an active attempt.

Views are read projections, not writable status objects. TaskView includes
`task`, `dispatch?`, `attempt?`, `contextRequests[]`, `currentResult?`,
`evaluations[]`, `obligations[]`; WorkView includes `work`, `spec`, `plan?`,
`taskSummaries[]`, `candidate?`, `obligations[]`; OperationView includes
`operation`, `subjects[]`, `failure?`, `nextAction?`. Record shapes are those
defined here and in the linked domain inventory. List order is ascending ID
within the current authorized view; event subscriptions supply live changes.

Human-facing work cards, selectors and question forms consume these views and
invoke the existing methods; they do not introduce parallel approval or
acceptance semantics. A readable label is not an object identity. The client
retains the exact selected IDs, expected versions and command identity while
showing goal, project, question, evidence and impact. Response schemas retain
their JSON value types even when input is collected through individual fields.
Unsupported form schemas remain explicit limitations rather than fabricated
answers. Raw requests and IDs are diagnostic details, not required user input.
Client rendering never removes identity or provenance from the wire contract.

## 5. Admission, runtime dispatch and terminal records

### 5.1 Admission and runtime operations

Admission atomically reserves the applicable resources, creates the Attempt
and TaskDispatch, and records an Invocation intent. Only then does S invoke R.
A TaskDispatch is not an unsolicited MCP notification.

```text
Invocation {
  id, subject: {kind: Task or Coordination, id},
  runtimeId, capabilityId, sessionReuseRef?,
  dispatch?: TaskDispatch, coordinationInput?: CoordinationInput,
  replyMessageId?, transcriptMessageId?, bindingGeneration,
  limits: {deadlineUtc, remainingExecutionAllowance, remainingContextRounds},
  availableToolNames: string[]
}
```

Exactly one of dispatch/coordinationInput is present. Task subject ID is an
attempt ID; Coordination subject ID is a turn ID. The service allocates reply
message identity for coordinator output before dispatch. The runtime builds
the provider prompt from this object and binds work tools out of band.
Task invocations receive transcriptMessageId; coordination receives replyMessageId.
The runtime may report deltas only for the message allocated to that invocation.
Its part/chunk identities remain stable across replay and distinct across turns.

Before Starting, R resolves workspace.get and artifact.get for the pinned
references, materializes inputs and checks their digests, and sets the actual
cwd from that workspace. Failed materialization reports EXECUTION_FAILED before
any agent work; it is never replaced with an empty input directory. MCP forwarding
uses the invocation/session binding, never the last active provider session or
physical tab. The bridge supplies fixed dispatch identity and maps dotted
methods to underscore tool names without changing response semantics.

| Method | Caller -> receiver; ifMatch | Params | Result / next owner |
|---|---|---|---|
| runtime.register | R -> S; empty | runtimeInstanceId, protocolVersions: integer[], capabilities: `{id, kinds: (ProduceResult or EvaluateGate or ReviewResult or Coordinate)[], supportsContinuation: boolean, supportsScopedStop: boolean}[]` | runtimeId, runtimeBinding; S can send runtime requests |
| runtime.invoke | S -> R; empty | invocation: Invocation | invocationId, disposition: Recorded or AlreadyRecorded; R starts it once |
| runtime.continue | S -> R; empty | invocationId, continuation: Continuation | continuationId, disposition: Recorded or AlreadyRecorded; R queues at an idle boundary |
| runtime.stop | S -> R; empty | operationId, invocationId, reason: Hold or Cancel or ContractDeclined or SettleCompletion or Deadline | pending with that service-issued operationId; R reports settlement or inability to establish it |
| runtime.release | S -> R; empty | invocationId, terminalDisposition: Succeeded or Failed or Cancelled | released: true, sessionReuseRef?; remove live invocation/tool binding after proven settlement |
| runtime.probe | S -> R; empty | invocationId | state: NotStarted or Running or Idle or Settling or Ended or Unknown, lastSequence, terminalRecordIds: Id[], terminalObservation? |
| runtime.report | R -> S; empty | observationId, invocationId, bindingGeneration, sequence: integer, kind, data | recorded observationId and lastSequence; S applies the reducer below |
| task.acknowledge | W -> S; empty, bound invocation | dispatchId, taskRevision, continuationId?, disposition: Accepted or Declined, reason? | acknowledgmentId; reason required for Declined |

Runtime keeps a command/invocation ledger. Repeating runtime.invoke for the same
ID returns its receipt, never a second prompt. On an ambiguous transport result,
S probes that ID; NotStarted permits dispatch of the recorded intent, Unknown
requires explicit repair. This is event/effect reconciliation, not model polling.

Runtime reports Started and waits for its service acknowledgment before sending
the first ACP prompt. Started.data includes adapterKind and executionIdentity;
ACP additionally requires providerSessionId, while native command execution
forbids that field rather than inventing a provider session.
The worker's first work action is task.acknowledge. Until accepted, only
acknowledgment/inspection is allowed through work tools. Decline triggers scoped
settlement and coordination; it is not a successful empty result.
An ACP provider may request permission before sending that acknowledgement.
The host must permit the exact invocation-bound acknowledgement with
`allow_once` without requiring prior acknowledgement; permission itself is
not a service receipt and must not advance the task's acknowledged state.
Match the session, bound tool, closed arguments, dispatch revision and current
continuation. Provider title text alone is insufficient. Unrelated execution
or writes remain denied before acknowledgement, and cancelled/released
invocations cannot obtain permission.
After settlement a declined dispatch ends Failed with endReason ContractDeclined,
not MissingSubmission. A failure before Started may report TurnEnded/Error
against the recorded invocation without inventing a provider session.
A native command-check adapter records acknowledgment for its bound check
before executing the recipe. A capability cannot register v1 agent execution
without the required work tools and terminal-record behavior. Tasks requiring
context continuation are admitted only to a capability that supports it.

### 5.2 Runtime observation kinds

| kind | data | Service interpretation |
|---|---|---|
| Started | adapterKind: ACP or Command, executionIdentity, providerSessionId? | Correlate invocation; Attempt becomes Running, still awaiting contract acknowledgment |
| TextDelta | messageId, partId, chunkIndex: integer, text | Persist ordered conversation/transcript chunk; no task progress or verdict inferred |
| ToolActivity | callId, toolName, phase: Started or Ended, summary | Execution observation only |
| TurnEnded | turnNumber: integer, finish: Normal or Cancelled or Error, quiescent: boolean, errorText? | Classify using section 5.3, not the last natural-language sentence |
| Settled | quiescent: true, executionIdentity, completedOperationIds: Id[] | Confirm tracked writers ended and corresponding stop operations; final binding/slot release uses runtime.release |
| Disconnected | reason | Mark health unknown; no automatic terminal success/failure or replacement writer |

Sequence starts at 1 per invocation and strictly increases. An exact duplicate
observationId/content is acknowledged without reapplying; conflicting content
is rejected. A sequence gap pauses interpretation of later facts until replay
or probe establishes the missing disposition. Old binding observations may
settle a known old execution but cannot advance replacement work.
The adapter journals the exact pending observation and command identity before
sending it. If cancellation or transport loss interrupts the acknowledgement,
the next report replays that same observation before advancing the sequence,
including before TurnEnded or Settled. A committed text chunk is never replaced
with a new observation at the same sequence, and replay does not duplicate chat
text. Definitively rejected observations do not consume a sequence; an unknown
delivery outcome retains the pending observation for reconciliation.
Each transaction persists only new or version-changed records, together with
its events/effect receipts. Streaming a text chunk must not rewrite unrelated
historical records; rollback and command replay retain their atomic behavior.
Probe state Ended requires terminalObservation with the TurnEnded data and
its original sequence; other states may include the latest such observation.
A probe observation is explicitly reconciled as a snapshot, not presented as
replay of nonexistent sequence entries. Insufficient evidence remains Unknown.

### 5.3 Ending a provider turn

After a Normal TurnEnded, S follows this precedence:

1. If quiescent is false, record Settling and request scoped SettleCompletion.
   Defer terminal classification and resource release until Settled; inability
   to establish settlement becomes an explicit repair obligation.
2. If the attempt's waitingRequestId points to an unresolved request or an
   unacknowledged continuation, keep it waiting, even if its answer arrived
   before TurnEnded. Applied/superseded/cancelled historical requests do not
   satisfy this condition. Dispatch the continuation only after R is idle.
3. Otherwise, if the required terminal record was committed, end the attempt
   as Succeeded. Result/gate/review acceptance remains a separate decision.
4. Otherwise, end it as Failed with PROTOCOL_INCOMPLETE and a
   MissingSubmission or ContractUnacknowledged obligation. A producing task
   queues coordination; a check/review unit reports to its evaluation controller;
   a coordinator failure is handled by its driver. Do not schedule duplicate
   producer rework directly from an evaluation unit's failure.

Error/Cancelled endings retain committed artifacts and records, settle
resources, and end Failed/Cancelled. They do not roll back an already recorded
submission. The policy controller may evaluate such a fixed result once the
producer is settled; it must not infer completeness from a partial body.

After terminal classification, S calls runtime.release as a tracked effect.
R removes the live invocation/MCP binding and returns an optional reusable
provider-session reference; shared provider processes remain governed by the
pool, not terminated because one attempt ended. Execution-slot release and
completion-relevant resource cleanup finish on this receipt. Unknown release
leaves an explicit operation/repair, not a live scope silently assigned to a
replacement. Waiting attempts keep their reservations and are not released.

The invocation can contain multiple ACP turns while an Attempt waits for
context. A terminal Attempt cannot be resumed. Any subsequent production
requires a new Attempt and Invocation, even if a provider session is reused.

ProduceResult requires result.submit; EvaluateGate requires gate.submit;
ReviewResult requires review.submit; a coordinator invocation requires
coordination.finish. A command-check adapter emits the equivalent gate record
and runtime observations itself. It does not require an agent to narrate them.

## 6. Progress, questions and continuation

| Method | Caller -> receiver; ifMatch | Params | Result / next owner |
|---|---|---|---|
| task.report_progress | W -> S; empty, bound dispatch | dispatchId, taskRevision, activity, findings: string[], artifacts: ArtifactRef[], nextStep, blockerRequestId?, coordinationRequest? `{reason, affectedTaskIds: Id[], proposedAction}` | reportId, eventId; explicit request also queues coordination |
| task.request_context | W -> S; empty, bound dispatch | dispatchId, taskRevision, question, target `{kind: Coordinator or Task, taskId?}`, inputs: ArtifactRef[], blocking: boolean | needs_input with Context reference; pending question is recorded before reply |
| task.answer_context | assigned responder -> S; ContextRequest | requestId, answer, evidence: ArtifactRef[], compatibility: ExistingInputs or RequiresRevision | answerId, delivery: Queued or RecordedForFuture or RequiresRevision |
| decision.request | C or service gate controller -> S; target Work/TaskResult/proposal | workId, purpose: ScopeChange or GrantChange or GateApproval or TaskInput, subject: EntityRef, application: DecisionTarget, question, options: `{id, label, impact}[]`, responseSchema, contextRequestId? | decisionId, version; inbox waits for human |
| decision.answer | H -> S; DecisionRequest | decisionId, value | answerId, applicationId, state: Recorded; apply according to purpose |

Progress blockers reference an actual request/obligation. Merely writing
"blocked" does not create a waiting state with no wake source. Routine findings
do not start a coordinator turn. An explicit coordinationRequest does, with
the worker's reason and the current work snapshot.

For blocking Context requests, S marks WaitingForContext; the tool returns
needs_input, instructing the worker to yield its ACP turn rather than poll.
An internal request is not presented as a human question unless escalated into
a linked DecisionRequest. Nonblocking answers are recorded for normal future
context access; they never inject a concurrent prompt or resurrect an ended
attempt.
Target Task is a preferred source of evidence/knowledge, not a promise of an
always-live peer mailbox. The coordinator remains responsible for resolution.
If that participant has no legal continuation point, use its recorded output
or plan a bounded clarification task; never leave the requester waiting for an
ended session. A clarification requiring new inputs follows the revision path.

```text
Continuation {
  id, requestId, answerId, dispatchId, taskRevision, bindingGeneration,
  answer, evidence: ArtifactRef[], compatibleInputManifestDigest
}
```

S creates Continuation only for ExistingInputs after checking the original
task/scope. R waits for idle, invokes the recorded continuation once, and tells
the worker to acknowledge continuationId with task.acknowledge before work.
The answer arriving is Answered, runtime recording is Queued, and worker
acknowledgment makes it Applied and returns the Attempt to Running. A declined
continuation settles the attempt and queues coordination. No new inputs are
silently appended to its original manifest. Setting a blocking request records waitingRequestId on the
attempt; a successful continuation acknowledgment clears it atomically with
Applied/Running. An old applied question cannot keep a later completed turn
in WaitingForContext.

RequiresRevision supersedes the pending continuation and queues a change/plan
proposal; affected execution settles before a replacement uses new inputs.
If a continuation arrives after the Attempt became terminal, return BAD_STATE
and keep the answer as context for a replacement, not a callback to another task.

TaskInput decisions use this same continuation route. GateApproval applies
the human answer to the exact gate/result manifest and emits GateEvaluated.
ScopeChange/GrantChange applies the approved proposal and reports settlement
separately. DecisionRequest becomes Resolved only after its application, not
when the answer was merely saved.

DecisionTarget is a purpose-specific closed object: TaskInput has requestId;
ScopeChange/GrantChange have changeProposalId and grantProposalId; GateApproval
has evaluationUnitId, resultId, gateDefinitionId, evaluationRound and
inputManifestDigest. All are fixed when the question is recorded. Applying
an approved change invokes the same work.apply_change transition; applying a
human gate writes the matching internal gate.submit record. The answer cannot
select a different application target.

Context rounds are finite under Invocation.limits. Repeated unanswered or
circular questions become a coordination blocker; new investigation is an
explicit task/dependency, not an unbounded agent-to-agent chat loop.

## 7. Artifact capture, results and internal verdicts

### 7.1 Capture and submission

| Method | Caller -> receiver; ifMatch | Params | Result / next owner |
|---|---|---|---|
| artifact.capture | bound producer or H -> S; empty | workspaceId, sources: `{kind: File or Tree or GitCommit, relativePath?, commitId?}[]`, purpose: Output or Evidence or ManualInput | pending operationId; completion gives artifacts: ArtifactRef[] |
| result.submit | W -> S; empty, bound ProduceResult | TaskResultBody | resultId, version, disposition: Submitted, evaluationRound: 1 |
| gate.submit | registered check execution or human-gate application -> S; empty, bound evaluation unit | GateSubmission | gateResultId, resultId, evaluationRound |
| review.submit | declared reviewer -> S; empty, bound ReviewResult | ReviewSubmission | reviewId, resultId, evaluationRound |
| result.accept | policy controller or C request -> S; TaskResult | resultId, evaluationRound | disposition: Accepted and selected output bindings, only if all predicates pass |
| result.request_changes | policy controller or C request -> S; TaskResult | resultId, evaluationRound, instruction: ReworkInstruction | disposition: ChangesRequested, reworkId; driver queues bounded action |
| result.reject | policy controller or C request -> S; TaskResult | resultId, evaluationRound, instruction with action Replan | disposition: Rejected, reworkId; no automatic work cancellation |
| task.rework | C -> S; Task and TaskResult | taskId, resultId, reworkId, action: CollectEvidence or ReviseOutput or Replan, capabilityId? | rework operationId, scheduledUnitIds; scheduler owns actual admission |

File/Tree require relativePath and forbid commitId; GitCommit requires commitId
and forbids relativePath. Sources resolve inside the registered workspace/
repository. The capture manager stages content, computes the manifest digest,
publishes immutable content, then commits Artifact records. Failed capture
produces no Ready reference. Consumers cannot use a capture operationId as an
artifact. The MCP bridge waits on the capture operation's service event and
returns its terminal result, without asking the model to poll. If the bridge
loses the call, recover the same operation, not a guessed new artifact.

result.submit verifies acknowledged dispatch, current task contract, exact input
manifest and readable artifact references. Outputs must satisfy declared slot
shapes; every required criterion appears either with evidence or a declared
known gap. Schema/capture errors are correction errors in the current attempt,
not a recorded rejected result. A valid submission is immutable and receives
its ID before review. One producing attempt has at most one formal result.

Submitted is a receipt state. The controller waits for producer settlement,
then starts evaluationRound 1 and its declared check units. It does not hold
a producer concurrency slot while waiting for the user's final acceptance.
Check/review units have their own metered execution slots and terminal records.

### 7.2 Deterministic evaluation order

Each evaluation unit is keyed by `(resultId, evaluationRound, gate/reviewerId)`.
Its dispatch pins the submission digest and evidence inputs. Stale-round
submissions are historical observations only and cannot decide the current
result.

Evaluation inputs preserve the exact producing dispatch's InputRefs and append
the submitted outputs, each tagged with the current result's `sourceResultId`.
The evaluation manifest covers both sets; identical slot names do not replace
one another. The runtime uses `sourceResultId == subjectResultId` to identify
the submitted-output layer, not artifact order or the latest dependency state.
The evaluation dispatch retains the producing dispatch's output contracts.

For command checks, verified captures are copied into a fresh writable execution
directory. File captures use their captured basename; Tree/GitCommit captures
use their manifest member paths. Current output files supersede pinned input
files at the same path. Differing collisions within either layer, file/directory
conflicts, or the combined capture limits fail before process launch with a
diagnostic; no arbitrary artifact wins. A current Code/Tree/GitCommit output
whose manifest is a Tree supplies a complete snapshot instead of inheriting the
older input tree, preserving deletions. Report-only outputs can therefore check
their exact accepted code dependencies, or run directly against standalone
captured files. Independently pinned File inputs remain available when older
input snapshots are superseded. No mutable workspace files are implicitly added.

The controller evaluates a round in this order:

1. Wait for every declared required check's terminal record and settlement.
   A missing check terminal record becomes Inconclusive with its execution
   failure evidence; it does not remain Reviewing forever.
2. Any required Failed gate produces ChangesRequested/ContractViolation.
   Otherwise any required Inconclusive/missing evidence produces
   ChangesRequested/NeedsEvidence. Do not run a model reviewer just to override it.
3. If gates pass and a review is required, dispatch exactly the declared review
   against the completed gate evidence manifest. Wait for its terminal record
   and settlement. A missing review produces ChangesRequested/NeedsEvidence
   with a review-failure instruction, so task.rework can retry the review within
   allowance or expose its blocker; the result does not wait indefinitely.
4. Review NeedsEvidence/ChangesRequested/Reject produces the corresponding
   rework disposition with criterion-bound findings. Accept, or no required
   review, permits result.accept only when all required output/schema/criterion
   predicates are satisfied.
5. Accepted binds selected versions to output slots and reevaluates consumers.
   No natural-language response can replace these checks.

Review policy cannot be weakened during a failed round. A changed criterion
or policy follows explicit spec/plan revision. Optional findings are recorded
without masquerading as required failed checks. Model review remains advisory
about actual test execution, even when its findings legitimately request rework.

Preparation/runtime errors without a GateResult retain a bounded `errorText`,
phase and affected invocation/attempt/evaluation/result identities in the
evaluation failure, result/rework diagnostics and coordinator
`snapshot.evaluationFailures`. They do not fabricate a process exit or
GateResult. `RepairEvaluation` requests an explicit correction: ReviseOutput
can repair captures under the existing contract, or CollectEvidence can retry
the fixed result after a transient cause has actually been repaired. Identical
failures remain subject to repeated-rework limits, independently of fresh
invocation IDs. Human cancellation and normal turns missing a required
submission retain their distinct classifications.

### 7.3 Rework and task output currency

task.rework is the action that consumes a recorded changes request; merely
sending a notification does not schedule anything. CollectEvidence increments
evaluationRound and runs the necessary check/review units against the same
result. It does not repeat implementation or create a new code submission.
Unchanged passing evidence may carry forward only when its definition and
input/evidence manifests are identical.
The service creates a current-round evaluation-unit binding to that historical
record with carry-forward provenance. A stale worker submission cannot create
this binding or masquerade as a new check.

ReviseOutput creates a new producing Attempt after old execution settles,
with original contract, fixed result and ReworkInstruction as inputs. The next
result names supersedesResultId; its body is not an edit of the prior record.
Replan applies a compatible plan proposal or creates the necessary user
decision before dispatch.

All modes consume the existing approved allowances. Repeating the same
instruction with no new evidence/plan change or exhausting allowance returns
a recorded blocker and choices to coordination/human attention. It does not
automatically keep creating attempts. Each task has one current submission
and one set of currently valid accepted output bindings; old acceptance history
is retained. Explicit invalidation clears affected bindings and holds consumers.
Already-pinned consumers never silently switch to a newer result.

`ArtifactAvailable` can admit a checker on an unaccepted submission. A normal
`GatePassed` consumer requires its exact evidence; final human acceptance is
not a prerequisite for ordinary internal task handoff.

## 8. Coordinator driver and scheduling rules

```text
CoordinationInput {
  turnId, scope: {workId?, conversationId},
  replyMessageId, snapshotVersion,
  triggerEvents: {eventId, kind, subject: EntityRef}[],
  snapshot: WorkView or intake message/context view,
  remainingAllowance
}
```

The driver creates this input from committed state and calls runtime.invoke.
Only one coordination invocation is active per work or intake conversation.
Its commands use their own version checks; receiving a snapshot does not lock
the work or permit overwriting subsequent state.

New human input in a global conversation supersedes its active conversational
invocation through a scoped `runtime.stop` with reason `NewConversationInput`.
New input remains queued until that exact invocation settles and releases;
arrivals during settlement coalesce into the next snapshot. Existing messages,
questions, proposals and committed actions remain recorded. Only the obsolete
conversation turn is stopped: approved work, its coordinator/workers and other
conversations remain independent. A turn without a terminal finish ends as
`Superseded`, not a `PROTOCOL_INCOMPLETE` blocker; an already recorded valid
finish remains completed when this scoped stop settles normally.

Coordinators record dispatch/handoff receipts and finish rather than waiting
or polling for execution. The driver resumes them for actionable reported
state. The global collaborator answers simple questions and queries recorded
progress; substantial execution belongs to approved work tasks. ACP permission
for coordinators is limited to advertised invocation-bound work tools, not
provider-owned shell/subagent execution (this is not provider-tool isolation).

| Method | Caller -> receiver; ifMatch | Params | Result / next owner |
|---|---|---|---|
| coordination.finish | C -> S; empty, bound turn | turnId, outcome: Answered or ActionsRecorded or WaitingOnRecordedSubject or NoActionNeeded, commandIds: Id[], operationIds: Id[], messageId?, waitingSubject?: EntityRef, explanation | finishId; driver closes turn only after runtime settlement |

Answered requires the preallocated reply message with nonempty content recorded
from that turn. ActionsRecorded requires applied command receipts or
their explicitly pending operation refs; a plan written only in prose is not
an action. Waiting requires a real open request/dependency/operation, not
"wait and see". NoActionNeeded is valid only when the triggering condition has
already been resolved or superseded. Otherwise finish returns BAD_STATE so the
coordinator can correct its response; an ended turn without a valid finish
becomes PROTOCOL_INCOMPLETE and a bounded coordination blocker.

After finishing and settlement, the driver marks captured events handled,
reevaluates queued subjects against current state and runs another turn only
for still-actionable triggers. Arrival while busy never injects another prompt
into the active coordinator. Progress chunks and the coordinator's own finish
event do not trigger reflexive turns.

| Trigger | Next owner |
|---|---|
| Intake message, approved work needing a plan | Coordinator driver |
| ProgressReported without coordinationRequest | Read projections only |
| CoordinationRequested, declined task, missing terminal record | Coordinator driver with reason/record |
| ContextRequested | Recorded-fact responder or coordinator; task recipient only through its controlled continuation |
| IntakeAnswered/IntakeCancelled | Coordinator driver for the captured intake scope |
| ResultSubmitted plus producer settlement | Evaluation controller |
| Required checks complete | Evaluation controller, then reviewer if required |
| ChangesRequested/Rejected | Coordinator driver; action must be task.rework, plan change or recorded decision |
| Accepted contribution | Scheduler evaluates ready edges |
| Accepted integration result | Candidate controller prepares delivery |
| Applied user answer | Recorded target's continuation/controller |
| Accepted effect reaches terminal OperationChanged | Its recorded effect controller; failure queues coordination/repair for that subject |
| No-progress, exhausted allowance | Explicit blocker/decision; no automatic wake loop |

Planning has a separate bounded lane, one active turn per work, charged to the
project planning allowance. Producer/check/review attempts use execution slots.
Every project profile supplies finite execution, coordination and context-round
allowances; no implicit unlimited default is allowed. All workers waiting for
clarification must leave the planning lane able to answer. If an answer needs
new execution while all slots are held, the coordinator must revise/settle the
affected waiting attempt before admitting the investigation, or expose the
capacity choice. It cannot add an invisible dependency that can never run.

## 9. Integration, manual contribution and human delivery

### 9.1 Integration is a task, not an acceptance shortcut

The plan's Integration task consumes exact accepted contribution versions.
Its adapter combines them in the integration workspace, captures code/report
artifacts, and submits through result.submit. Its gate definitions execute
combined-result checks. A merge conflict is recorded as a gap/finding requiring
correction; separate branch test passes cannot bypass integration checks.

The integration dispatch also pins the prior integration-head identity.
Internal acceptance compares that parent and relevant current input bindings.
Mismatch produces a rebase/re-evaluation obligation rather than installing a
stale head. A valid integration acceptance installs the new immutable head
and permits candidate preparation.

Read-only LocalCode integration with report/evidence outputs may retain one
unambiguous Tree/GitCommit pinned from a still-current accepted upstream result.
The candidate includes that code reference and its `sourceResultId`/`sourceInput`
provenance, alongside the integration outputs. Aliases of the same snapshot and
source result do not create additional snapshots. Conflicting snapshots or
integration outputs that change retained code entries require an explicit new
code result; they cannot silently rewrite the destination. Final acceptance
revalidates both the source binding and captured bytes.

| Method | Caller -> receiver; ifMatch | Params | Result / next owner |
|---|---|---|---|
| delivery.prepare | candidate controller or C request -> S; Work and integration TaskResult | workId, integrationResultId | candidateId, version, criterion/evidence mappings, local destination; inbox item |
| delivery.accept | H -> S; Work and DeliveryCandidate | candidateId | acceptanceId, work version, phase: Completed or Finalizing |
| delivery.request_changes | H -> S; Work and DeliveryCandidate | candidateId, findings: `{criterionId, requestedChange}[]`, preserveArtifacts: ArtifactRef[], advance: boolean | rejected candidate, reworkIds or changeProposalId; advance only within current authority |
| workspace.inspect | authorized client -> S; empty | workId, candidateId? | workspaceId, exact artifact/version, local path, branch/commit if applicable, registered run recipe refs |
| workspace.takeover | H -> S; Workspace | workspaceId | pending operationId; hold conflicting admission, scoped stops, then human writer grant |
| workspace.handback | H -> S; Workspace | workspaceId, summary, resumeAffected: boolean | pending operationId; capture contribution and affected task/input refs, then release human writer authority |
| work.control | H -> S; Work | workId, action: Hold or Resume or Cancel | work version, desired advancement, settlement operationIds |

`workspace.inspect` does not require a delivery candidate. Without `candidateId`,
it returns the current candidate when one exists; otherwise it returns the
provisioned `workspace`, current result references (`results` with task/result
identity and disposition), their fixed `artifacts`, a local `destination`, and
registered `runRecipes`. `candidateId` is absent in that pre-candidate response:
inspection does not imply acceptance or human writer authority. A draft without
a provisioned workspace fails explicitly. An explicit candidate must exist and
belong to the requested work; it never falls back to the current workspace on
an invalid reference. Candidate inspection keeps its fixed artifacts and
destination, while the separate `workspace` field describes the current live
workspace.

delivery.prepare verifies all required contributions, the accepted current
integration result and complete criterion mappings. Candidate content,
destination and evidence are fixed; it is not a live branch label. A code
candidate names the managed directory, branch and immutable commit/snapshot;
a report candidate names readable artifacts. Human acceptance follows the
domain guards and resolves the review item. Local completion needs no external
connector. Finalizing ends on the recorded settlement events; no extra "continue"
message is required.

If human feedback changes only an explanation/report, only affected tasks and
evidence are revised. Goal expansion returns a change proposal for approval.
Neither final revision nor manual edits silently reuse mismatched test evidence.

workspace.takeover is a tracked sequence: identify scopes, hold admission,
request stops, await settlement, then transfer writer authority and emit
TakeoverReady. Inspection alone does none of those steps. handback revokes
managed interactive input, settles remaining tracked writers, captures the
manual snapshot, records affected-input changes and emits HumanContributionRecorded.
The coordinator then replans affected work with those inputs. If a writer
cannot settle, the operation explicitly waits for the named action/repair.

An inspection presentation can remain open after handback. Shell close is not
a hidden prerequisite. resumeAffected clears the manual hold only; it cannot
override an independent work-wide Hold. A requested full resume uses
work.control explicitly. Cancel holds new dispatch immediately and becomes
Cancelled only after its tracked settlement.

## 10. Subscription and visible notifications

| Method | Caller -> receiver; ifMatch | Params | Result |
|---|---|---|---|
| events.subscribe | authorized client -> S; empty | scope `{kind: Work or WorkList or Conversation or Operation, id?}`, afterCursor? | subscriptionId, cursor, snapshot?; then Event frames |
| events.unsubscribe | owning client -> S; empty | subscriptionId | closed: true |

WorkList forbids id; other scopes require it. Without afterCursor the service
atomically selects a committed snapshot/cursor and arranges delivery strictly
after that position before returning. Events cannot precede the subscribe
response on the connection. Reconnect replays strictly after afterCursor;
an expired cursor returns CURSOR_EXPIRED, not a partial successful stream.

```text
Event {
  type: "event", subscriptionId, cursor, eventId,
  kind, subject: EntityRef,
  workId?, taskId?, attemptId?, invocationId?, correlationId,
  changes: {subject: EntityRef, view}[]
}
```

`changes[].view` is the corresponding typed read view or a typed conversation
delta for that subject. Bounded frames may reference a large ArtifactRef;
they never silently truncate authoritative content. The event kinds emitted
by v1 are the following; each maps to the named record/view:

| Kind | Record/view and producer condition |
|---|---|
| MessageRecorded, MessageDelta, MessageCompleted | Conversation; accepted input or correlated runtime stream |
| IntakeRequested, IntakeAnswered, IntakeCancelled | IntakeRequest; recorded clarification and its human disposition |
| WorkDrafted, WorkStarted, WorkChanged, WorkCompleted, WorkCancelled | Work; corresponding committed transition |
| PlanApplied, DispatchCreated, DispatchAcknowledged | Plan/Task; application, admission or worker acknowledgment |
| RuntimeObserved, ProgressReported | Attempt/Task; runtime observation or worker report |
| ContextRequested, ContextAnswered, ContextApplied | ContextRequest; saved request, answer or continuation acknowledgment |
| CoordinationRequested, CoordinationFinished, CoordinationBlocked | Coordination; request, settled turn or no-progress condition |
| ArtifactReady, OperationChanged | Artifact/Operation; captured content or effect step disposition |
| ResultSubmitted, GateEvaluated, ReviewRecorded | Result/evaluation; recorded typed terminal submission |
| ResultChangesRequested, ResultRejected, TaskResultAccepted | Result; policy-controlled verdict |
| DecisionRequested, DecisionAnswered, DecisionApplied | Decision; request, saved answer, applied target |
| TakeoverReady, HumanContributionRecorded | Workspace; completed transfer or captured handback |
| DeliveryProposed, DeliveryAccepted, DeliveryChangesRequested | Candidate; prepared result or human action |
| AttentionChanged | Inbox item; subject obligation created/resolved/superseded |

Allocating a coordinator response emits `MessageRecorded` with its preallocated
assistant item in `Streaming` status, even before adapter startup or the first
text delta. The same item is included in Conversation snapshots so a Console
can show response activity without inferring it from work execution. Terminal
classification emits `MessageCompleted` with `Complete` or `Interrupted`.

Messages and progress are not extra model wake sources. UI refresh, internal
scheduling, coordinator activation and human notification consume different
subsets of this stream. A background event never changes Console selection
or its input target.

Client projection rules are: apply changes by subject version, deduplicate
eventId and chunk identity, then advance cursor after the frame is applied.
Conversation chunk identity is `(messageId, partId, chunkIndex)`; completion
records complete/interrupted explicitly. Runtime sequence and event cursor
are different namespaces. Multiple chunks may share an aggregate version;
chunk identity, not version alone, decides whether text was applied.

The service bounds each outbound subscription to 256 queued frames and 8 MiB.
Exceeding either sends a terminal stream error RESYNC_REQUIRED and closes that
subscription; if the connection is not writable, close it. The client obtains
a new snapshot. This is a defined disposition, not a requirement to run a
reliability experiment before validating the work mode.
The terminal frame is `{type: "stream_error", subscriptionId, failure, cursor?}`;
cursor, if present, is the last sent position, not evidence that the client
applied it. Resume uses the client's last applied cursor. Unsubscribe returns
its response after the subscription is closed; later frames for that ID are
not sent.

Client-side pressure must also have an explicit bounded disposition. A slow
event consumer cannot silently discard committed changes, indefinitely starve
request replies or lose the terminating cause behind a full event queue.
After a stream/connection failure, retain the underlying cause and refresh
the affected authorized subscription by replay or a new consistent snapshot.
Old subscription frames cannot make the refreshed projection older. Connection
recovery does not imply mutation failure or success: preserve command identity
and reconcile uncertain effects rather than automatically issuing a new
mutation. A Console remains usable for its local drafts and navigation and
labels disconnected facts stale; closing and reopening it is not the normal
recovery path.

Needs-input responses and AttentionChanged point to the same durable records.
Delivery badges and decision items show work identity, reason, evidence and
action. Dismissing a notification does not answer the question. Polling is
limited to adapters without push support and explicit runtime reconciliation.

## 11. Closed-loop traces

These are normative ordering examples. Each arrow is a real operation/event,
not a scripted progress card.

### 11.1 Submit, reject, revise, accept, hand off

```text
Scheduler -> S: admit T1/A1; record D1 and I1
S -> R: runtime.invoke(I1, D1)
R -> S: Recorded; runtime.report Started
R -> Worker: ACP prompt D1
Worker -> S: task.acknowledge(D1, Accepted)
Worker -> S: artifact.capture; MCP bridge awaits ArtifactReady
Worker -> S: result.submit -> R1 Submitted
R -> S: TurnEnded Normal/quiescent -> A1 Succeeded
S -> R: runtime.release I1 -> receipt; producer slot available
Controller -> Scheduler: evaluation units for R1 round 1
Checker -> S: gate.submit Failed + concrete evidence
R/S: settle and release required check units
Controller -> S: result.request_changes -> ChangesRequested
Driver -> R: coordinator invocation with R1 and findings
Coordinator -> S: task.rework(ReviseOutput); coordination.finish
R/S: settle and release the completed coordinator invocation
Scheduler -> R: new A2/D2/I2 with R1 and structured correction
Worker -> S: acknowledge; capture; submit R2 superseding R1
R -> S: TurnEnded/quiescent
S -> R: runtime.release I2 -> receipt
Checker/reviewer -> S: gates Passed; required review Accept; settle/release
Controller -> S: result.accept R2
Scheduler: bind R2 outputs and admit ready downstream task
```

No user transports R1's errors to A2. Submission, attempt completion and task
acceptance are three distinct facts.

### 11.2 Internal question with answer arriving early

```text
Worker -> S: task.request_context(blocking=true) -> Context Q1
S: WaitingForContext; driver resolves/routes Q1
Responder -> S: task.answer_context(Q1) -> answer B1, Queued
Worker yields; R -> S: TurnEnded Normal/quiescent
S: Q1 still awaits delivery; do not classify MissingSubmission
S -> R: runtime.continue(C1 for Q1/B1)
R -> Worker: ACP continuation when idle
Worker -> S: task.acknowledge(continuationId=C1, Accepted)
S: Q1 Applied; Attempt Running
Worker executes and submits the required result
R/S: terminal settlement/release; rejoin the evaluation path in section 11.1
```

If user judgment is required, a DecisionRequest replaces the responder step;
the same bound continuation carries the applied decision back.

### 11.3 Evidence-only correction and final user revision

```text
R1 has usable code, missing required evidence -> NeedsEvidence
Coordinator -> S: task.rework(CollectEvidence)
Controller: R1 round 2; capture matching check/review evidence
Controller: accept R1 without another implementation attempt
Integration task: combine accepted results, submit, pass combined checks
Controller -> S: delivery.prepare -> candidate C1, inbox
Human -> S: delivery.request_changes(C1, report explanation only)
Coordinator: revise affected report task; retain matching code evidence
Integration/controller: prepare C2 from revised exact manifest
Human -> S: delivery.accept C2
S: resolve review; finish tracked settlement -> WorkCompleted
```

## 12. Closure ledger and implementation obligations

Every nonterminal condition has an owner and an explicit wake/exit:

| Waiting condition | Owner | Wake / successful exit | Unsuccessful exit |
|---|---|---|---|
| Draft, intake or material proposal needs approval/input | Human/inbox | work.start, conversation.answer_input or work.apply_change | Cancel/retained draft or question; no automatic execution |
| Dispatch recorded, start unconfirmed | Runtime dispatcher | Started observation | Probe same invocation; Unknown -> repair |
| Contract unacknowledged | Runtime/worker | task.acknowledge | Decline, deadline, or turn end -> failed dispatch obligation |
| Blocking context/decision | Responder, driver or human | Recorded answer plus acknowledged continuation | Supersede/cancel/revision; no hidden polling loop |
| Artifact capture pending | Capture manager/MCP bridge | ArtifactReady/OperationChanged | Explicit capture failure; no valid ArtifactRef |
| Result submitted, producer still active | Runtime settlement | TurnEnded and Settled | Scoped stop/repair; no replacement writer |
| Required check/review pending | Scheduler/evaluation controller | Matching terminal record and settlement | Inconclusive/missing review -> bounded correction/blocker |
| Changes requested | Coordinator driver | task.rework or approved plan change | Recorded human choice/blocker; not endless automatic retries |
| Coordinator running | Driver/runtime | valid coordination.finish plus settlement | PROTOCOL_INCOMPLETE or deadline -> bounded blocker |
| Human edit active | Human/workspace operation | handback capture and authority transfer | Named unsettled writer/repair remains visible |
| Candidate awaiting acceptance | Human/inbox | delivery.accept | delivery.request_changes or work cancel |
| Accepted, still finalizing | Settlement controller | Remaining tracked executions settle | Explicit operation failure/repair; no premature Completed |
| Terminal invocation still bound to runtime | Runtime dispatcher | runtime.release receipt clears binding and execution slot | Tracked release failure/repair; no implicit scope reuse |
| Subscriber disconnected/behind | Client/event service | Resume cursor or new snapshot | Explicit stream end/error, not success with missing changes |

Deadlines and allowances are mandatory fields from the approved profile, not
an unspecified polling interval. A waiting human decision may remain open;
that is a legitimate externally owned wait, not a stuck internal protocol.
All other waits have the named controller and recorded subject above.

Implementations must derive operation/MCP schemas from these contracts, supply
one reducer for each runtime/evaluation transition, and make the three traces
observable using the same records/events consumed by the Console. UI effects,
model text and transport delivery cannot substitute for the required operation.
No transition depends on a developer inventing a "continue" prompt outside
this contract.
