---
author: Kai
created on: 2026-09-15
last updated: 2026-09-18
---

# Agent Center: Task-Driven Terminal Domain and Execution Contracts

**Design specification v0.8 - Work-Owned Executor Sessions**

This specification defines the domain, authority, execution, and recovery
contracts for the complete [Agent Center product experience](agent-center-product.md).
The current milestone validates the work model through a real, continuous
experience: delegation, internal handoff, review, rework, and usable delivery.
The acceptance cases also retain longer-term engineering requirements; they
are not all prerequisites for this experience prototype. Section 19 separates
the two kinds of evidence. All interfaces below are target contracts, not a
claim of implemented functionality.
The [Collaboration Protocol v1](agent-center-protocol.md) is the normative
implementation contract for this experience boundary. It fixes wire envelopes,
operation payloads/responses, actor ownership, transitions and terminal
dispositions; the shapes in this document are domain summaries. Implementers
must not invent different meanings where those summaries omit transport detail.

## 1. Purpose

Terminal is the easiest way to get tasks done on the user's OS. Agent Center
is its task-centered work service and conversation entry, not a requirement
for the user to manage a collection of agents. It turns an approved goal into
an inspectable result.
It owns the continuity of that work: scope, planning, execution, decisions,
evidence, acceptance, and resource disposition.

The organizing unit is a work, not a command, shell, provider or chat session.
Users discuss desired outcomes, execution location, progress, strategy and
acceptance. Agents, commands and shells are implementation resources selected
inside the approved policy; normal work must not require the human to launch
the next worker, copy diagnostics, compact a provider session or rebuild its
context. Optional expert controls remain available.

The first qualification scenario is local developer work producing `LocalCode`
or `Report` under protocol v1. Broader OS/application outcomes and adoption of
existing execution resources are product targets, not newly implemented wire
types or claims of unrestricted process control. A report describing a desired
OS change is not evidence that the change occurred. Those capabilities require
explicit adapter, effect-verification and recovery qualification before use.

The human defines desired results and acceptable authority, makes consequential
decisions, and accepts delivery. The system advances ready tasks inside that
authority, coordinates execution capabilities, verifies handoffs, and presents
the next meaningful human action.

The entire Terminal is an execution environment. One window-scoped Agent
Console is the human work entry point, rendering a keyboard-first TUI in an
independent XAML host. Shells are optional execution resources and presentations.
The same work capabilities are available through a standalone TUI and
authenticated structured CLI/MCP clients.

The primary ownership and interaction chain is:

```text
Global conversation -> Non-executing master -> Approved Work
                                               |
                                      Durable executor session
                                               |
                          Serial work input -> Execution -> Reply / Review
```

Each Work owns its executor session. Work chat, continuation, recovery and
opening the work in another tab target that same executor, not a per-work
planning coordinator or a newly selected transient worker. The master manages
work but does not execute filesystem or shell operations itself. Opening a
view does not create an additional writer or restart execution.

Idle chat completion preserves the Work's provider session and approved running
resources, including a development server requested for human verification.
It is distinct from Work completion, acceptance, pause and cancellation.
New input is serialized through the same owner; recovery requires matching
work, provider, session and workspace provenance and safe writer settlement.
An unavailable session is not silently replaced.

The existing staged task/evaluation pipeline remains a legacy compatibility
contract for historical records and in-flight work. It is not the mandatory
routing path for Work-owned executor chat. Migrating a legacy Work first fences
its scheduler and settles existing writers; a coordinator session cannot be
relabelled as the executor. The legacy chains are:

```text
Intent -> WorkSpec -> ExecutionGrant -> Plan -> Task/Attempt
       -> Artifact -> QualityGate -> DeliveryCandidate
       -> HumanAcceptance -> RequiredPublication -> CompletedWork

TaskDispatch -> Acknowledgment -> Progress -> TaskResult -> TaskReview
                                                      -> Accepted handoff
                                                      -> Rework -> New attempt

Workspace -> ExecutionSandbox -> WriterOwnership -> IntegrationCandidate
          -> VerifiedSnapshot -> Presentation

Obligation -> Decision -> AppliedChange -> ResolvedObligation
```

Every presented fact has an owner and provenance. Every side effect has an
authorization and operation identity. Every result has a version.

## 2. System boundaries

```text
Window Console TUI      Standalone TUI      Agent / Script
 XAML terminal host     ordinary TTY        CLI JSON / MCP
          |                  |                   |
          +------------------+-------------------+
                             |
             Command Registry / Typed Work APIs
                         |
                 Agent Center Service
                         |
    +--------------------+-----------------------+
    |                    |                       |
Work and Plan       Decisions and          Project Context
Engine              Acceptance             and Evidence
    |                    |                       |
    +--------------------+-----------------------+
                         |
        Transactional Store / Operations / Outbox
                         |
        Scheduler / Policy / Resource Reservations
                         |
       +-----------------+----------------------+
       |                 |                      |
   Runtime Host     Workspace and          Publication
   and Adapters     Integration Manager    Connectors
       |                 |                      |
Agent capabilities  Isolated files,        External receipts
and session routes  terminals, artifacts
       +-----------------+----------------------+
                         |
          Correlated observations and recovery
```

The window opens in agent-focus layout with one Agent Console and no required
user shell. Its TUI composes home, work selection, conversation, plan, evidence,
decisions, and history. A workbench is a selected-work view inside that console,
not an additional console or a physical shell tab.

```text
WindowRoot
  AgentConsoleHost       one window-owned TUI presentation
  ShellWorkspaceHost     optional shell tabs/panes
```

The two hosts are siblings in the window layout. The TUI host's terminal
rendering channel and client process are presentation infrastructure, distinct
from a user shell, agent execution session, and work resource. Startup can
launch the TUI directly without PowerShell/cmd, a shell profile, a user shell
tab, or an active-shell selection.

Each window has its own console selection, drafts, navigation, and layout.
All consoles access the same authorized work service. A work may have zero
or many shell sessions and zero or many shell presentations. Opening or
selecting a work does not allocate a tab or restart its agents.

The service is a headless local authority for one user's application state
root. Its lifetime and execution hosting are independent of presentation
windows. The runtime host owns managed execution lifetimes; host adapters
provide optional terminal presentations and manual interaction. Background
planning, execution, decisions, and delivery do not require a Console client.
A presentation-only operation with no compatible host returns a precise
capability result rather than creating an implicit GUI dependency.

Machine sleep or shutdown suspends availability of local execution. Persistent
work records survive; startup reconciliation establishes what can reconnect,
restart safely, or requires a decision. Execution location and availability
are visible product facts.

### 2.1 Runtime and host adapters

| Adapter | Responsibilities |
|---|---|
| Window host | Independent Agent Console XAML host, TUI terminal rendering, layout, focus return, correlated shell tab/pane presentations |
| TTY host | Same TUI on a supported ordinary terminal; foreground shell interaction and return without a graphical window |
| Headless client | Typed requests, structured results, event subscriptions, and explicit missing-input responses |
| Execution adapter | Owned child processes or PTYs, cwd/environment, authorized files/tools, output, cancellation, and recovery |

The work engine and execution API must not require a WT COM server or window
tab for every operation. A Windows window adapter may use the WT protocol for
window-specific presentation. A supported non-window execution adapter must
complete core work flows without activating a graphical Terminal instance.
Host enumeration concerns registered, authorized resources rather than
unrestricted control of arbitrary processes on the machine.

Resource selection is the system's responsibility within that boundary.
It must identify the actual authorized execution location and capabilities,
not silently fall back to another host, directory, provider or account.
An unavailable capability produces a named limitation or approval request;
the user's request to "do the work" does not grant installation, elevation,
access to new data destinations or control over unrelated running processes.

### 2.2 Collaboration ownership

The user interacts with one work-organizing collaborator. Each work has its
own context and coordination stream; this does not require one global model
session or expose each worker as a separate human conversation.

| Participant | Responsibility |
|---|---|
| Coordinator agent | Interpret intent, propose plans, answer from work evidence, organize internal clarification and rework, and explain consequential choices |
| Coordinator driver | Programmatically consume relevant events, queue/coalesce work, and invoke the coordinator through the runtime adapter |
| Work service | Record authoritative tasks, submissions, reviews, decisions and events; validate and apply proposed operations |
| Scheduler | Dispatch ready work and declared checks; advance known dependencies without requiring a model call for each transition |
| Worker agent | Acknowledge a task, execute its contract, report progress, request context, and submit a result |
| Check executor / reviewer | Inspect an exact submission and return evidence and findings under the declared review policy |
| Runtime adapter | Deliver ACP prompts, expose work tools, and report observed session/tool/turn execution independently of worker self-report |

A WorkItem is a record, not a process that emits its own progress. Neither the
coordinator's prose nor a worker's "done" message directly changes acceptance.
Task and result exchange goes through the work service. Targeted questions
and findings supplement, rather than replace, versioned artifact handoffs.

## 3. Domain inventory and identity

Durable entities use opaque service-generated UUIDs, timestamps, and versions.
Human-facing short IDs uniquely resolve within the store and expand when
necessary. Titles, paths, provider session IDs, issue numbers, and branch names
are locators or labels rather than work identities.

| Entity | Meaning and ownership |
|---|---|
| `Project` | Approved resource context, execution capabilities and policy; the v1 developer pilot uses repositories and data sources |
| `WorkItem` | A durable goal with one current specification and advancement policy |
| `WorkSpec` | Immutable revision of goal, scope, acceptance criteria, and delivery requirements |
| `ExecutionGrant` | Approved capabilities, data access, side effects, and resource limits for a work |
| `PlanRevision` | Versioned task graph and gate definitions consistent with the work specification |
| `Task` | Bounded responsibility with explicit inputs, outputs, dependencies, and resource requirements |
| `Assignment` | A capability or human assigned responsibility for a task; retained through replacements |
| `Attempt` | One execution against fixed inputs and an effective grant |
| `TaskDispatch` | Exact task contract, inputs, review policy and reporting instructions delivered to an attempt |
| `TaskResult` / `TaskReview` | Immutable task submission and version-bound internal evaluation, distinct from final human acceptance |
| `ContextRequest` | Internal clarification with an explicit recipient, affected scope and recorded answer |
| `IntakeRequest` | A human clarification for a conversation before a work exists, with explicit answer/cancellation |
| `CoordinationTurn` | One programmatically triggered coordinator invocation with captured events, context and recorded outcomes |
| `SessionBinding` | A generation-scoped route between an attempt and an actual runtime session |
| `Workspace` | Work-owned durable resource set and integration baseline |
| `ExecutionSandbox` | Isolated writable or read-only environment for an attempt or human |
| `WriterReservation` | Exclusive managed writer authority for a physical writable resource |
| `ConsoleSession` | Immutable global or work conversation registration; a client separately retains its layout, drafts and reading positions |
| `Conversation` / `ConversationItem` | UI-independent work or console-scoped interaction and streaming content |
| `ShellSession` | An owned interactive shell with optional work association and independent liveness |
| `ShellInputLease` | Exclusive interactive input authority for a shell, scoped to a client or managed execution |
| `PresentationBinding` | A console or shell view attached to a host/window/pane and optional work resource |
| `Artifact` | Versioned output with content identity, provenance, and retention policy |
| `GateResult` | Evaluation of a declared predicate against an exact input manifest |
| `DeliveryCandidate` | Immutable work-level result bundle mapped to acceptance criteria |
| `Acceptance` | Human acceptance of a candidate and specification revision |
| `Publication` | Authorized external delivery with exact destination and observed receipts |
| `DecisionRequest` | A consequential choice with context, options, impact, and an application target |
| `DecisionAnswer` / `DecisionApplication` | Saved human input and its acknowledged application |
| `Blocker` / `AttentionItem` | A constraint on progress and a projection of what requires attention |
| `ContextSnapshot` | Exact approved input manifest used for planning or execution |
| `Operation` | Durable intent, dispatch, receipt, and recovery state for external effects |

One work can own many tasks, assignments, attempts, sandboxes, and candidate
deliveries. It has one current specification, one current plan, and one current
integration head. Each mutable filesystem location has at most one managed
writer. A provider session has at most one current attempt binding.
One window owns exactly one live Agent Console presentation. A shell tab does
not own an Agent Console. Console identity, provider session identity, shell
identity, and durable work identity are distinct.

One-level stages and nested task groups are presentation choices over the same
graph. Cross-work dependencies are explicit versioned handoff relationships
inside the authority's work store.

## 4. Intent, work specification, and planning

```text
WorkItem
  id / version / projectId
  title / priority
  lifecycle: Draft | Active | Completed | Cancelled
  desiredAdvancement: Advance | Hold | Cancel
  currentSpecRevision / currentPlanRevision
  currentGrantId / workspaceId?
  currentCandidateId? / currentAcceptanceId?
  archivedAt?

WorkSpec
  workId / revision
  goal / scope / exclusions
  criteria[{ id, description, requiredEvidence, evaluationPolicy }]
  deliveryRequirements[]
  humanCheckpoints[]
  externalReferences[]
```

Explicit delegation creates an intent and draft. Immediate questions and
discussion of existing work remain conversation operations; they do not
require a work-start or final-acceptance ceremony. A planner may propose a goal summary,
criteria, execution graph, and resource estimates. Planning itself is a metered
operation under a project-approved planning grant. Data access and model
destinations are checked before material is sent to a planning capability.

Only user approval or an already explicit project policy can establish an
execution grant. A planner cannot grant itself access, enlarge a budget, or
approve its own generated scope.

An immediate answer can be promoted into a work when the user asks for sustained
investigation, implementation or tracked delivery. Its selected conversation
and evidence become referenced inputs rather than a goal the user must repeat.
An explicitly commissioned report uses the work lifecycle even if it has no
code output. Conversation/planning calls remain metered under project policy.

Starting a work requires a confirmed specification, applicable grant, and
valid resource plan. It makes the work active and lets the scheduler advance
eligible tasks. Saving a draft performs only the operations explicitly
authorized for drafting; execution and resource provisioning await start.

### 4.1 Specification and plan changes

An approved specification is immutable. Scope, required output, quality, data
access, or budget changes create explicit change proposals with impact:

- affected criteria, tasks, inputs, and candidates;
- new or revoked permissions and resource requirements;
- work already performed that can still be reused;
- running tasks that need cancellation or a safe checkpoint;
- downstream work affected by changed handoffs.

The human confirms consequential changes. The service revokes affected
dispatch authority immediately, records a new specification/grant revision,
and settles already-dispatched effects through tracked operations.
Acceptance and publication approvals for superseded content retain their
historical meaning but lose current authority. Undispatched incompatible
publication is held; an already-dispatched effect is reconciled and reported
against its original content rather than relabeled as the revised result.

Within an approved specification and grant, the planner may revise execution
details autonomously: decomposition, order of independent tasks, compatible
capability replacement, and bounded retries. Every revision records its reason.

A user-requested strategy replacement first resolves the intended work and
shows the current reason, proposed alternative, retained outputs, affected
attempts and resource/authority impact. The confirmed choice must lead to a
recorded compatible plan revision or specification/grant change as appropriate,
not only conversational agreement. Ordinary exploration of alternatives does
not itself change the plan. Already-dispatched effects are settled or reconciled
under their original identity before incompatible replacements are admitted.

Unaffected in-flight tasks can continue through an explicit carry-forward map
between plan revisions. A carry-forward requires identical task contract,
input identities, permissions, and relevant acceptance conditions. A changed
task's old outputs become historical until explicitly reevaluated.

### 4.2 Conversation intake and changes during execution

Intake distinguishes `ImmediateQuestion`, `WorkDiscussion`, `NewWork`,
`ContextAddition`, and `ChangeProposal`. A submitted message retains its
console context and original text. Resolved intents identify their own target
work or proposed new work; one message can produce several explicitly displayed
intents. Ambiguous targets or material changes wait for clarification/approval.
The user sees what was understood and can correct the interpretation.

Global questions such as "where are my works running and what needs me?"
must be answered from authorized work, runtime, plan and attention records.
The answer identifies each target, observation freshness and unknown state.
Strategy explanations reference recorded plan reasons, constraints, decisions
and evidence; plausible model narration is not authoritative progress.
This is a target read/interaction contract, not permission to bypass a
work-scoped invocation binding. Missing global read capability must be surfaced
and tracked rather than filled by unrestricted provider filesystem reads.

Work discussion can continue while independent authorized tasks advance.
It must not reset the current work, pause execution, dispatch a new worker or
rewrite pinned inputs merely because the user asks a question. A mixed message
can create B and propose a strategy change to A, with separately displayed
targets and approvals. Ambiguous references are clarified before mutation;
changing the currently viewed work cannot retarget an in-flight answer.

Answering a question does not dispatch a worker. Recording a new attachment
does not silently inject it into an active attempt's pinned input manifest.
An internal clarification consistent with existing inputs can resume the same
attempt. New execution inputs or constraints follow the revision, carry-forward
and replacement paths. Acknowledgments distinguish "recorded/proposed" from
"applied to execution", including any affected execution still settling.

## 5. Execution authority and resource policy

```text
ExecutionGrant
  id / workId / specRevision / issuedBy
  allowedCapabilities[]
  dataScopes[] / approvedModelDestinations[]
  writableResourceScopes[]
  allowedOperationKinds[]
  limits{ concurrency, attempts, duration?, cost? }
  checkpointPolicy / expiresAt? / revokedAt?
  version
```

Every dispatch checks current grant validity, task scope, input version,
capability enforcement, and resource reservation. Delegated tasks receive a
subset of their parent's authority. External publication, destructive cleanup,
and sensitive data expansion require explicit operation-scoped approval unless
a specific approved policy already covers that exact action.

A strategy decision changes work direction; an authorization changes permitted
effects. Their records may be created in one confirmed workflow, but they
remain separately typed and audited.

### 5.1 Enforcement and capability qualification

Capabilities declare and demonstrate:

- supported task kinds and structured output contracts;
- data and filesystem isolation mechanisms;
- start correlation, liveness observation, and cancellation semantics;
- input/decision acknowledgment and deduplication support;
- session loading and reconnect behavior;
- resource metering and enforceable limits.

Tool prompts are instructions rather than enforcement mechanisms. A grant
requiring filesystem or network restrictions can run only on an executor that
enforces them through its tool broker or isolation environment. A separate
worktree provides managed write isolation, while access restrictions require
their own verified enforcement.

Provider-owned tools and terminal-control actions are distinct execution
paths. Each capability's qualification states which path it controls. Terminal
mutation uses an authorization-gated action broker with exact work, attempt,
target, and operation identity.

An executor that lacks a mandatory property is ineligible for that task.
The product presents a usable alternative or a concrete blocked condition.
It must not label a prompt instruction, usage estimate, or unobserved process
as a hard execution guarantee.

### 5.2 Admission and budgeting

Work starts authorize scheduling; users do not have to approve each internal
handoff. Task admission atomically reserves:

- a concurrency slot in the applicable project/work limits;
- a compatible executor;
- exclusive writer ownership when needed;
- any enforceable budget reservation required by that executor.

Ready work is ordered by user priority and stable queue order. Fairness and
dependency readiness are explicit scheduler policies. Priority changes affect
future admission; preempting an active task follows a separate authorized
control operation.

Waiting-for-decision, waiting-for-context, stopping, and uncertain external executions retain their
relevant reservations. Lowering a limit below usage creates a visible draining
state. Admission resumes as occupancy falls below the new limit.

Hard duration or spending limits require executor-side enforcement with a
defined stop/overshoot bound. Polling-only metrics are advisory. The UI labels
measured usage, estimates, reserved amounts, enforceable caps, and unknown
values separately.

Automatic retries have a finite approved attempt allowance and error
classification. A retry consumes that allowance even if the previous attempt
produced no useful output. Unknown side effects require reconciliation before
any replacement writer is admitted.
The work-level allowance includes all delegated tasks, planner operations, and
replacement attempts as applicable to their metering category. Renaming a task
or creating another plan revision does not reset usage or create new authority.

## 6. Plan graph and handoff semantics

```text
Task
  id / revision / workId / planRevision / parentGroupId?
  objective / responsibility
  inputContracts[] / outputContracts[]
  criteria[] / requiredGates[] / requiredForDelivery
  reviewPolicy{ revision, reviewerCapability?, requiredReviewKinds[], acceptanceRule }
  requiredCapabilities[] / resourceNeeds

Dependency
  id / consumerTaskId
  sourceTaskId? / sourceWorkId?
  sourceOutputSlot
  condition: ArtifactAvailable | GatePassed | WorkAccepted
  requiredGateIds[]
```

The scheduler derives task readiness from valid inputs, dependencies, gates,
authority, and resource availability. A finished attempt is an observation;
satisfied task contracts determine whether downstream work can proceed.

Dependency conditions are deliberately different:

| Condition | Meaning |
|---|---|
| `ArtifactAvailable` | A schema-valid, immutable output exists; suitable for a report that another task must inspect |
| `GatePassed` | Required declared evaluations passed on the exact output and input versions |
| `WorkAccepted` | A work-level accepted delivery meets the producer's applicable completion contract |

`WorkAccepted` includes required publication/settlement conditions, not merely
the existence of an Acceptance record. Internal task handoffs use artifact or
gate predicates and do not wait for final human acceptance unless explicitly
declared. An `ArtifactAvailable` edge can intentionally feed an unchecked
submission to a checker; it does not assert that the producing task passed.

Internal technical gates can advance the plan automatically. Human review
gates exist at explicitly approved checkpoints and final acceptance.
An advisory model review is labeled advisory; it is not interchangeable with
a deterministic verification result.

Validate duplicate edges, endpoints, self-edges, output types, and cycles.
Cycle checking includes cross-work dependencies. Establishing or changing a
cross-work edge requires authority over the consumer and visibility into the
producer's selected output. It does not authorize starting an unapproved work.

Before starting a consumer, resolve the output slot to exact artifact,
acceptance, gate, and code-baseline identities. Pin that input manifest in the
attempt's context snapshot. Subsequent producer outputs do not silently replace
an already-consumed version.

Revoking an input's validity invalidates dependent gates and candidates
transitively. Block future affected dispatches and settle active affected
attempts. Unaffected work may continue. Reuse requires an explicit carry-forward
or reevaluation against current contracts.

### 6.1 Task dispatch and reporting contract

The scheduler records a TaskDispatch for an admitted attempt. For an agent
capability, the runtime adapter renders that package into an ACP prompt and
exposes the matching work-scoped MCP tools. A command-check adapter executes
the declared recipe directly. ACP transports execution; it does not define
task acceptance or peer-to-peer work semantics.

```text
TaskDispatch
  id / workId / taskId / taskRevision / attemptId
  kind: ProduceResult | EvaluateGate | ReviewResult
  subjectResultId?
  objective / scope / exclusions
  inputManifest / contextSnapshotId / workspaceRef
  outputs[{ slot, kind, schema, required }]
  criteria[{ id, description, requiredEvidence }]
  gateDefinitions[] / reviewPolicy
  effectiveGrantRef / reworkFromResultId? / reviewFeedbackRefs[]
  reportingContractVersion: 1

ProgressReport
  activity / findings[] / artifactRefs[]
  nextStep / blocker?
  coordinationRequest?{ reason, affectedTaskIds[], proposedAction }
```

A gate definition supplies the evaluation kind, pinned inputs, executor and
recipe, expected result/evidence, and execution limits. A command-based check
names its cwd, argument vector, environment reference, timeout and evidence
parser. A human checkpoint identifies its criterion and authorized response.
These are executable task contracts rather than free-text requests to "verify".

The following payloads extend the Command envelope in section 13. Worker
requests bind to their dispatch/attempt and task revision; identity comes from
the runtime binding. Returned IDs refer to service records. Transport acceptance
of a request is not task acceptance.

| Operation | Producer -> consumer | Required payload / response |
|---|---|---|
| `task.assign` | Scheduler -> service admission | Internal creation of TaskDispatch/Invocation; runtime.invoke is the actual runtime request |
| `task.acknowledge` | Worker -> service | dispatchId, taskRevision, continuationId when resuming, `Accepted` or `Declined`, reason if declined |
| `task.get` | Authorized participant -> service | taskId; returns contract, permitted inputs, current result/review and context requests |
| `task.report_progress` | Worker -> service | ProgressReport; returns recorded report/event identity |
| `task.request_context` | Worker -> service/coordinator | question, target coordinator/task, relevant input refs, blocked scope; returns pending requestId |
| `task.answer_context` | Assigned responder -> service/runtime | requestId, answer, evidence refs; returns recorded answer and delivery disposition |
| `result.submit` | Worker -> service | TaskResult content below; returns resultId and `Submitted`, or a specific correction error |
| `gate.submit` | Declared check executor or human-gate application -> service | evaluation unit, result/round, gate definition/input identity and GateResult |
| `review.submit` | Declared checker/reviewer -> service | TaskReview content below; returns recorded evaluation identity |
| `result.accept` | Service policy controller, or coordinator request -> service | resultId, taskRevision, review/evidence refs; service validates all acceptance predicates |
| `result.request_changes` | Service policy controller, or coordinator request -> service | resultId and structured feedback; records disposition and rework obligation |
| `result.reject` | Service policy controller, or coordinator request -> service | resultId, reason and proposed next disposition; reject the submission, not automatically the work |

MCP tool names use the corresponding underscore form, such as
`task_report_progress` and `result_submit`. Schemas and meanings come from the
same registry as typed service requests; these internal roles do not imply
that every operation is a human slash command.

The worker acknowledges before beginning task work. Runtime start and worker
acknowledgment are separate facts; a decline reports an assignment problem to
coordination and settles that attempt rather than fabricating progress.
Reports occur at meaningful milestones, changed findings, blockers and
submission, not on every token or on a model-driven polling timer.
An explicit coordinationRequest identifies a finding that needs replanning;
the service queues CoordinationRequested, not an automatic plan change.
The driver does not need to run a model on every progress paragraph to discover
whether the worker wanted help.

Three projections remain distinct: runtime-observed execution, worker-reported
progress, and service-evaluated task outcome. Progress text and percentages do
not satisfy criteria. A provider turn ending is an observation, not a result.
An ended execution without its required submission produces `MissingSubmission`
as a task obligation and triggers bounded coordination, never automatic success.

### 6.2 Internal context exchange

A ContextRequest records requester/attempt, recipient, input references,
question, blocked scope, answer/evidence and
`Open | Answered | Applied | Superseded | Cancelled`.
Only a request that blocks execution moves the attempt to WaitingForContext.
For that blocking request, the tool returns a pending receipt and the worker
yields its ACP turn. The runtime retains the logical attempt and its outstanding
request rather than interpreting this turn boundary as missing submission.
A nonblocking question does not end the worker's current execution.

The coordinator answers from approved work facts or routes a focused question
to a relevant participant. Shared discoveries include source and result/review
status; an unreviewed finding is not silently promoted to a satisfied dependency.
New investigation becomes a planned task with ordinary dependency validation,
not a hidden chain of agents waiting on each other's chat.

When an answer is recorded, the runtime continuation dispatcher delivers it at
an idle/supported continuation boundary of the bound attempt. Applying the
answer records a receipt; the worker does not poll task.get waiting for it.
An unchanged-input clarification can resume the same attempt. Material input
changes require the revision path; a terminal attempt receives a replacement,
not an unsolicited resurrection.
An answer is Applied only after the worker acknowledges its exact continuation.
An early answer queues until the current turn yields; it is not mistaken for
the absence of a waiting request. The protocol defines the pending receipt and
continuation fields for this transition.

Questions about internal implementation or missing shared facts remain inside
this loop. A consequential choice that requires the user creates a linked
DecisionRequest under section 11. The coordinator cannot answer that decision
on the user's behalf. Its eventual applied result resolves or supersedes the
linked context request.

### 6.3 Submission, internal review and rework

```text
TaskResult
  id / workId / taskId / taskRevision / attemptId
  contextSnapshotId / inputManifest
  outputs[{ slot, artifactRef }]
  criterionEvidence[{ criterionId, evidenceRefs[], claimedOutcome }]
  summary / knownGaps[] / supersedesResultId?
  disposition: Submitted | Reviewing | Accepted | ChangesRequested | Rejected | Superseded

TaskReview
  id / resultId / taskRevision / reviewPolicyRevision / reviewerIdentity
  gateResultRefs[]
  findings[{ criterionId, evidenceRefs[], explanation, requestedChange }]
  recommendation: Accept | NeedsEvidence | ChangesRequested | Reject
  preserveArtifactRefs[] / evaluatedAt
```

Result bodies are immutable. Artifact references must identify captured,
readable versions, not mutable output filenames that a worker keeps editing.
The service captures/verifies artifact content before acknowledging submission.
There is one current formal submission per producing task and at most one per
ProduceResult attempt; intermediate findings are progress/artifacts. A newly recorded submission
can supersede the prior current submission but retains its review history.
Supersession changes currency; invalidation of already-consumed accepted
evidence is a separate operation.

Submitting first validates the package shape, identity and artifact references.
Missing proof of a criterion can be declared as a gap and evaluated as
NeedsEvidence; an invalid package returns a correction error while the current
execution can still correct its request. Changing a submitted body or output
requires a new result through a new attempt after the previous execution settles.
Additional check evidence can instead attach a new review to the same fixed
result. Evidence-only work need not repeat implementation or manufacture a new
code submission.

The service schedules the declared checks and reviewer against the exact
submission. Checker/reviewer work is explicitly associated with its target
result and metered. EvaluateGate and ReviewResult dispatches name subjectResultId;
their required completion is gate.submit or review.submit, not result.submit
and not a product submission that recursively requires another reviewer. Model opinion remains
advisory under section 10 and cannot substitute for a missing test run.

| Evaluation | Service outcome and next action |
|---|---|
| Required outputs, criteria, gates and declared review requirements satisfied | Record internal Accepted and publish selected output-slot bindings for downstream admission |
| Required evidence missing or inconclusive | Record ChangesRequested with NeedsEvidence feedback; arrange evidence collection, preserving usable output |
| Output violates the task contract | Record ChangesRequested with exact criteria, evidence, requested corrections and content to preserve |
| Result unusable or task interpretation wrong | Record Rejected; coordination clarifies/reassigns or proposes a plan change |
| Repair would expand goal, authority or budget | Create a user decision rather than widening the task automatically |

Task acceptance is committed by the work service, not asserted by a worker or
an unqualified reviewer. All required task outputs and criteria participate.
The current accepted result selects one version for each output slot; consumers
pin those versions. A plan cannot silently drop requiredForDelivery tasks to
assemble a final candidate.

```text
dispatch -> acknowledge -> execute/report -> submit -> review
                                               ^        |
                                               |        +-> accept -> downstream
                                               |
                            new attempt <- changes requested
```

Rework is not "try again" prose. It binds a result, failed/missing criteria,
evidence, requested changes and reusable artifacts. The coordinator proposes
the bounded correction; the scheduler creates a new attempt with those inputs.
It can retain the responsible capability or choose a compatible replacement.
New attempts and checks consume the existing allowances. A repeated unresolved
failure or exhausted allowance produces a concrete blocker/choice, not an
unbounded retry or coordinator wake loop.

Internal task acceptance, combined-result integration checks, and final human
acceptance are separate. Successful internal handoffs do not ask the user to
approve each child task. User feedback on a final candidate enters this same
rework mechanism for the affected tasks, preserving unaffected contributions.

## 7. Assignment, attempts, and runtime routing

```text
Assignment
  id / taskId
  responsibleActor / capabilityId
  role / status / assignedAt / endedAt?

Attempt
  id / taskId / assignmentId
  specRevision / planRevision / effectiveGrantId
  contextSnapshotId / sandboxId
  attemptNumber / replacesAttemptId?
  state / endReason?
  currentBindingGeneration
  startOperationId / startedAt? / endedAt?

SessionBinding
  attemptId / generation
  executorInstanceId / providerId / executionLocation
  providerSessionId / routeIdentity
  status / observedAt
```

The service records task responsibility and runtime routes separately.
Multiple assignments may cooperate on a work; each attempt has one responsible
actor and a bounded output contract. Agent-generated plans remain proposals to
the authoritative engine rather than direct scheduling authority.

| Transition | Meaning and guard |
|---|---|
| New -> `Queued` | A ready task is authorized for admission |
| `Queued` -> `Dispatching` | Resource reservations and durable start intent committed |
| `Queued` -> `Cancelled` / `Failed` | Cancelled before dispatch or readiness revalidation failed |
| `Dispatching` -> `Running` | Executor confirms exact attempt identity and start |
| `Running` -> `WaitingForDecision` | A blocking decision is persisted |
| `WaitingForDecision` -> `Running` | Valid application of the answer is acknowledged |
| `Running` -> `WaitingForContext` | An internal blocking context request is persisted and the worker yields |
| `WaitingForContext` -> `Running` | Worker acknowledges the exact compatible answer continuation |
| Dispatched nonterminal -> `Stopping` | Authorized control requests a checkpoint or cancellation |
| `Running` -> `Succeeded` | Bounded execution settled with its required terminal record; output validation and gates are evaluated separately |
| `Dispatching` / `Running` / `WaitingForDecision` / `WaitingForContext` -> `Failed` | Definitive execution failure ends this attempt |
| `Stopping` -> `Cancelled` | Confirmed cancellation and managed writer disposition established |
| `Stopping` -> `Cancelled` with endReason `Checkpointed` | Durable checkpoint and writer quiescence confirmed; resume uses a new attempt |
| `Stopping` -> `Succeeded` / `Failed` | A correlated natural completion wins the control race |
| Dispatched nonterminal -> `Interrupted` | Reconciliation cannot safely reconnect the attempt |

All other transitions require an explicit contract revision. A fast completion
arriving before start acknowledgment is reconciled against the same operation
and applied as ordered facts, or marked uncertain when identity is unproven.
A terminal attempt cannot be resurrected by a late event.

Disconnect first changes health. Interrupted attempts can retain reservations
if external execution may still be writing. Release resources only after
writer disposition and outstanding actions are settled.

Checkpoint-based pause stores a resumable artifact and settles the current
attempt. Resuming admits a new attempt against verified checkpoint inputs and
current authority. A runtime that provides checkpointing must prove both
artifact durability and writer quiescence; otherwise pause uses supported
cancellation or waits for bounded execution to finish.

Reconnection to a proven live attempt preserves its ID and advances binding
generation. Retries and replacement execution receive new attempt IDs and
input snapshots. Old callbacks can contribute historical evidence but cannot
mutate a replacement.

Shared provider processes are runtime resources. Cancellation targets the
authorized attempt/session; process-wide termination requires proof that every
affected session is in the same authorized stop scope.

## 8. Workspaces, isolated execution, and integration

```text
Workspace
  id / workId / projectId / executionLocation
  repositoryRefs[] / resourceOwnership
  integrationHead / currentCandidateId?
  resourceState / retentionPolicy

ExecutionSandbox
  id / workspaceId / attemptId?
  baseManifest / filesystemLocator
  mode: ReadOnly | ExclusiveWrite
  ownership: Managed | Attached
  status / writerReservationId?

PresentationBinding
  id / consoleSessionId? / workId? / resourceId?
  kind: Console | Shell | Artifact
  applicationInstance / window / surface / pane?
  generation / observedAt

ShellSession
  id / ownerPrincipal / workId? / workspaceId?
  executionLocation / cwd / shellProfile
  processIdentity / foregroundExecutionRef?
  liveness: Running | Exited | Unknown

ShellInputLease
  shellSessionId / generation
  owner: ConsoleView | ExecutionBinding
  ownerId / state
```

A workspace is a durable resource set. It may contain a repository, multiple
repositories explicitly approved for the work, reports, and data inputs.
Question-oriented work can use an artifact/context workspace without a Git
repository.

Repository identity uses a local record, execution location, verified Git
common-dir locator, and filesystem identity where available. Remote URLs are
descriptive references. Canonicalize locators before ownership checks; separate
clones of the same remote remain separate local resources.

### 8.1 Parallel work

Each write attempt receives an isolated sandbox from a pinned input baseline.
Managed writer reservations are exclusive per physical writable resource,
including aliases to the same worktree. Independent read-only inputs may be
shared through verified immutable snapshots.

Attempts submit artifact manifests, patches or commit identities, and validation
results. They do not independently advance the work's integration head.

The integration manager serializes changes to that head:

1. Verify candidate input identities and output contracts.
2. Apply compatible outputs in a separate integration sandbox.
3. Resolve or expose conflicts using an authorized integration task.
4. Execute required gates on the combined snapshot.
5. Compare-and-swap the integration head only if its parent and relevant inputs
   are still current.
6. Publish a new immutable delivery candidate when its criteria are met.

Passing tests in two isolated branches does not establish that their combined
result passed. A concurrent integration update requires reevaluation rather
than silently publishing a result validated against a different parent.

### 8.2 Human participation

The workbench routes conversation, evidence inspection, terminal output, and
editing through the selected work and resource identity.

Human edit takeover is a tracked operation:

1. Identify the resource and dependent writer scopes.
2. Hold new conflicting dispatches and settle relevant active writers.
3. Verify resource identity and current content.
4. Transfer the writer reservation to the human workspace.
5. Record the resulting changes as a candidate artifact on handback.
6. Invalidate affected checks and resume authorized work against new inputs.

Independent tasks can continue if their inputs and write scopes are unaffected.
External modifications detected outside managed ownership are recorded as
drift and require reconciliation. A reservation coordinates managed actors;
it is not an assertion that unrelated applications cannot access the filesystem.

Associating an existing directory or session records provenance and actual
resource state. Managed execution begins after explicit authority and writer
handoff; unobserved prior activity is not retrospectively counted as an attempt.

Viewing a candidate diff or an existing session's output does not request edit
takeover. "Let me edit" and "My changes are ready" map to takeover and handback,
including the necessary internal steps and a visible change summary. Handback
does not require closing every shell: a presentation can remain open for
inspection after its input lease is released. Any foreground program that may
still write must be identified and settled or kept in an explicitly separate
scope. The user sees the actual remaining action rather than a mandatory
hide/close/handback command sequence.

### 8.3 Presentation and lifetime

Selecting work changes the window console's logical view. It does not create
or reparent a per-work console process. The optional shell host presents
registered execution resources as tabs/panes and may be entirely hidden.

`shell.open` validates cwd, ownership, resource reservations, and any required
writer handoff before starting an owned shell. It can create a work-associated
manual session or, with an explicit cwd and human authorization, an unassociated
manual session. Work association is then an explicit validated attach operation.

`shell.show` binds an existing session to a compatible presentation, or offers
an explicit create action if none exists. Open/show retain console input focus
by default; `shell.focus` requests interactive focus and the applicable input
lease. `shell.hide` changes only the
requesting console's layout and focus; it sends no exit, cancellation, work
pause, or writer-handback operation. It does not end the shell input/output
stream or revoke a work's authority.

`shell.close` is a separate tracked operation for an exact session, with
foreground-program impact, authorization, and writer settlement checks.
The console rendering channel is not listed as a user shell and cannot be
targeted by a shell-close command.

Each interactive shell has one input-lease owner at a time. Multiple views can
observe output, but input requires an acknowledged current lease. Focus or
presentation transfer checks that lease's generation; an old view cannot inject
input after transfer. Taking over an agent-owned interactive resource also
requires its execution/writer handoff. Hiding a human shell returns UI focus
to the console while any manual writer reservation remains explicit until
handback.

Changing a physical shell tab changes the inspected shell, not the console's
selected work. Both contexts are shown when they differ. Domain actions use
their captured work/attempt/decision identities rather than a late lookup of
the active tab.

Closing a window detaches its console and shell presentations. Resource and
execution dispositions are determined by the runtime and explicit close
operations. Window moves update presentation locators and input leases, not
work identity. Creating a terminal surface remains a correlated operation;
an ambiguous result requires discovery or explicit association before retry.

### 8.4 Console context and client state

```text
ConsoleSession
  id / hostKind / hostIdentity / windowIdentity?
  selectedWorkId? / selectedProjectId? / page
  contextVersion
  layout: AgentFocus | AgentWithShell
  selectedShellPresentationId?
  globalConversationId / globalDraftRef
  workConversationRefs / draftsAndReadingPositionsByWork
  taskListNavigation / detailNavigationRefsByWork
```

Enforce at most one live console presentation per window identity. A recreated
client reconnects through the verified window/console association; concurrent
startup cannot create two competing work entry points.

The default presentation is a task list. Opening an existing work binds the
client to that work's durable conversation, observed execution state and
continuation controls; opening it does not start or resume execution.
Global conversation remains available for new goals and cross-work questions.
Immutable existing console/conversation registrations are not repurposed across
works or rebound between global and work scopes. These wire bindings do not
themselves qualify the verified host/window association above.

Work selection identifies the conversation target, not execution authority.
Each client retains separate unsent drafts and reading positions per work and
for global conversation. Another window/tab can open the same persisted work
conversation without sharing the first client's unsent draft or acquiring
another writer. Persisted content follows access, retention and deletion policy.

Command submission atomically captures resolved target IDs, current object
versions, and the submitted input. Subsequent navigation cannot redirect that
command, its answer application, or its output. Background changes update their
own work projections even when another work is selected.

Human-readable selectors and action cards are projections over these captured
identities, not new domain objects or authority. Default work presentation
shows goal, observed status, recent activity, its conversation and continuation
actions. Genuine human questions and inspectable delivery remain accessible.
Plans, actual locations and evidence are available
on demand; aggregate IDs, invocation IDs, revisions and raw protocol objects
belong in explicit diagnostics. Do not remove IDs from the authoritative
records or rewrite user content, real paths or evidence to hide them.

Ordinary creation, selection, approval, typed question answers and delivery
acceptance must not require users to enter those internal IDs. Duplicate
display names require a readable disambiguation and explicit selection.
Question forms retain the declared response types; unsupported schema is an
explicit capability limit, not an implicit approval or success-shaped default.
Readable confirmation still freezes the exact target, payload, expected
versions and command identity. Inspection itself never authorizes acceptance
or acquires a writer.

Presentation recovery preserves each conversation's draft, caret, selection and
separate chat/detail navigation, marks disconnected observations stale, and refreshes authorized
projections before treating them as current. A recovered transport cannot
approve a frozen preview or blindly replay an uncertain mutation.

The keyboard-first presentation defaults to tasks: select with arrows and open
with Enter. Navigation labels are not three copies of the same F4 shortcut;
the action-menu shortcut is described once. Task chat shares the agent pane's
presentation components, while its data remains service-owned. Necessary
questions, explicit approvals, failures and unknown outcomes remain visible.
Grouping and detail visibility are client state, not new execution authority.
Collapsing navigation or opening details must not change the selected work or
discard its editor and reading state. Visible actions retain deterministic
keyboard access; input submission depends on explicit focus, while an active
confirmation always retains its captured target and confirmation requirement.

Render hierarchy does not authorize invented facts. Progress and next-owner
labels require observed state; cached observations are visibly stale.
Concurrency versions are not content editions. Source-project paths cannot
substitute for an unknown execution directory. Important authorization and
risk information remains readable in the relevant confirmation even when
technical diagnostics and optional detail panels are collapsed.

Continuing a work is distinct from inspecting it and from setting the legacy
`desiredAdvancement` flag. A live execution is attached to, not duplicated.
An expired or disconnected execution requires reconciliation and proven writer
settlement before replacement. A saved primary coordinator session is loaded
with the matching provider, working directory and fresh work-tool binding.
Missing provider resume support or unavailable history is an explicit failure;
starting a new session from recorded context requires a separate human choice.
Opening another task tab remains a client of this same service-owned session,
not an independent provider process with competing workspace authority.

### 8.5 Conversation and streaming without a UI dependency

The service owns semantic conversation items, message/part identities,
tool-call state, plan updates, pending decisions, and delivery references.
Conversations are work-scoped or console-scoped for global planning/queries.
A single console can present any authorized work conversation; provider
sessions retain their own distinct identities.

Normalized events include text deltas, tool updates, plan changes, decision
creation/application, delivery changes, and runtime health. Each event carries
the relevant conversation, work, attempt, message/part, and source identity.
Durable event cursors refer to committed state. Stream reconnection can replay
from a cursor or return a snapshot-required response if that cursor is no longer
available.

Domain/conversation events do not contain keyboard, mouse, terminal dimensions,
XAML objects, or renderer-specific widgets. TUI input and layout state belong
to the client. Message aggregation and authoritative decision/result state
continue without a TUI process. All clients use structured events rather than
parsing ANSI output to reconstruct business state.

## 9. Context, evidence, and project knowledge

A `ContextSnapshot` includes:

- work and plan revisions and the effective authority reference;
- selected project context and approved model/data destinations;
- pinned dependency artifacts and acceptance/gate references;
- workspace baseline and relevant human decisions;
- selected user-provided material and its provenance.

Snapshots are bounded manifests with content-addressed references. Data access
is checked at use time as well as at snapshot creation. Credentials and bearer
capabilities are referenced through runtime secret mechanisms, not copied into
work history or prompts by default.

When a qualified adapter must replace a runtime or renew provider context,
the system reconstructs permitted inputs from these records, retains the work
identity and effective decisions, settles the old execution and obtains the
new attempt's input acknowledgment. A provider summary/compaction alone is
not proof that scope, evidence or pending side effects survived. Unsupported
continuation is an explicit blocker, not an instruction to the user to relay
context followed by an "autonomous" completion claim. Existing-resource
adoption and recovery still require their own qualification evidence.

An `Artifact` records kind, content digest, locator, producing attempt or
human, relevant baselines, capture time, availability, and retention policy.
Mutable external content is captured or verified against its declared identity
before it can satisfy a gate or final criterion.

Project knowledge has explicit scope, source, revision, and approval.
Promoting an accepted result into reusable context is an explicit action.
Updates affect new snapshots; revoked access or invalidated knowledge blocks
new use and triggers impact assessment for dependent work.

Deletion removes service-owned content and derived summaries and leaves
non-sensitive tombstones where historical references are necessary.
Provider-held copies and backups have separately disclosed retention/deletion
policies. Metadata-only audit records must not retain deleted private bodies.

## 10. Gates, delivery, acceptance, and publication

```text
GateResult
  id / gateDefinitionRevision
  inputManifest / evaluatorIdentity
  result: Passed | Failed | Inconclusive
  evidenceRefs[] / evaluatedAt

DeliveryCandidate
  id / workId / specRevision / planRevision
  integrationManifest / artifactRefs[]
  criterionMappings[] / gateResultRefs[]
  knownLimitations[]
  status: Proposed | Accepted | Rejected | Superseded

Acceptance
  id / candidateId / specRevision
  actor / evidenceManifest / acceptedAt

Publication
  id / acceptanceId / requirementId?
  target / contentManifest / approvalRef
  operationId / status / externalReceipt?
```

A gate definition specifies a predicate, required evidence, evaluator authority,
and freshness/version requirements. Deterministic checks and explicit human
checkpoints can satisfy declared gates. Advisory model assessments remain
advisory unless the specification explicitly defines that assessment itself
as the requested output; they cannot impersonate a test execution or user
acceptance.
`Inconclusive` leaves a required gate unsatisfied and exposes its missing
evidence or evaluator limitation. Recovery can obtain evidence, reevaluate, or
request an explicit specification/gate-policy revision. Final human acceptance
is a separate action, not a prerequisite gate that depends on itself.

A work can have many candidate versions and at most one current proposed
candidate. Creating a replacement supersedes the old proposal and its review.
Any relevant input or output change invalidates candidate currency.

Human acceptance requires:

- the current specification and candidate;
- all required criteria mapped to available, matching evidence;
- required technical and human gates satisfied;
- no unresolved obligation relevant to accepting this result, excluding the
  review being resolved in this transaction;
- no active or uncertain managed writer that can alter the result scope;
- no pending scope cancellation or incompatible revision.

The transaction records acceptance and resolves the review. If the specification
requires external publication, the work remains active with a `Delivering`
projection until authorized required receipts are verified. Otherwise all
required delivery conditions are satisfied and it can become completed.
After acceptance, new execution that could alter the accepted result requires
an approved revision. Remaining authorized settlement and publication steps
operate on the fixed accepted manifest. The UI can show `Finalizing` while
non-result-changing execution is being settled before completion.

Optional publication after completion has its own operation and status.
External issue/PR events update their typed references; their ability to change
work completion is governed by the specification, not inferred from a URL.

Publication approval binds exact content, destination, base revision, visibility,
and action. New content or changed destination invalidates that approval.
Unknown external results are reconciled by operation identity before retry.

Revision feedback references candidate criteria/evidence. The human may approve
`Revise and advance`, which rejects/supersedes the candidate and authorizes
bounded corrective work. Scope expansion follows the specification-change path.

### 10.1 Future OS-state outcome qualification

The current protocol's local code/report delivery remains unchanged. A future
adapter that promises an OS/application state outcome must first define:

- exact target resource identity and execution location;
- approved operations, preconditions, expected postconditions and ownership;
- before/after observations, operation receipts and verification freshness;
- retention of result evidence and effects on resources outside the target;
- cancellation, retry/reconciliation and rollback capability or disclosed
  irreversibility.

Acceptance must check the actual target against the approved postcondition,
not just a successful command exit or generated report. Changed, replaced,
unavailable or stale target state cannot retain a valid acceptance proof.
Unknown effects remain unresolved until observed; replay is not a substitute
for reconciliation. If an adapter cannot enforce the promised boundary or
verify the goal, it is ineligible for that contract. These are extension
requirements, not an implicit new entity, operation, delivery enum or arbitrary
system-administration permission in v1.

## 11. Decisions and reliable application

```text
DecisionRequest
  id / workId / scopeRef
  specRevision / relevantInputManifest
  question / options[] / responseSchema
  impactDescriptions[] / recommendationProvenance?
  target: AttemptInput | SpecificationChange | GrantChange | GateApproval | Repair
  blocksScope[]
  status: Open | Answered | Resolved | Superseded | Cancelled

DecisionAnswer
  id / requestId / requestRevision
  actor / value / answeredAt

DecisionApplication
  id / answerId / targetIdentity
  expectedVersions / bindingGeneration?
  status: Pending | Acknowledged | Unknown | Failed | Superseded
  receipt?
```

Persist the request before presenting it. Saving an answer validates current
request revision and response shape, then atomically records an application
intent. A second distinct answer to that revision returns a conflict.
Idempotent resubmission of the same answer returns its recorded result.

For attempt input, acknowledgment means the runtime accepted the answer into
the specified continuation. For specification/grant changes, the service
records committed revisions and any still-pending control operations. The UI
distinguishes saved input, applied decision, and execution still settling.
For GateApproval, the request pins its gate definition, result/input manifest
and criterion. Applying the authorized human response records the matching gate
evaluation; it neither impersonates a worker callback nor accepts final delivery.

When a request's relevant inputs change, supersede it. Its saved answer remains
historical and can be explicitly reconsidered in a new request or context
snapshot. It cannot silently authorize a different task, capability, or scope.

Lost acknowledgment creates `Unknown` application. Query or deduplicate by
application ID where supported; otherwise present recovery rather than repeat
an uncertain action. Rebinding a live attempt requires a verified mapping of
its pending request and generation. Ended attempts cannot receive old callback
answers through a replacement session.

Global inbox and in-workbench decisions use the same records and actions.
Dependent tasks wait; unrelated authorized scopes continue.

## 12. State projection and attention

State dimensions remain independent:

| Dimension | Representation |
|---|---|
| Work lifecycle | Draft, active, completed, cancelled |
| Desired advancement | Advance, hold, cancel |
| Actual execution | Attempts, writer reservations, pending controls, resource availability |
| Readiness | Task dependencies, gates, authority, and capacity |
| Delivery | Candidate, acceptance, required publication receipts |
| Obligations | Decisions, reviews, blockers, repairs, resource requests |
| Health | Confirmed, disconnected, unknown; source and observation time |
| Visibility | Archive, seen, snooze, notification preferences |

The primary card group is computed in this order:

1. Completed or cancelled.
2. Needs a human decision or repair.
3. Needs acceptance of a current candidate.
4. Pausing/cancelling, or paused after confirmed quiescence.
5. Delivering or finalizing an accepted result.
6. Advancing with confirmed execution.
7. Queued for resources.
8. Blocked by required inputs, gates, or authority.
9. Draft or ready for initial authorization.

All outstanding obligations remain visible regardless of grouping.
`Paused` requires no active advancement attempt or uncertain writer in its scope.
`Hold` immediately prevents new conflicting admission; a pause request and a
confirmed pause are different facts.
It also holds undispatched publication and other advancement side effects.
Inspection, decision capture, cancellation, and recovery can proceed under
their own scoped authority while advancement is held.

Runtime health uses reconciliation or an advertised heartbeat. A disconnected
endpoint is shown immediately; absence of a fresh reconciliation for 30 seconds
marks that binding unknown. Tool inactivity alone is not proof of failure.
Product summaries show their source and as-of version; generated prose cannot
overwrite domain facts.

### 12.1 Attention lifecycle

An `AttentionItem` references a subject, obligation revision, kind, severity,
required action, and resolution predicate. Its lifecycle is
`Open | Resolved | Superseded`; seen and snoozed are presentation properties.

| Subject | Resolution |
|---|---|
| Decision | Applied acknowledgment or request supersession/cancellation |
| Candidate review | Acceptance, rejection, or supersession |
| Dependency block | Its exact predicate becomes satisfied or its scope is revised |
| Resource/authority request | Capacity or authorization established, or affected work cancelled |
| Repair | Observed repair completion and settled relevant uncertainty |
| Informational completion | User acknowledgment affects the notification, not acceptance |

Attention creation/resolution is committed with its subject transition.
Deduplicate by subject and obligation revision, not message wording. Related
decisions can share a presentation bundle while retaining individual identities.

Notification policy uses severity, user priority, blocked scope, and quiet
hours. One logical notification identity is reused for retries. Ambiguous
delivery on a nondeduplicating channel is not blindly repeated; the durable
inbox remains authoritative. Desktop payloads default to generic event kinds;
sensitive details are shown only after entering the authorized work context.

## 13. Typed commands and event authority

The TUI, CLI, MCP clients, planner, runtime adapters, and publication connectors
use typed APIs. Actor authority is authenticated and checked against the
current work scope.

| Command family | Authority and meaning |
|---|---|
| Capture intent / propose brief | User request under approved planning/data authority |
| Approve and start work | Human approval of specification and execution grant |
| Propose/revise plan | Planner may change execution details within the effective grant |
| Admit task / start attempt | Scheduler after atomic readiness and resource checks |
| Submit artifact / report observation | Bound actor for the exact attempt/input versions |
| Evaluate gate | Authorized evaluator against a pinned manifest |
| Request/answer/apply decision | Scoped requester, human answer, version-checked application |
| Take over / hand back resource | Human request plus confirmed writer transfer |
| Hold / resume / cancel | Authorized work owner; asynchronous controls report actual settlement |
| Accept / revise delivery | Human decision on the current candidate |
| Publish / clean resources | Specific authorization and tracked external effects |
| Archive / reopen | Validated lifecycle change with history retained |
| Open/attach/close shell | Exact owned session/resource, cwd, process and writer checks |
| Select work / show/hide/focus shell | Explicit console/host target and presentation/input-lease checks |

```text
Command
  commandId / actor / targetId / expectedVersion
  type / payload

Observation
  observationId / sourceInstance / sourceEpoch / sequence?
  attemptId? / bindingGeneration? / operationId?
  occurredAt / receivedAt / type / payload

DomainEvent
  eventId / aggregateId / aggregateVersion
  correlationId / causationId
  actorOrSource / recordedAt / type / payload
```

The service rejects stale generations, obsolete grants, and conflicting
versions for new advancement. Correlated settlement observations from revoked
execution can still record actual exit, cancellation, or already-dispatched
effects; they cannot restore dispatch authority or publish a current result.
Source timestamps do not establish
ordering. Unordered native observations require reconciliation or an unknown
state rather than an invented authoritative sequence.

Same command ID and payload returns the original operation/result. Reusing
an ID with different content is an error. Concurrent distinct commands with
the same expected version return one accepted mutation and an explicit conflict.

### 13.1 Work lifecycle commands

| Command | Preconditions | Immediate state and settlement |
|---|---|---|
| Start | Draft, confirmed specification/grant and valid resource plan | Active/advance; tracked provisioning and task admission |
| Revise | Draft or active; authorized change proposal | New revisions; hold affected dispatches and settle incompatible effects |
| Hold | Active | Desired hold; actual pausing until scoped advancement settles |
| Resume | Active/hold; valid current authority and reconciled resources | Desired advance; admit only ready authorized work |
| Take over | Authorized human and identified resource scope | Hold conflicting writers; transfer only after verified settlement |
| Accept | Current candidate and all acceptance guards satisfied | Record acceptance; publish/finalize as required by the specification |
| Publish | Exact accepted content and destination approval | Tracked operation; complete required delivery on verified receipt |
| Cancel | Draft or active | Desired cancel; cancelled lifecycle after all required settlement |
| Reopen | Completed/cancelled with no unsettled conflicting effects | New active specification; clear current acceptance, retain historical records |
| Archive | Completed/cancelled and all operations/obligations settled | Visibility change with resources retained |
| Clean resources | Exact manifest, approval, and resource safety checks | Tracked deletion/retention steps with per-resource receipts |

Reopen establishes the desired advancement explicitly. Reusing a prior grant
requires current policy validity and human confirmation for the new scope.
Externally effectful commands return an operation identity and pending state
until their effect is confirmed. An authorization or input error returns its
specific reason and an actionable correction.

### 13.2 One command registry, several clients

```text
CommandDescriptor
  name / aliases / operation
  scope: Global | Project | Work | Object | ConsoleView
  argumentSchema / resultSchema
  completionProvider / help
  authorizationPolicy / confirmationPolicy
  requiredHostCapabilities[]
  kind: Read | Mutation | Presentation
```

The product specification's slash command catalog is the required human-facing
surface. Completion, help, selectors, operation cards, structured CLI commands,
and MCP tool schemas derive from the same operation contracts. Renderers may
format results differently but cannot redefine domain meaning.

Examples:

| TUI | Structured CLI | Domain meaning |
|---|---|---|
| `/work list` | `wta work list --json` | Query authorized work |
| `/work show --work <id>` | `wta work show <id> --json` | Read the current work snapshot |
| `/task list --work <id>` | `wta task list --work <id> --json` | List authorized tasks and their execution/result projections |
| `/task show <id>` | `wta task show <id> --json` | task.get: inspect dispatch, context requests, submissions and reviews |
| `/result show <id>` | `wta result show <id> --json` | Read one immutable task result with its evaluation history |
| `/work start --work <id>` | `wta work start <id> --input-json <request> --json` | Validate specification, grant and expected version; start tracked advancement |
| `/decision answer <id>` | `wta decision answer <id> --input-json <request> --json` | Versioned answer by an authorized actor |
| `/review accept <id>` | `wta review accept <id> --input-json <request> --json` | Accept exact candidate under required human authority |
| `/work events --work <id>` | `wta work events <id> --after <cursor> --jsonl` | Subscribe to authorized structured work events |
| `/shell hide` | `wta shell hide --view <id> --json` | Hide shell presentation in an explicit compatible console view |

In CLI examples `<request>` denotes a JSON request file; `-` reads it from
stdin. The request schema includes required payload fields, expected versions,
command identity, and applicable explicit approval references. Credential
material is supplied through authenticated runtime channels, not help examples
or arbitrary self-asserted actor fields.

Presentation commands require a view/host context. A headless client requesting
one without a compatible target receives `unsupported` with the missing
capability; work operations remain available. CLI callers never inherit an
unrelated window's currently selected work.

TUI context defaults are resolved once at submission. Explicit `--work` and
object IDs take precedence and are displayed in the preview. Global/context
commands require the descriptor's own target rather than opportunistic use of
an active shell. The slash parser handles quoted arguments and the documented
literal-slash escape; invalid commands return a parser error with input intact.

### 13.3 Noninteractive and agentic operation

`wta ui` is the TUI client entry, launched directly by a window host or attached
to an ordinary supported TTY. Worker/session lifetimes belong to the runtime
host, not the TUI client process.

Noninteractive clients use structured operations. JSON output goes to stdout,
diagnostics to stderr, and event subscriptions emit JSONL with resumable
cursors. Result envelopes distinguish `ok`, `pending`, `needs_input`,
`conflict`, `unsupported`, and `error`, with operation/object IDs and current
versions where applicable. `pending` means the request was accepted, not that
its side effects completed.

`needs_input`, conflicts, unsupported operations, and errors use documented
non-success exit codes. Missing required human input returns a persistent
decision/approval reference and schema; it never opens an invisible prompt or
silently approves the action. Event cursor expiry requests a new snapshot and
fresh cursor explicitly.

Agent clients receive scoped capabilities for work discovery, proposal,
execution, and result submission as granted. Access to the CLI/MCP transport
does not itself confer human decision, acceptance, or publication authority.
Every mutation is authorized by the service regardless of the renderer or
transport that submitted it.

### 13.4 Reporting and event delivery

Normal collaboration is push-driven. Workers report through work-scoped MCP
tool requests; runtime adapters and check executors submit observations through
typed service calls. The work service records accepted input, updates affected
state, and emits the corresponding DomainEvents. A tool receipt means the
report/submission was recorded, not that its requested outcome was accepted.

| Producer | Input | Service notification / effect |
|---|---|---|
| Runtime adapter | Correlated start, tool activity, prompt completion, process exit | Attempt/runtime observations; completion interpreted with outstanding context and submission state |
| Worker | task.report_progress | ProgressReported; explicit coordinationRequest also queues CoordinationRequested |
| Worker | task.request_context | ContextRequested; route internally, or link a consequential human decision |
| Responder | task.answer_context | ContextAnswered; queue delivery to the bound worker continuation |
| Worker | result.submit | ResultSubmitted; schedule declared checks/review |
| Check executor / reviewer | gate.submit / review.submit | GateEvaluated / ReviewRecorded; evaluate task acceptance or rework |
| Work service | Successful acceptance predicates | TaskResultAccepted; unblock matching declared dependencies |
| Work service | Recorded rework disposition | ResultChangesRequested / ResultRejected; queue bounded coordination |
| Work service | Current combined delivery / required human choice | DeliveryProposed / DecisionRequested; update user inbox |

Consumers receive events only after their authoritative state is committed.
The local service uses an internal event dispatcher and long-lived client
subscriptions; the Windows prototype uses framed JSON messages over a named
pipe. No external message-broker service is required. A frame is a four-byte
little-endian UTF-8 payload length followed by a JSON message; the v1 prototype
rejects frames over 1 MiB, with large content referenced as artifacts.
MCP carries agent tool calls and ACP carries runtime prompts/events; neither is
assumed to wake an idle model from an arbitrary notification.

The v1 client exchange consists of a version-negotiated Hello, Request/Response
messages using the command/result envelopes, and Event messages for a returned
subscriptionId. One connection can multiplex requests and subscriptions by
their IDs. Unsupported protocol versions return an explicit incompatibility
response. Subscription scope includes work IDs or the authorized work list.
Reads/subscriptions omit mutation-only expected-version fields; mutation
requests retain the version checks of their operation contract.

`subscribe` returns a snapshot and its committed cursor from one consistent
read, then events strictly after that cursor. Reconnect with `afterCursor`
replays committed events; an expired cursor returns snapshot-required. Event
IDs and aggregate versions identify duplicate or stale deliveries. A slow
consumer that exceeds the configured bounded queue receives resync-required
(or reconnects after transport loss), rather than silently missing events.
CLI `work events --jsonl` reads the same subscription; repeated `work show`
calls are not the normal notification mechanism.

Conversation deltas include messageId, partId and ordered chunk identity.
Only committed chunks receive a durable resume cursor; batched chunk persistence
may delay display but cannot claim a cursor for unsaved text. Replays deduplicate
chunks and explicitly mark completed/interrupted messages.

Polling is confined to external adapters without push support and reconciliation.
Heartbeats establish availability, not semantic progress. Neither the coordinator
nor a worker periodically invokes a model to ask whether another task is done.

### 13.5 Coordinator activation and worker continuation

The coordinator driver is ordinary service logic, not a continuously reasoning
agent. Each work has a pending-event queue and at most one active coordination
turn across all consoles. Console-scoped intake uses a separate queue for new
intent and immediate questions; global intake resolves work targets explicitly.

```text
CoordinationTurn
  id / workId? / consoleConversationId?
  triggerEventIds[] / snapshotVersion / inputManifest
  runtimeInvocationId / submittedCommandIds[]
  state: Queued | Running | Completed | Blocked | Failed
  outcome: ActionsRecorded | Answered | WaitingOnRecordedSubject | NoActionNeeded
```

The runtime starts a metered ACP invocation with the captured work snapshot,
coalesced relevant events and permitted operation schemas. The coordinator
returns an answer or submits proposed actions through work tools. Domain changes
occur only when those actions are applied by the service.

| Event class | Handling |
|---|---|
| Text chunks, tool activity, ordinary progress | Refresh the appropriate view; do not start a coordination turn |
| Declared dependency becomes ready | Scheduler advances the existing plan without asking a model |
| Initial approved work needs a plan; new user intent or CoordinationRequested | Queue coordination with explicit targets and the reported reason |
| Internal context request | Resolve from recorded facts or queue focused coordination |
| Submission ready for declared checks | Schedule check/reviewer directly; no coordinator call just to forward a result |
| Changes requested, rejected result, declined task or missing submission | Queue a bounded response with the original contract and evidence |
| Applied human decision | Resume its recorded continuation or queue affected replanning |
| Human judgment required | Persist inbox item; wait for input, not a coordinator self-approval |

An active coordinator is not interrupted for each arrival. Events queue and
coalesce by affected subject/revision; after the turn, the driver reevaluates
pending conditions against current state before starting another. Multiple
windows cannot start competing turns for the same work. A turn's own output
does not reflexively wake that same turn again. Empty/failed turns expose a
coordination blocker or use the finite retry policy, not a busy loop.

Coordinator invocations use a separate, bounded planning lane (one turn per work)
and the planning budget. Worker concurrency slots are for execution attempts;
all workers waiting for context must not prevent the coordinator from answering.
Check/reviewer execution uses declared execution capacity, released by settled
workers, rather than taking an unmetered hidden lane.

Worker continuation is a separate runtime dispatcher. It queues answers until
the bound ACP session is idle or advertises a supported continuation boundary;
it does not concurrently inject a new prompt into an active turn. A yielded
WaitingForContext attempt is not terminal. Ended attempts require new attempt
identity even when the adapter reuses the provider session.

### 13.6 Human notification contract

The Console subscribes to the same committed work state. Background progress
updates cards, internal correction stays in the work timeline, and decisions
and delivery populate the inbox. None steals input focus or changes selected
work. An inbox action can temporarily inspect another work and return to the
originating view, draft and reading position.

Each user item explains the work, reason, evidence, proposed action and impact.
Context questions that coordination can resolve are not user notifications.
Notification delivery and model activation are distinct: showing a badge does
not invoke the coordinator, and a coordinator turn need not interrupt the user.

## 14. Persistence and service ownership

Use a single-writer service per local user/application state root.
The service owns authoritative work state, scheduler admission, migrations,
operation reconciliation, and outbox dispatch.

Store domain state in SQLite below the shared runtime state-path resolver,
for example `agent-center\work.db` relative to that root. Enable foreign keys,
WAL, and durable commits (`synchronous=FULL`). A per-state-root OS lifetime lock
and durable service epoch establish a single active authority.

Local clients authenticate through an access-controlled endpoint. Endpoint
names, work IDs, or pipe identifiers alone do not prove authorization.
New service instances reconcile outstanding effects before admitting
replacement execution. Epoch fencing rejects obsolete callbacks; it does not
terminate an already-dispatched external action.

One transaction includes:

- expected-version checks and domain state changes;
- current plan/grant validation and resource uniqueness checks;
- command deduplication result and audit/domain events;
- obligation and attention updates;
- operation intents and transactional outbox entries.

Work-scoped acceptance, revision, and admission changes advance the work
version. Physical resources have their own transactional writer constraints.
Seen/snooze updates do not create artificial conflicts with goal editing.

The store contains current state and an audit trail; read models and summaries
are rebuildable. Outbox delivery is at least once, with recipient deduplication
where supported. Exactly-once effects across the database and external tools
are not assumed.

Schema migration checks and transaction-consistent backups are mandatory.
Corrupt or incompatible storage blocks mutations with a recovery explanation.
Retention and deletion cover work content, artifacts, context, and backups
according to their disclosed policies.

## 15. Operations and reconciliation

```text
Operation
  id / commandId / workId? / projectId?
  kind / effectiveAuthorization / serviceEpoch
  expectedVersions / desiredState / targetResourceIds
  status: Pending | Running | Succeeded | Failed | RepairRequired
  steps[{ id, intent, state, externalReceipt?, error? }]
```

Step states are `Planned | Dispatched | Confirmed | Failed | Unknown`.
Conversation/planning operations can have project scope without a WorkItem;
work-scoped effects require their workId.
Persist intent and expected resource identity before dispatch. Save receipts
before dependent steps. Revalidate authority and input conditions before each
new dispatch, while treating already-dispatched effects as reconciliation work.

Managed resource creation uses pre-recorded locators and correlation identity.
Git resources are verified by repository/worktree registration and content
identity. Terminal and provider start operations require a discoverable
correlation contract across the dispatch/receipt crash window.

An unknown result is not a failed operation safe to repeat. Repair either
discovers the intended resource/effect, establishes its absence, or requests a
specific human decision backed by inspection.

### 15.1 Recovery matrix

| Failure/race | Required behavior |
|---|---|
| Console window closes | Detach its console/shell presentations; service and execution retain their lifetimes |
| Shell region hidden | Change local layout/focus only; retain shell liveness and manual writer obligations |
| Work or shell selection changes during submission | Apply the command to its captured identities and version, not the new selection |
| Service restarts after intent commit | Recover operation and reconcile target before dispatch |
| Sandbox created before receipt save | Verify recorded identity and adopt it, or expose mismatch |
| Terminal surface created before binding save | Discover by operation identity; require explicit association if ambiguous |
| Attempt start sent, acknowledgment missing | Reserve resource; query intended attempt; no blind prompt replay |
| Agent/runtime disconnects | Update health, establish whether execution survives, reconnect only to proven identity |
| Interrupted attempt may still write | Retain writer and capacity reservations until disposition is known |
| Two attempts finish against the same integration parent | Serialize integration; losing parent comparison triggers reevaluation |
| Human takeover races with active write | Hold conflicting dispatches and await writer settlement before transfer |
| Old attempt reports after replacement | Preserve historical evidence; reject mutation of current execution/output |
| Decision saved before application | Recover exactly scoped application intent |
| Decision applied but receipt lost | Query/deduplicate by application ID or show unknown application |
| Scope changes with active tasks | Revoke affected dispatch authority; settle dispatched effects; preserve approved unaffected tasks |
| Upstream evidence revoked | Invalidate dependent gates/candidates; hold affected consumers |
| Acceptance committed before publication | Recover approved exact-content publication operation |
| Publication succeeds before receipt saved | Query external correlation before issuing another publication |
| Resource paths or branch identity drift | Inspect and explicitly rebind/adopt; preserve user content |
| Cleanup outcome is ambiguous | Reconcile each named resource and retain operation state |
| Machine returns after shutdown | Restore durable work; establish actual execution/resource state before rescheduling |

Reservations survive service crashes. Lease expiry alone does not prove an
external writer stopped. Human repair records the actor and inspected evidence;
clearing a status flag is not a substitute for establishing resource disposition.

## 16. Completion, archive, and cleanup

Completion requires a current accepted candidate, satisfied required
publication receipts, all completion-relevant obligations settled, and all
managed execution for that work settled or explicitly cancelled.

Cancellation records desired cancellation immediately and stops new admission.
The work becomes cancelled only after execution settlement; unresolved resource
uncertainty remains visible. A natural finish racing with cancellation does not
silently restore authorization or accept its result.

Reopen preserves prior acceptance/publication history and creates a new active
specification revision. Archive changes visibility for ended, settled work.
It does not delete resources.

Cleanup is a separate operation over an exact user-approved resource manifest.
Managed and attached ownership are distinguished. Before deletion, validate
identity, live writers, uncommitted data, unpublished results, evidence retention,
and dependent references. Preserve selected immutable delivery artifacts before
removing disposable execution resources.

Destructive approval names the resources and any data-loss implications.
Unknown cleanup results are reconciled individually. Project-context deletion
and filesystem cleanup have separate scopes and audit records.

## 17. Implementation ownership

| Boundary | Required responsibility |
|---|---|
| Window XAML host | One independent Console terminal host, optional sibling shell area, agent-focus startup, layout and focus return |
| Agent Console TUI | Window/TTY client, slash command completion, work selection, chat/streaming rendering, keyboard forms and projections |
| Command registry and clients | Shared schemas, help, completion, authorization metadata, CLI/MCP parity and noninteractive results |
| Work service | Specification, grants, task dispatch/results/reviews, gates, acceptance, scheduling, persistence, operations and committed event dispatch |
| Coordinator driver | Per-work event queues, metered model activation, coalescing, action outcomes and no-progress escalation |
| Runtime host | Work-scoped execution independent of client windows/processes; owned process/PTY adapters and shutdown coordination |
| WTA/ACP adapters | Capability qualification, dispatch prompts, work MCP tools, runtime observations, session multiplexing and queued continuation delivery |
| Terminal action broker / host adapters | Authorized resource actions, window or TTY presentation, input leases and correlated creation |
| Workspace/integration manager | Sandboxes, writer handoff, Git/resource identity, combined-result gates, retention |
| Artifact/context service | Versioned manifests, provenance, availability, data access and deletion |
| Publication connectors | Exact-content approval, idempotent dispatch or discovery, destination checks and receipts |

The normative [protocol](agent-center-protocol.md) closes the shared boundary
summarized in sections 6 and 13: requests, replies, typed terminal records,
evaluation order, continuation acknowledgment, coordinator finish and concrete
exit paths. Implementation derives schemas and reducers from that contract;
it does not postpone their semantics to individual adapters. Database migrations
and production recovery qualification remain separate engineering deliverables.

## 18. Acceptance cases

These cases define target behavior. Product story IDs refer to the companion
specification. Section 19 selects the current experience experiment; the full
list also covers subsequent production engineering. Agent collaboration must
use real executions and artifacts rather than scripted completion cards.

| ID | Scenario | Required result | Story |
|---|---|---|---|
| AC-01 | Start from a goal and project context | Reviewable specification/grant precedes task execution | US-01 |
| AC-02 | Five works receive interleaved observations | Correct work/attempt routing; actionable obligations remain discoverable | US-02 |
| AC-03 | Approved parallel plan becomes ready | Eligible internal tasks advance automatically within grant and capacity | US-03 |
| AC-04 | Planner requests broader files, tools, or budget | Grant check blocks dispatch and creates a specific approval request | US-03 |
| AC-05 | Two write tasks run concurrently | Separate sandboxes and exclusive physical writer identities | US-03 |
| AC-06 | Individually passing patches fail when combined | Integration gate fails; candidate not represented as verified | US-03 |
| AC-07 | Reopen the same work in several windows | One work identity; version-consistent views and resource references | US-04 |
| AC-08 | Human requests write takeover | Conflicting attempts settle before writer authority transfers | US-05 |
| AC-09 | Human hands back changed files | New input manifest; affected gates rerun; unaffected tasks retain validity | US-05 |
| AC-10 | Answer committed before delivery crash | One saved answer and a recoverable scoped application | US-06 |
| AC-11 | Answer applied, acknowledgment lost | Query/dedup or explicit unknown state; no blind duplicate continuation | US-06 |
| AC-12 | Two windows answer different values | One version-checked answer; explicit conflict for the other | US-06 |
| AC-13 | Decision changes specification and authority | New revisions and impact recorded before new affected dispatch | US-07 |
| AC-14 | Old attempt finishes after scope revision | Only proven compatible outputs carry forward; others remain historical | US-07 |
| AC-15 | Model says done or process exits successfully | No human acceptance or work completion inferred | US-08 |
| AC-16 | Evidence changes or disappears before acceptance | Acceptance blocked with exact invalid criterion/evidence | US-08 |
| AC-17 | User accepts current candidate | Acceptance binds exact candidate, criteria, and evidence manifest | US-08 |
| AC-18 | Required publication remains pending | Work shows delivering; completion awaits verified required receipt | US-08 |
| AC-19 | Publication succeeds before receipt persistence | Discover external effect before retry; no duplicate PR/report | US-12 |
| AC-20 | Accepted upstream output unlocks approved consumer | Consumer pins selected output version and schedules within its own grant | US-09 |
| AC-21 | Dependency edits create a cross-work cycle | Reject graph update atomically with a cycle explanation | US-09 |
| AC-22 | Upstream result is invalidated | Affected consumers/gates/candidates held or invalidated transitively | US-09 |
| AC-23 | Auto-retry reaches approved allowance | Stop automatic retries; expose evidence and choices | US-10 |
| AC-24 | Start acknowledgment lost | Keep reservations and reconcile; no uncorrelated duplicate attempt | US-10 |
| AC-25 | Resource created before receipt save | Adopt matching resource or require repair; repeated recovery creates none extra | US-10 |
| AC-26 | Interrupted external writer may survive | Replacement admission remains blocked until disposition established | US-10 |
| AC-27 | Hold requested during active execution | No new affected dispatch; pausing until confirmed quiescence | US-11 |
| AC-28 | Cancel races with successful completion | Cancellation intent remains authoritative; no automatic acceptance | US-11 |
| AC-29 | Shared provider process hosts independent work | Stop targets authorized session scope and preserves other work | US-11 |
| AC-30 | Capacity decreases below occupancy | Visible draining state; future admission respects new limit | US-11 |
| AC-31 | Executor can only estimate cost or stop timing | UI and grant advertise advisory constraints, not hard guarantees | US-11 |
| AC-32 | Close all presentation windows while host remains available | Authorized background execution and durable work remain available | US-10 |
| AC-33 | Machine returns after sleep/shutdown | Reconcile actual state; preserve goals, answers, evidence, and identities | US-10 |
| AC-34 | Archive while execution or repair is unsettled | Reject archive with the specific remaining obligation | US-12 |
| AC-35 | Cleanup targets dirty/attached/in-use resources | Require exact authorized disposition; preserve unapproved resources | US-12 |
| AC-36 | Delete approved project knowledge | Owned bodies/derived summaries removed; references tombstoned | US-12 |
| AC-37 | Repeated same command ID and payload | One logical mutation/effect; conflicting payload rejected | US-01 |
| AC-38 | Corrupt or incompatible durable store | Explicit blocked mutation/recovery state; no empty successful replacement | US-10 |
| AC-39 | Repeat identical blocked observations 100 times | One obligation revision; notification dismissal leaves its meaning intact | US-02 |
| AC-40 | Binding lacks fresh observations for 30 seconds | Unknown health with provenance; no fabricated progress or completion | US-02 |
| AC-41 | Capability lacks required isolation or input acknowledgment | Ineligible for the promised task contract; explicit alternative/blocker | US-03 |
| AC-42 | Concurrent candidates integrate against an outdated parent | Only a current verified integration head is published | US-08 |
| AC-43 | Starting/resuming retries a revoked grant | Dispatch rejected even if an old view still presents the action | US-07 |
| AC-44 | Existing active context is associated with a work | Explicit provenance and safe ownership handoff before managed writes | US-05 |
| AC-45 | Scope changes after acceptance but before publication | Incompatible undispatched publication loses authority; dispatched effects retain original provenance | US-07 |
| AC-46 | Resume from a recorded pause checkpoint | New attempt uses durable verified checkpoint and current grant; writer disposition established | US-11 |
| AC-47 | Planner repeatedly renames tasks to retry failed work | Work-level usage and attempt allowance remain cumulative | US-03 |
| AC-48 | Hold with an approved but undispatched publication | Publication waits; inspection and safe recovery remain available | US-11 |
| AC-49 | Start/reconnect a window without any user shell | Exactly one window Console, Agent Focus, zero required shell tabs/profile processes; work commands usable | US-01 |
| AC-50 | Browse five works in one Console | Each work restores its conversation, draft, caret and reading position; detail state remains separate; opening does not change execution identities | US-04 |
| AC-51 | Hide/show the shell region repeatedly | Layout/focus changes only; same shell and ongoing attempts; no duplicate shell creation | US-11 |
| AC-52 | Close a named shell while Console is active | Only authorized target/session effects; Console rendering channel remains intact | US-11 |
| AC-53 | Change work or physical shell tab while answering | Saved answer targets captured work/decision/version; current shell selection cannot redirect it | US-06 |
| AC-54 | Two windows view/edit the same work | One Console per window, separate drafts, shared version-checked domain state | US-04 |
| AC-55 | Execute an approved work via CLI with no graphical host | Owned process/PTY adapter completes work operations without WT COM/tab activation | US-03 |
| AC-56 | Invoke equivalent authorized actions via slash command and CLI/MCP | Same domain validation and effects; transport adds no authority | US-11 |
| AC-57 | Noninteractive command needs a human answer | Structured needs_input and durable request; no hidden interactive prompt or implicit approval | US-06 |
| AC-58 | Complete creation, decision, review, and control using only commands | All required operation families discoverable and executable through slash help/completion | US-01 |
| AC-59 | Transfer an interactive shell between views | Current input lease acknowledged; old-generation input rejected; work identity unchanged | US-05 |
| AC-60 | Use standalone TUI and foreground shell on a supported TTY | Same work operations, normal return to TUI, explicit unsupported presentation capabilities | US-04 |
| AC-61 | Hide a shell that still owns an unsettled manual writer | Writer obligation remains; handback/settlement required for conflicting work or acceptance | US-05 |
| AC-62 | Reconnect a conversation/event stream after client loss | Replay committed scoped events by cursor or require snapshot; preserve message identities and domain state | US-10 |
| AC-63 | Ask an immediate question, then delegate a fix | Answer without work-start/acceptance ceremony; delegation carries selected evidence into a work | US-01 |
| AC-64 | One message adds a work and changes an existing goal | Display resolved targets/intents; consequential changes are proposed, not silently injected into a running attempt | US-07 |
| AC-65 | Dispatch to a worker that accepts or declines | Runtime start and contract acknowledgment are distinct; decline reaches coordination with a reason | US-03 |
| AC-66 | Execution ends with "done" text but no result submission | MissingSubmission obligation; no task acceptance or downstream success inferred | US-03 |
| AC-67 | Worker reports progress and submits a result | Separate reported progress, observed execution and Submitted outcome; receipt is not acceptance | US-02 |
| AC-68 | A checker consumes an unchecked result | Explicit artifact input permits checking; GatePassed consumers wait for matching evidence | US-03 |
| AC-69 | Result fails a declared criterion | Structured changes request pins result/evidence; bounded new attempt preserves reusable output and resubmits | US-03 |
| AC-70 | Revised result passes its task contract | Service records internal acceptance and admits the authorized next task without human information relay | US-03 |
| AC-71 | Worker needs clarification available in approved work facts | Internal request/answer resumes recorded continuation; no unnecessary human decision | US-03 |
| AC-72 | A needs a decision while the user drafts in B's conversation | Notify without retargeting input; inspect/answer A and retain B's draft and chat position | US-06 |
| AC-73 | Relevant events arrive while coordination is idle or busy | Driver starts one metered turn per work or queues/coalesces; no model polling or concurrent duplicate coordinator | US-04 |
| AC-74 | Many progress chunks arrive without a new decision | Views update without coordinator wake-per-token or user notification-per-tool | US-02 |
| AC-75 | State changes between snapshot acquisition and event consumption | Subscription starts after the snapshot's consistent cursor; client receives the intervening change | US-10 |
| AC-76 | User inspects output, edits, then says changes are ready | Inspection alone does not take over; explicit edit/handback shows changed inputs and resumes affected work without mandatory shell closure | US-05 |
| AC-77 | Every worker slot is occupied by internal context waits | Bounded planning lane can answer; no worker-slot dependency prevents coordination | US-03 |
| AC-78 | Checker or model reviewer reports on a submission | Check evidence stays tied to exact result; no recursive review task chain or model opinion replacing required checks | US-08 |
| AC-79 | User asks to revise the final delivery | Affected tasks enter internal correction; retained results are reused and a new inspectable candidate is returned | US-09 |
| AC-80 | Run the continuous product journey | Real handoff, failed check, revision, intervention and usable delivery; any human orchestration assistance recorded separately | US-12 |
| AC-81 | Send a v1 request through CLI/MCP/runtime adapters | Operation payload, response status and next owner follow the normative protocol, not adapter-specific prose | US-03 |
| AC-82 | Context answer arrives before the requesting turn yields | Queue exact continuation; worker acknowledgment applies it; no false MissingSubmission | US-03 |
| AC-83 | Coordinator ends with a prose plan but no recorded action/finish | Explicit protocol-incomplete outcome; no claimed autonomous advancement | US-03 |
| AC-84 | Required check or review ends without its terminal record | Evaluation controller produces evidence correction/blocker; result cannot remain indefinitely Reviewing | US-08 |
| AC-85 | Collect missing evidence without changing result content | New evaluation round uses the same fixed result; no unnecessary implementation attempt | US-09 |
| AC-86 | Check result from an obsolete evaluation round arrives | Preserve provenance; it cannot determine the current verdict | US-08 |
| AC-87 | Ambiguous intake needs an answer before any work exists | IntakeRequest and its answer/cancellation close the conversation path without inventing a WorkItem | US-01 |
| AC-88 | A named peer task has already ended when context is requested | Coordinator resolves from recorded output or planned clarification; no wait on a nonexistent live peer | US-03 |
| AC-89 | Native command check executes a recipe | Recorded invocation, inputs, command outcome and evidence produce the declared GateResult without agent narration | US-08 |
| AC-90 | Local final delivery is accepted after internal integration | Fixed code/report destination and tracked settlement produce WorkCompleted without an unstated external connector | US-12 |
| AC-91 | Ask globally where works run, their progress and why an approach was chosen | Answer identifies authorized work/runtime state, observation freshness, plan reasons and evidence; unknowns stay explicit, with no mutation | US-02/04 |
| AC-92 | One natural-language message adds B and proposes another approach for A | Resolve and display distinct targets; clarify ambiguity and confirm consequential changes before application; preserve current draft and B's independent execution | US-01/06/07 |
| AC-93 | Discuss goals or alternatives while a work executes | Respond without pausing, restarting, changing scope or injecting new pinned inputs; eligible independent tasks keep advancing | US-04/07 |
| AC-94 | Reject the current approach and approve a replacement | Show reasons and impact, record compatible replan or spec/grant revision, retain proven compatible results and invalidate incompatible undispatched actions; reconcile unsettled effects | US-07 |
| AC-95 | Delegate work using configured authorized capabilities without naming a shell or provider | System selects the correct permitted location/resources and drives internal handoffs without human agent launching, shell choice, context compaction or version matching; unavailable capability is explicit | US-13 |
| AC-96 | Adopt an existing authorized runtime or replace one during a work | Preserve Work identity, decisions and evidence; establish ownership/settlement and acknowledged context before continuation; do not seize arbitrary processes or require human context relay | US-10/13 |
| AC-97 | Accept a promised OS/application state outcome | Qualified adapter verifies exact target identity, approved postcondition, current before/after evidence and side effects; missing, changed or unknown state blocks acceptance; report alone is insufficient | US-08/14 |
| AC-98 | Retry or recover an operation that may have changed OS state | Reconcile prior effect by operation/target identity before replay, preserve unauthorized resources and disclose rollback limits; unknown outcome never becomes assumed success | US-10/14 |

AC-01–AC-90 keep their existing identities and historical evidence. AC-91–AC-95
extend the current task-centered conversation and resource-selection experiment;
they are required targets, not proof of implementation. AC-96–AC-98 require
later adapter/result qualification. They do not add protocol operations by
being listed here. The executable-boundary and negative-control designs are
tracked in the [verification plan](agent-center-verification-plan.md).

## 19. Work-model experiment and subsequent engineering

### 19.1 Current milestone: validate the complete work mode

The experiment evaluates the first developer-work instance of a task-driven
Terminal: the user stays in work-level conversation while the system selects
authorized execution resources, organizes tasks and transfers information.
The human retains goals, consequential judgments and final acceptance. It is
not yet qualification of arbitrary OS tasks, infrastructure availability,
isolation or recovery. Those targets remain documented and cannot be advertised
as proven by an experience demonstration. Existing authority restrictions still
apply throughout the experiment.

Use the companion product specification's continuous journey with at least two
real works. Include question-to-delegation, internal context exchange, a changed
plan after a real finding, a failed task check and structured rework, a user
decision while another work is selected, manual contribution, and final revision.
Also exercise J9: work-grounded global status, execution location, strategy
explanation and a confirmed alternative that changes actual execution without
disrupting the other work. Ordinary discussion must not silently mutate plans.
Choose a real repository, executable checks and a concrete delivery destination.
The correction should arise from an actual discrepancy, not a fabricated failure
notification; a repeatable fixture can provide the task and failing behavior.

One provider can supply multiple roles; agent count is not the success metric.
Every purported internal handoff must have a dispatch, acknowledged contract,
submission, evaluation and downstream input reference. Ordinary UI progress is
driven by those events. Record manual assistance, repeated goal explanation,
copied context, "continue" prompting, mandatory internal shell/provider/session
administration and unusable deliveries as experiment findings. Operator
provenance setup is recorded separately from product-required human assistance;
do not count assisted orchestration as system autonomy.

AC-63 through AC-80 define collaboration behavior; the current journey focuses
on AC-63 through AC-74, AC-76, AC-79 and AC-80, plus AC-91 through AC-95.
Snapshot-race and capacity-edge
fixtures do not gate the experience experiment. The basic single Console,
work switching and input paths remain part of the journey. Structured command
success cannot stand in for work-level natural-language interaction.

Use the product section 12.1 north-star definition: unique verified,
human-accepted completed works with zero required human orchestration divided
by all approved in-scope works in a predeclared cohort and observation window.
Failures, blocked/uncompleted works and post-approval cancellations stay in the
denominator; retries and revised candidates are not new works. Report the intake
funnel separately so a failure before Work creation does not disappear behind
a zero denominator. Never pool results from different builds as one sign-off.

Current milestone sign-off requires J1–J9 on identified same-build continuous
journey evidence, both actual accepted deliveries and zero required internal
human orchestration. A real failed-check/rework cycle in J5 cannot be replaced
by human-requested final revision in J8 or an all-green run. Historical passes,
component conformance, native interaction and release-report coverage remain
separate evidence layers. New or unexercised gates are NOT RUN, not inherited
passes; unsupported future AC-96–AC-98 stay planned/unqualified. Compare these
outcomes before expanding infrastructure or provider coverage.

### 19.2 Subsequent production evidence

Production readiness additionally requires concrete engineering evidence:

| Area | Required proof |
|---|---|
| Window Console and agent focus | Launch with no user shell, switch works in one TUI, show/hide resources, and return keyboard focus without context loss |
| Work-level conversation and command equivalence | Principal stories support natural-language intent plus explicit confirmation; shared-registry commands are precise alternatives, not mandatory user syntax |
| TTY and headless execution | Core work flow completes in an ordinary terminal and through structured calls with no graphical host |
| Continuous execution | A qualified local executor remains correctly owned across window closure and service reconnect |
| Scoped authority | Isolation, operation authorization, delegation limits, and revocation are exercised against real tools |
| Parallel contribution | Two isolated write tasks produce one verified integrated candidate, including a conflict case |
| Human intervention | Pause, writer handoff, manual changes, and rescheduling have observed safe boundaries |
| Reliable decisions | Persisted answers survive callback loss and are applied to the correct continuation exactly as represented |
| Evidence and publication | Current content is accepted and the intended external effect can be rediscovered after receipt loss |
| Recovery | Fault injection across intent/dispatch/receipt boundaries preserves identity and prevents unsafe replay |
| OS/application results | Qualified adapters verify the authorized target's actual postconditions and freshness, capture effects and reconcile retries before replay; local code/report delivery is not proxy evidence |
| Resource continuity | Registered resource adoption or provider/session replacement preserves acknowledged context and work identity without manual context administration or unauthorized process control |
| Product usefulness | The work-mode experiment demonstrates responsibility transfer; interaction timing and presentation quality are measured separately |

These are not prerequisite experiments for validating the work mode. Subsequent
implementation can deliver them incrementally without redefining result
acceptance or weakening the stated target boundaries.
