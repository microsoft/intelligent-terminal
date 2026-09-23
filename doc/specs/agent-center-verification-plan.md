# Agent Center: next verification plan

This plan follows the supplied Agent Center verification report and its B1-B6 findings.
It is a test plan, not a completed product acceptance report.

The product goal is to make Terminal the easiest way to use and manage the OS
to finish tasks through one human-facing work conversation. Users discuss the
goal, location, progress, rationale, changed approach, concurrent work, and
results; they should not have to choose PowerShell versus cmd, launch Copilot,
compact context, copy diagnostics, or schedule internal agents.

The first pilot is Developer/Verifier orchestration for `LocalCode | Report`,
not the whole product or proof of general OS/application outcome support.
The current milestone is the continuous responsibility-transfer journey in
[product sections 11 and 12.1](agent-center-product.md), under
[domain specification](agent-center.md) and the
[normative protocol](agent-center-protocol.md). Passing component tests does
not establish that a real coordinator can carry this journey without human
orchestration.

## Scope and explicit exclusions

Use the pilot to qualify actual task completion, not merely editor operation
or a sequence of successful protocol calls. In addition to the defect gates,
verify work-grounded discussion and status, confirmed approach changes,
concurrent work, authorized resource arrangement, and accepted goal outcomes.
Current AC-91 through AC-95 are required pilot contracts. Natural language plus
explicit confirmation is the main user path; structured CLI requests alone
cannot prove the conversational experience.
US-13/AC-95 cover arranging authorized execution resources without requiring
human shell/provider/session/context administration. US-10/US-13/AC-96 managed
runtime adoption/replacement is later qualification; it must retain work and
evidence identity, settled ownership and acknowledged context, never take over
arbitrary OS processes. US-14/AC-97/AC-98 OS-state effects and recovery require
future adapter/type qualification and are not current LocalCode/Report claims.

Verify the confirmed defects: approved criteria, fixed-content acceptance,
coordinator evidence access, cross-work attention, pre-candidate inspection,
guided manual handover, and declined-contract information and disposition.

Do not implement or require the proposed `$` shortcut for opening an ordinary
shell pane in this round. Ordinary shell presentation is separate from managed
work and writer authority.

Manual handback currently conservatively invalidates consumers of the edited
workspace. Record unnecessary re-execution as a known product limitation; do
not claim fine-grained preservation of unaffected same-workspace tasks. Keep
the stronger safety requirement: old evidence must not silently validate
changed content, other workspaces must remain independent, and old artifacts
and history must not be deleted.

Permission/budget expansion, human checkpoint gates, cross-work dependency
editing, external publication, and automatic production repair are not
sign-off targets for these fixes.

### Current product correction: Work-owned execution conversations

The approved default is a task list with persistent F1 global-chat and F2 task
navigation. The global master does not execute shell/filesystem work; opening a
Work selects that Work's persistent executing agent session, not a context hint
on the global conversation and not a per-work planning coordinator.
Natural language is the primary command surface; slash commands remain optional
precise triggers. Each Work and global chat retain their own conversation,
draft, caret, selection and reading position. Details are explicit, reversible
reading surfaces and must preserve the currently selected conversation state.

This supersedes the context-hint-only conversation requirement, not identity,
confirmation or input-safety protections. Historical C331/C332 results establish
the earlier global-editor behavior only; they do not qualify Work-owned
execution. Retain those historical results and qualify the following revised
boundaries separately before claiming the new architecture works.

| Gate | Required boundary and oracle |
|---|---|
| C329: project-free conversation | Start with an approved assistant and zero execution Projects/Works. Native ordinary text reaches the global ACP actor, produces a real service-backed reply and settles its turn without creating a placeholder project or work. |
| C330: natural-language execution setup | A captured human message supplies a real directory. The actor proposes its exact project/capability/allowance approval; plain Enter or cancel cannot configure it. Explicit confirmation creates the real project, not permission to start work. |
| C331: isolated conversation editors | Switch between global chat and Works A/B and open/close details while editing. Restore each exact draft, caret/selection and console/conversation pair. Fresh messages prove global input reaches only the master and Work input reaches only that Work's executor, not a newly created coordinator. |
| C332: sustained conversation input | Keep the editor usable through at least 180 seconds and 784 successful updates; complete ten physical context/view transitions within 15 seconds each. Busy Work input queues serially without retargeting or creating a second writer. Retain original observer deadlines, real receipt counts and negative controls. |
| C333: cross-work captured action | While A is the context hint, select a proposal explicitly targeting B. The authoritative preview and confirmed method/params/guards/command ID remain B's; navigation, ordinary Enter and a stale proposal cannot retarget or execute it. |
| Ordinary clarification | A global actor interprets an actual later human message against the captured open intake schema. Record interpretation provenance, close only that question and avoid a duplicate turn. This must not answer a permission/DecisionRequest or invent human directory authority. |
| Retained execution and acceptance | Preserve native clipboard/Unicode tests, separate brief/Start approval, bound post-start questions, uncertain exact retries and inspection of the exact fixed delivery before final human acceptance. |

The Work-executor qualification must additionally capture these behavioral
oracles; accepted requests, `sessionLoaded` alone and mock replies are not
substitutes for the actual requested effects:

| Boundary | Required evidence |
|---|---|
| Direct execution | A message in a Work asks its agent to modify an authorized test file. The same recorded provider session performs that change and replies; no per-work coordinator or replacement worker is created. The global master cannot perform the same write. |
| Follow-up and tab identity | Send a follow-up, open the Work in another tab, then submit a third message. All three target the same executor and workspace; the existing conversation and draft are preserved independently in each view. |
| Idle resource lifetime | Ask the executor to start a local verification server. Probe its response after the assistant reply and after another chat turn. Both probes succeed without restarting the server; merely seeing a PID is insufficient. |
| Explicit stop | Pause or cancel the Work using its authorized action. The owned provider/process tree settles, server probes stop succeeding, pending input is not executed, and unrelated Work/Terminal/CLI processes remain alive. |
| Original-session recovery | Restart the service only after its owned writers settle. Continue the Work and verify the same provider session is loaded and a follow-up changes the authorized test file. Unproven settlement or unavailable provider state must produce explicit recovery/unavailability, not a replacement session disguised as recovery. |
| Legacy migration | Preserve prior chat, artifacts and acceptance facts; fence legacy scheduling before adopting a proven actual worker session. A recorded coordinator session must never qualify as that executor. |
| Acceptance remains separate | A normal chat reply leaves the Work active/ready. No successful gate, human acceptance or completed Work is invented to make the chat appear healthy. |

Assistant approval is separate from execution-project approval. No configured
assistant means explicit unavailability, not an inferred model destination or
a request to create a dummy project. Only projects already permitting that
assistant enter its captured read scope; another window's conversation is not
automatically project data.

Scripted ACP/native tests qualify the interface and service transitions above.
They do not establish real natural-language understanding, authentication
readiness, autonomous completion of J1-J9, or general OS capability. Real-model
qualification requires separately authorized quota and a declared cohort.

Frozen-request verification must not parse word-wrapped display text as if it
were lossless JSON: wrapping can insert line breaks and discard boundary spaces.
Pending-confirmation F12 diagnostics retain readable fields and include a
root-level `requestUtf8Hex` copy of the actual captured
`method/params/ifMatch/commandId` object. Decode its hexadecimal UTF-8 JSON
strictly, ignoring only display whitespace in the encoding, and compare every
decoded field without normalizing user content. This is diagnostic metadata,
not a new protocol request field or an execution path.
Omit a nested proposal preview from the rendered diagnostic only when it
duplicates the complete top-level authority preview. Keep the original internal
proposal, distinct evidence, proposal identity and frozen request intact.

## 1. Freeze the build and protect the user's environment

Before a live run, obtain an explicit package selection. Recommend **Dev built
from the repaired feature worktree**; Store is only a shipped-behavior baseline.
Set `ITE2E_PACKAGE=Dev` in every Dev validation process. Do not infer intent
from whichever package or execution alias happens to be installed.

Record the following in the run's evidence:

| Field | Required evidence |
|---|---|
| Source | Branch, HEAD, and a digest or immutable snapshot of the uncommitted changes |
| Executable | Built and deployed `wta.exe` SHA-256; they must match |
| Package | Full package name, family, install location, registration status, and experiment flag |
| Provider | Actual ACP executable/version, selected model, approved destination, authentication readiness |
| Allowance | Explicit planning/execution budgets and authorization to spend real model quota |
| Safety | Existing settings hash, test-owned work IDs/directories/process IDs, cleanup ownership |
| Cohort | Declared task classes, supported capabilities/adapters, scope, fixed observation window, and required journey gates before any trial |

Freeze these fields before the run. A build, capability, task-class or allowance
change starts a separately reported cohort; do not pool mixed builds or relax
the scope after seeing failures. Build/deployment preparation and observer setup
are test-operator work, not automatically product-required user orchestration.
Keep their provenance separate from assistance needed by the actual user journey.

Do not terminate the current CLI host, stop unrelated Terminal/WTA processes,
uninstall a package, or replace real project contents. A live authority loaded
from an old binary must be identified before restarting anything; updating the
file alone does not prove that the new service code is running.

Use an isolated, disposable project containing real defect material. Capture a
baseline for its original dirty/staged changes and verify that managed
workspaces do not alter that source checkout.

## 2. Deterministic defect gates

Run the durable Rust regression tests first, then the real ACP/HTTP MCP/native
process conformance tests. For packaged coverage, extend the existing ItE2E
framework rather than inventing another runner.

For each case below, save the exact request/event IDs and before/after state.
An error string alone is not sufficient: assert the downstream state did not
advance incorrectly.

| ID / checklist title | Contract and trigger | Boundary and deterministic oracle | Negative control / existing protection |
|---|---|---|---|
| R1: Agent Center preserves approved initial criteria | Approve a work requiring a concrete check; submit an initial plan changing that requirement to artifact presence or dropping it. | Coordinator request -> plan admission -> work/result/candidate state. Reject the weakening; no applied weak plan, runnable weak dispatch, accepted result, or delivery candidate. | A compatible plan preserving the approved rule can execute and deliver. Preserve same-grant spec revision and stale-version rejection. |
| R2: Agent Center rejects changed delivery content | Produce a candidate through real capture, then change bytes at the same locator before final acceptance. Repeat for File, Tree/GitCommit content, and required check evidence. | Filesystem capture -> persisted references -> human `delivery.accept`. Fail explicitly with the affected reference; no Acceptance and no Completed work. | Unchanged content succeeds; deletion, file/directory substitution, changed manifests and replaced paths also fail. Do not mistake read-only attributes for verification. |
| R3: Agent Center delivers progress findings to coordination | A worker reports a unique finding and requests coordination; let the service create the next coordinator invocation. | Worker MCP -> committed report -> coordinator input/read. The coordinator can obtain the exact finding, reason and referenced evidence without a human copying it. | Reports from another work are denied; duplicate reports do not create duplicate effects; routine progress without a request does not imply human attention. |
| R4: Agent Center exposes captured check diagnostics | A real command gate emits a unique stderr/stdout marker and fails. The coordinator reads the evidence through its supplied tools. | Native process -> immutable diagnostic artifact -> bound HTTP MCP -> coordinator. Assert actual readable bytes or explicit bounded-content metadata, then a rework action bound to the failed result/criterion. | Foreign-work, unavailable or mutated evidence is rejected. Binary/large content must not silently masquerade as complete text; retain valid fixed-input native checks. |
| R5: Agent Center retains cross-work attention | Leave an open decision in A while drafting in B. Refresh B's nonempty inbox, then an empty scoped inbox. | Service response/events -> Console sidebar and work view. A remains visible globally; B's scoped view is accurate; resolving A removes only A's item. | Global refresh may replace the complete global set. Preserve B's text, caret/selection, target and reading position; replayed events do not duplicate attention. |
| R6: Agent Center separates routine progress from notifications | With a pending A decision, send ordinary text/tool/progress events for B and C. | Subscribed work events -> view updates and human-notice surface. Progress updates the appropriate history without replacing the decision notice or stealing focus. | New decision/final-delivery attention remains visible and can update counts; active confirmations remain frozen. Do not merely assert that the event was produced. |
| R7: Agent Center inspects work before a delivery candidate | Open the workspace before any output, then after an intermediate submission but before a candidate. | `workspace.inspect` -> workspace/artifact projection -> Console. Return the real workspace and exact available result references without inventing a candidate or granting human write ownership. | Draft without a provisioned workspace fails clearly; explicit wrong/foreign candidate IDs are rejected. Existing fixed-candidate inspection remains unchanged. |
| R8: Agent Center guides manual takeover and handback | From selected A, request takeover without authored JSON, confirm, edit the managed workspace, and hand back with a summary and explicit continuation choice. | Console preparation -> guarded command -> runtime settlement/capture -> coordinator continuation. Confirm exact Workspace version; wait for TakeoverReady; capture real changed bytes and record the contribution before agent execution resumes. | Switch to B while preparation is pending: never retarget the action. Cancel/stale/error preserves the draft. Full request files are not rewritten. Work-wide Hold is not overridden, and other-workspace tasks remain unchanged. |
| R9: Agent Center preserves declined contract reasons | A real worker declines with a unique explanatory reason, then the adapter ends through cancellation and releases its binding. | Bound acknowledge -> adapter cancellation -> engine terminal classification -> next coordinator. Persist and expose the exact reason; final state/disposition is Failed/ContractDeclined, with reservations released. | Human cancellation remains cancellation; accepted-but-missing-result remains MissingSubmission. Duplicate acknowledgments/observations do not consume extra attempts or erase the original reason. |

The earlier `verification-probes.patch` cases are regression seeds, not a
replacement for real capture/adapter boundary coverage. Keep their failing
intent, but update fixture metadata to the production capture format rather
than retaining success-shaped synthetic receipts.

Suggested local commands:

```powershell
cargo test --target x86_64-pc-windows-msvc --manifest-path tools\wta\Cargo.toml agent_center::
cargo test --target x86_64-pc-windows-msvc --manifest-path tools\wta\Cargo.toml
cargo build --target x86_64-pc-windows-msvc --manifest-path tools\wta\Cargo.toml
```

These are component/regression gates, not packaged UI sign-off.

### Round 2 follow-up gates

Round 2 reached real ACP/model output but stopped at work-MCP initialization.
The client's offered version was not recorded; do not infer it from the test
version below. Round 2 also encountered a fresh-capture publication access
error once, even though subsequent runs passed.

| Gate | Trigger / boundary | Required oracle and negative control |
|---|---|---|
| MCP version negotiation | Offer supported versions, `2025-11-25`, and a future version over real HTTP; then initialize, list bound tools and execute the existing ACP conformance journey. | Known versions are preserved; unknown offers receive the supported `2025-06-18`, not a dropped socket or a claim to support unknown semantics. Missing/non-string/empty versions receive correlated `-32602` errors. Actual work-tool calls, rework and delivery still succeed after negotiation. |
| Capture publication contention | Hold a real Windows descendant handle without delete sharing while publishing its immutable staging directory; release it during the bounded rename retry window, then repeat without releasing. | Transient lock: publish the same captured bytes successfully. Persistent lock: explicit failure after at most six attempts/620 ms scheduled delay, no recapture or false success. Collision/nonretryable error: preserve existing content and fail. Acceptance integrity errors identify the Artifact EntityRef. |

For the next **real Copilot** run, retain the `work MCP protocol negotiated`
record with its offered/selected versions and invocation ID. Require actual
bound tools to initialize and J1 to finish before proceeding through J2-J8.
A controlled actor accepting negotiation proves the transport behavior, not
that the real model completed the product journey. The original publication
lock holder remains unknown; preserve OS error and stage information if it
recurs rather than masking it with another successful whole-test retry.

### Round 3 follow-up gates

Round 3 proved real MCP negotiation and the J1 diagnosis/delegation boundary.
It did not admit an initial real-model plan or run a worker. Preserve the
original long-path failure and the separately counted shorter-path control;
neither is an uninterrupted J2-J8 journey.

| Gate | Trigger / boundary | Required oracle and negative control |
|---|---|---|
| Discoverable plan output kinds | Obtain `plan_propose` through real HTTP `tools/list`; submit an invalid output kind, then correct it using the advertised vocabulary. | All six supported kinds are advertised. Invalid proposals return `INVALID_ARGUMENT`, exact indexed field feedback and allowed values without changing the work or admitting tasks. A corrected proposal applies and starts a worker. Code/Tree/GitCommit without a required check still fail. |
| Approved prose evidence mapping | Approve an ordinary-language evidence rule; copy it exactly into task criterion `evidenceRule`, with concrete `requiredEvidence` bindings. | The immutable task/dispatch retains the exact rule, and actual checks/captured artifacts govern acceptance. Changed/omitted prose, empty/unknown/optional-only bindings fail. Existing command, artifact and bare gate reference rules cannot be replaced by weaker mappings. Run real ACP conformance with both prose and reference-based works through failed-check/rework/delivery. |
| Long managed Git worktrees | Provision under managed workspace paths of at least 237 and 250 characters, then capture a fixed commit and run Git in the populated worktree. | Production provisioning and fixed capture succeed without source checkout/global Git configuration changes. Checkout failure remains explicit and removes partial worktree registration where possible; source-repository branch references are retained, and cleanup failure is reported. Keep artifacts inside the managed root. Do not substitute a shortened-state run or only set core.longpaths and call the original error fixed. |
| HTTP response shutdown | Use real Windows TCP with queued input after a complete notification or JSON request. | Complete 202/JSON response and clean EOF, not 10054; maintain authorization, framing and timeout limits. Retain per-exchange/server diagnostics. The reproduced queued-input mechanism does not establish the exact historical trigger; retain any new reset rather than hiding it with a passing rerun. |

The next real-model run must show an admitted/applied plan, a dispatched worker,
and typed coordination completion before claiming the J2 execution boundary.
Record exact rejected field paths and version conflicts if planning still
fails. Do not inject a valid plan, increase the deadline, or relax approved
scope/evidence constraints to turn it green. The new prose mapping preserves
meaning and concrete references; semantic adequacy of the chosen check still
requires the real journey and delivery review.

### Round 4 follow-up gates

Round 4 admitted A/B plans and dispatched four real workers, but the ACP host
cancelled every initial acknowledgement permission request. Its provider-side
"user rejected" text was a host-policy decision, not an actual human rejection.
The old scripted actor bypassed that boundary by calling MCP directly.

| Gate | Trigger / boundary | Required oracle and negative control |
|---|---|---|
| Initial acknowledgement permission | Real ACP `session/request_permission` for the bound acknowledgement, then HTTP MCP `task_acknowledge`. Include Copilot's observed bare title with kind `other` and the server-qualified forms. | Select `allow_once`, then observe the service Acknowledgment and acknowledged Attempt. Permission alone must not set acknowledged. Deny wrong session/server/tool/dispatch/revision/continuation, malformed arguments, unbound tools and native execution/write requests before acknowledgement. Never substitute global permission approval. |
| Continuation and revocation | Yield for internal context, resume with the current continuation, and request permission before acknowledging it. Repeat after cancellation/release/end of turn. | Exact current continuation succeeds; missing/stale continuation and revoked/nonrunning invocations fail. Actual resumed text starts at chunk zero for its new part; no ignored TextDelta errors. Existing post-acknowledgement execution remains available. |
| Actual producing worker | Use the latest Dev binary with a bounded real Copilot worker on a synthetic project. | Record actual permission request/response, acknowledgement, producing result and real check/candidate. A scripted coordinator may prepare a diagnostic plan, but that is explicitly a worker-path probe, not an autonomous planning or J1-J8 pass. Retain failed probes and count every model prompt. |

The repair probe used two authorized real worker prompts: the first exposed
Copilot's bare permission title and was denied by the initial qualified-only
matcher; the second acknowledged, produced a result, passed its actual command
check and created a candidate. The coordinator was scripted, and no final human
acceptance, real-model rework or complete native journey was exercised.
Repeat the uninterrupted real two-work journey separately. Native foreground
acquisition and the previously recorded TAEF initialization failure remain
independent verification gaps; do not bypass safe input preconditions.

### Round 5 follow-up gates

Round 5 confirmed the real acknowledgement repair and one passing native code
check. Both works then submitted File-only report/evidence results whose command
checks failed before launch: evaluation omitted the producer's fixed dependencies,
and runtime required a Tree/GitCommit. Generic evidence collection repeated the
same error. Refusing a mutable workspace fallback was correct.

| Gate | Trigger / boundary | Required oracle and negative control |
|---|---|---|
| File-only checks and report delivery | Admit Report/Evidence outputs, capture two Files, change the synthetic mutable workspace, and run the declared command through the runtime. | Real process reads the captured files and reaches a Report candidate and explicit acceptance. Uncaptured workspace files are absent; modified workspace bytes do not affect the verdict. File paths are captured basenames; nested layouts use Tree captures. |
| Pinned integration inputs | An accepted code contribution feeds a ReadOnly integration that submits only File reports. Include two input aliases of the same immutable artifact. | Evaluation retains both exact original input bindings and current outputs in its manifest; the real command reads the accepted code plus fixed reports. LocalCode candidate retains the unambiguous accepted source snapshot and provenance; acceptance rechecks it. Changed source bindings or captured code block acceptance. |
| Ambiguous and complete snapshots | Submit conflicting same-layer paths, duplicate identical captures, and a replacement code snapshot that deletes an old input file while retaining a separately pinned File check script. | Conflicting captures fail explicitly before process launch; identical aliases remain usable. A complete new code snapshot does not resurrect deleted dependency files and still runs its independent check script. No arbitrary artifact order or mutable cwd decides the result. |
| Preparation diagnostics and bounded correction | Trigger a real preparation/startup error without a process GateResult; read result/rework and coordinator-visible diagnostics. | Preserve the exact bounded error, phase and affected identities. Explicit ReviseOutput dispatches corrected capture work without weakening its contract; repaired transient causes can explicitly retry CollectEvidence. Identical repeated failures reach a bounded blocker. Actual nonzero process results and human cancellation retain their distinct classifications. |

The deterministic ACP/HTTP/native-process fixture covers the new File-only and
pinned-code report paths through final acceptance alongside the existing
context/rework/decline paths. This is not a new live-model or packaged UI
sign-off. Obtain fresh package/model-budget authorization before repeating the
autonomous two-work journey; the two prompts authorized for the round-4
diagnostic probe are exhausted and cannot be reused as authorization.

### Round 6 follow-up gates

Round 6 passed seven real native checks and explicitly accepted both works,
including CLI handback and a genuine report revision. Native Console startup
instead exposed a preexisting cancellation-unsafe pipe reader: an interleaved
service update discarded a consumed length header, then interpreted the JSON
prefix `{"ty` as payload length `2037654139`. Two legal fragmented requests
reproduced this; the complete-frame control succeeded.

| Gate | Trigger / boundary | Required oracle and negative control |
|---|---|---|
| Fragmented requests and concurrent events | Consume the length header or partial header/body on a real named pipe, then cause service changes on another connection before sending the rest. | Preserve the pending frame and return the exact requestId with a successful response. Subscription events must arrive before the remaining request bytes are sent. Cover individual header bytes, repeated body fragments and a subsequent complete request. |
| Reader shutdown and malformed frames | Disconnect during a header/body or stop the service while a payload is incomplete; send zero/oversized lengths, malformed JSON, invalid UTF-8 and non-object JSON separately. | Dead connections release their handlers within the existing bounded cleanup. Illegal frames still fail without reading/allocating an oversized body; legal fragmentation never gets reclassified as malformed input. |
| Deployed Console transport | Repeat the report's split-frame probe against the verified Dev executable, then exercise the native Console under work updates. | Correctly correlated pipe responses establish the transport repair. Native R5-R8 interaction is a separate requirement; a pipe probe alone does not sign off attention, drafts, inspection or guided transfer. |

The durable regression observes actual read consumption and received events,
without sleep-based race assumptions. Keep round-6 integrity, File-only checks,
human contribution and report revision as regression gates. J3-J6 remain
unexercised live scenarios until separately demonstrated; do not substitute
successful component tests or transport probes for them.

### Round 7 follow-up gates

Round 7 confirmed the framing repair, completed two further real deliveries,
and demonstrated native takeover, manual contribution and handback. It also
confirmed that PageDown could scroll a pending confirmation completely out of
view. The input-tail observations remain unresolved between physical injection,
native input handling and observation timing; autocomplete text is not the draft.

| Gate | Trigger / boundary | Required oracle and negative control |
|---|---|---|
| Bounded confirmation reading | Prepare a long takeover/handback preview; repeatedly PageDown past its end and resize between narrow/wide and short/tall viewports. | The actual wrapped preview stays visible at the end; one PageUp moves immediately, without spending prior overscroll. Scrolling/events never change or send the frozen operation, target or version guards. Only explicit confirmation sends it. |
| Independent preview/history positions | PageUp from followed latest activity; open, scroll and cancel a preview; then prepare another preview. | Activity moves relative to its displayed tail, not to transcript start. Cancellation restores its saved reading position, and a new preview starts at the top without inheriting history/previous-preview offsets. |
| Actual input versus suggestions | Type a known full UUID with independently verified key delivery while service updates continue; observe the draft row separately from autocomplete. | Compare the actual draft and selected target, not whole-screen substring matches. Reducer/render tests retain every delivered character, but do not prove that physical injection or native input delivered every event. Preserve post-input evidence and distinguish a delayed event from lost input before assigning cause. |

Keep native draft caret/selection, scoped inbox, notification, inspection and
stale-transfer cases open until demonstrated on the deployed surface. Neither
the renderer regression nor the successful native handback closes those cases.

### Round 8 follow-up gates

Round 8 passed native confirmation scrolling/resizing and the framing regression.
Its acknowledged physical-key path delivered all 46 draft prefixes and selected
the intended work. This does not identify the cause of the older bulk-injection
tail observation. New native evidence confirmed that Left did not move the
insertion point in both Active and Cancelled work drafts: typing remained
append-only. This round was UI-only and used no model calls.

| Gate | Trigger / boundary | Required oracle and negative control |
|---|---|---|
| Editable draft caret | Type `round8-b-draft`, press Left three times, type `x`; separately type `draft`, press Left twice, type `x`. | Actual draft rows show `round8-b-drxaft` and `draxft`, with the caret after the inserted character. No submitted prompts; repeat in Active and Cancelled work. |
| Selection and Unicode editing | Shift-select in either direction, replace with text/paste/newline, delete before/at the caret, and navigate across Chinese, combining accents and joined emoji. | Exact resulting draft bytes, visible selection and actual caret cell agree; no split graphemes. Ctrl+A selects only the draft. Empty/boundary deletion does not alter unrelated text. |
| Independent editor state | Switch A/B while both have drafts and selections; deliver other-work events and an older response; resize a long multiline input and navigate back to its start. | Restore each work's text/caret/selection; viewport follows the caret rather than always showing the tail. A successful response clears only its unchanged submitted draft and resets that editor, not the currently selected work. |
| Existing shortcut and confirmation behavior | Use slash completion, PageUp/PageDown/Ctrl+End, and a frozen takeover/handback preview; try typing and paste while pending, then cancel. | Completion still works, history keys do not move the draft caret, pending edits cannot mutate the draft or operation, and cancel restores the editor. Only explicit confirmation submits the frozen command. |

Reducer and TestBackend regressions cover these edit/render contracts without
model calls. They do not establish physical key delivery or deployed-package
acceptance. Repeat the native gates against a verified repaired Dev binary with
foreground protection and observed input receipts; keep the original round-8
evidence intact. Intermediate-result inspection, transfer negatives and the
remaining real-model journeys still require their own evidence.

### Round 9 follow-up gates

Round 9 passed the original Active/Cancelled caret repair but found two native
paste failures: UTF-16 surrogate pairs disappeared and CRLF paste dispatched
the first line without Enter. Direct editor/Paste-event tests missed the
Windows input boundary. ConPTY's non-VT input path consumes the bracketed-paste
delimiters, and Crossterm's Windows reader pairs surrogate key-up records with
their key-down records. The Console now owns a Windows reader, enables VT input
before requesting bracketed paste, and assembles a single atomic Paste event.

| Gate | Trigger / boundary | Required oracle and negative control |
|---|---|---|
| Native non-BMP paste | Foreground-gated clipboard paste of `a` + U+1F469 + `z`, then joined U+1F469/U+200D/U+1F4BB; include ASCII and BMP/combining controls. | Exact draft bytes and visible native text retain the complete payload. Selection replacement and subsequent grapheme movement/deletion remain correct. Verify the synthetic clipboard before paste; preserve and restore prior clipboard data without logging it. |
| Atomic native multiline paste | Paste two unknown slash commands separated by CRLF and send no Enter. | Both LF-normalized draft lines remain unsubmitted; no premature METHOD_UNSUPPORTED or queued operation. A later explicit Enter remains an action, while physical Shift+Enter remains an edit. |
| Native input lifecycle and fragments | Feed per-unit down/up UTF-16 records, split paste delimiters/body across reads, interleave service updates and repeat-key records, then exit an idle reader. | No event before the closing delimiter; one complete Paste event afterward; no surrogate loss or key-up duplication. Preserve physical modifiers/repeats and restore the original console input mode without stopping other processes. |

The isolated Windows console regression crosses WriteConsoleInputW and the
actual native reader but does not replace clipboard -> Terminal -> ConPTY ->
Console validation against the deployed package. Keep old bulk-input delivery,
delayed-response restoration and the remaining business journeys separate
until their own oracles pass. No model quota is required for these paste gates.

A second isolated regression sends real UTF-8 paste and Win32 Enter sequences
through CreatePseudoConsole, not fabricated input records. This also covers
older ConPTY hosts that emit Unicode text on Alt release instead of ordinary
key-down records. Foreground clipboard acceptance remains distinct; if the
native test window cannot acquire foreground, report BLOCKED rather than
crediting this headless transport regression as packaged UI acceptance.

### Round 10 follow-up gates

Round 10 passed both original paste gates in the actual dedicated Console.
It exposed regressions in Shift+Enter, Ctrl+Enter and standalone Escape, plus
an E2E parser that matched `Enter` inside the top `Agent Center` title. A real
ConPTY negative control reproduced the lost Shift modifier before the repair.
The Console now requests lossless Win32 keyboard packets in addition to VT
input, instead of inferring physical keys from raw control characters.

| Gate | Trigger / boundary | Required oracle and negative control |
|---|---|---|
| Physical modified Enter | Shift-select `fi` in `first`, press Shift+Enter; separately Ctrl+Enter a frozen guarded preview. | Newline replaces the selection with no queued operation; confirmation queues exactly the frozen command ID, work, parameters and guards. Plain Enter remains submit. Keep native clipboard/multiline gates green. |
| Standalone Escape | Prepare a preview, press Escape once and provide no further input. | Preview cancels and restores draft/caret/selection with no operation. The real ConPTY regression requires an Escape receipt before it sends the next key; no timeout-based paste/escape guessing. |
| Packet and paste boundaries | Deliver raw and packet-encoded bracketed Unicode paste, including split headers, UTF-16 halves and key-up records. | One complete paste; packet ASCII cannot corrupt the saved surrogate or become draft content. Physical modifiers/repeats survive, key-up does not double-act, and console modes are restored on exit. |
| Exact E2E input frame | Capture top Agent Center/Current work rows before a later input frame; include localized or clipped input help and missing/malformed frames. | Shared parser selects only the anchored locked `Enter` title with matching borders, preserves actual draft whitespace and fails explicitly when the frame is absent. C319/C320 cannot pass from a title substring or relaxed empty-draft assertion. |

The expanded headless test crosses real ConPTY and then applies the received
events to the Console reducer, checking the draft and queued frozen operation.
It does not replace physical input on the newly deployed package. No deployment
or packaged acceptance is implied by a successful source build. Keep the
under-load F2 request delay separate: the round-10 observation already reached
the Sending state, and its quiescent control passed.

### Round 11 follow-up and double-work increment

Round 11 establishes physical Shift+Enter, standalone Escape, exact guarded
Ctrl+Enter, the shared input-frame parser, and actual packaged C319/C320 passes.
These are input and interaction evidence, not J1-J9 business-journey sign-off.
Its isolated UI-only cohort has no authorized external-model business trials;
the unassisted accepted-delivery metric remains N/A.

The remaining confirmed failure is the loaded request/update path. During 784
guarded updates over 180 seconds, successive physical F2 transitions took about
4.4 and 10.4 seconds, then failed the unchanged 15-second primary bound. The
live event stream closed and the selected work had not changed after about
136 seconds, including roughly 66 seconds after the final update. A later
quiescent editor matrix passed. Event queue overflow is a candidate, not the
proven terminating cause; retain the actual error when reproducing.

The current implementation increment targets a complete human-operable
double-work path, not just this transport repair. Full J9 grounded global
conversation and confirmed strategy replacement follow this increment but
remain required for final current-pilot acceptance.

| Contract | Trigger and boundary | Required oracle / negative control | Existing story |
|---|---|---|---|
| Loaded navigation and recovery | Keep distinct unsent A/B drafts while real service updates arrive; physically switch work, then exercise bounded stream resync and actual connection loss | Every switch meets the original 15-second bound; current goal and exact draft/caret/selection belong to the selected work. After traffic/loss, authorized updates resume without reopening Console. Preserve root error, make stale facts explicit, and do not replay uncertain mutations with new identities | US-02/06/10; R5/R6; round-6 framing; round-11 loaded failure |
| Long pre-work coordination location | Launch intake from the actual private state root with a coordination cwd of exactly 260 characters, and beyond that boundary | Where an existing Windows short name is available, a real child starts in the exact canonical directory and writes only its owned marker there. An unshortenable directory or a non-directory fails explicitly, without relocation or new junctions. Packaged ACP intake must reach its bound question operation without shortening the fixture path. Preserve the original `os error 267` evidence separately from fixture/parser failures | US-01/13; J1; round-11 workflow native follow-up |
| Readable work entry and approval | From a preconfigured project, submit a goal, inspect the brief and confirm using visible actions, not slash/UUID/JSON | Exact prior material, goal and approved criteria reach one work and guarded start; cancel sends no start. Same-named choices are disambiguated by readable context, never guessed | US-01/11/13; J1; R1 |
| Independent concurrent work | Create/select B through the visible work entry while A advances; switch back and forth and open A's attention while editing B | Separate work identities and scopes remain internal; default UI shows readable goals/project. A progresses; B draft/caret/selection and target survive entry, cancellation and return | US-02/06; J2/J6; R5/R6 |
| Typed question response | Open a real service-owned intake/decision and answer its fields or options | Exact request/decision ID, version and correctly typed value reach the service; saved and applied are distinct. Stale/cancelled/foreign targets cannot consume the answer. Unsupported schemas fail explicitly, not as empty/default answers | US-06/11; J6; R5 |
| Inspect and accept exact delivery | Open the selected work's current report/code result and evidence, then explicitly accept | Visible content/locator and checks refer to the fixed candidate; inspection does not acquire a writer or accept. Guarded acceptance targets that candidate/work, and pending settlement is not displayed as Completed. Changed content and stale candidates remain rejected | US-08; J8; R2/R7 |
| Normal versus diagnostic presentation | Read default work, question, preview and delivery views, then explicitly open diagnostics | Ordinary operations need no system ID, revision or authored JSON. Diagnostics preserve exact associations. User text, real paths, risks and failures are not scrubbed to meet an ID-free screenshot assertion | US-01/02/06/08/11 |

Deterministic fixtures may establish native input, rendering, typed command and
service-state boundaries, with fixture setup disclosed. They do not establish
natural-language interpretation, real internal clarification/failed-check
rework, or two autonomous business deliveries. Keep those assertions in the
separately authorized continuous J1-J9 journey; do not reduce its denominator
or mark it PASS because these interaction cases pass.

### Round 12 follow-up and approved presentation redesign

Round 12 reports no newly confirmed product failure. Its eight existing native
workflow/input cases pass. A separate dedicated Console keeps the same UI
process across actual authority loss/restart, becomes fresh in about 3.98
seconds and retains the original frame-length EOF cause. This establishes
that particular recovery path, not loaded editing or nonempty-draft recovery.

R12-H1 is a confirmed durable test-observer defect: after committed updates
have already been observed, a temporarily unavailable progress snapshot can
be misclassified as failure to receive the *first* update. Repair the monitor
without resetting its original start/progress clocks, masking producer
errors, treating a missing snapshot as zero progress, or relaxing the
15-second stall and 240-second total budgets. Cover both first-update and
established-progress gaps, actual stalls, early exit and deadline exhaustion.

Subsequent validation of the redesigned client found a separate product defect:
switching project/conversation, or creating a new intake from selected work,
could reuse an already-bound Console registration and fail with
`INVALID_REFERENCE` before producing a brief. Preserve the immutable
console/conversation pair for each scope and each uncertain command; do not
weaken the service's cross-binding rejection. Cover switching projects,
returning to the original draft, exact request reconciliation, and actual
native initial-brief/Start followed by a bound post-start decision. This is a
newly reproduced implementation defect, not a finding in the original Round 12
report.

The initial dedicated loaded-input experiment was incomplete: early and approximately
60-second work/draft/cursor checks passed, then the observer stopped around
112 seconds after 636 successful updates; a later run lacked the foreground
desktop prerequisite. Neither full 180-second input restoration nor final
selection replacement is credited from those partial observations.

The subsequent identity-fixed Dev build (`F35FEA97...`) passed all ten combined
native cases. Its completed pressure run recorded 1,021 successful updates
over 181.10 seconds and ten physical work switches, the slowest at 0.40 seconds.
That closes the old round's input/identity gates without erasing its failed
attempts. It does not qualify the later global-chat contract or real-model
business autonomy.

The approved TUI redesign must add the following evidence while retaining the
existing Unicode, multiline, exact-confirmation, 180-second/784-update and
15-second physical-switch protections:

| Contract | Required boundary and oracle |
|---|---|
| Work-focused layout | Render wide, compact and short terminal sizes with real-shaped service data. Navigation, optional details and contextual actions must preserve readable work identity, input target and visible caret. No raw object dump, fabricated progress, or loss of literal user/evidence content |
| Keyboard access and focus | Navigate and activate visible actions, return to the composer, open/close details and resize. Draft/caret/selection remain attached to their work; Enter in a selector or frozen preview cannot leak into message submission or mutation replay |
| Initial brief and Start | Inspect the actual work brief and authorization boundary, retain the exact work/project and captured guards, prove plain-Enter/cancel suppression, then explicitly confirm Start and observe its authoritative operation/state. A passing Cancel preview is not this gate |
| Post-start human question | A real invocation-bound actor raises a work-owned decision after Start; native typed confirmation reaches that exact request and its actual continuation/application. Pre-work intake answer storage alone is not this gate |
| Reliable sustained observer | Missing intermediate snapshots retain last known committed progress and original deadlines. Complete the required pressure interval with receipt evidence; partial runs or foreground blocks remain explicitly incomplete |
| Stale and uncertain presentation | Show last-known facts and unavailable mutation actions without inventing live execution. Unknown mutation outcomes retain their identities and remain unknown until a matching authoritative result arrives |

Use deterministic adapters only for the interface/service boundaries above.
They do not supply natural-language interpretation, real business decisions
or J9 evidence. New checklist entries require their own exact-title mapping
and actual full/incremental report results; a design prototype or missing test
must not be reported as either a product failure or a completed user journey.

## 3. One real two-work product journey

Use one real code project and two independent works. Choose defects that are
actually present; do not ask a scripted actor to emit pretend discoveries,
pretend check failures, or preset completion cards.

| Step | User action | Required downstream evidence |
|---|---|---|
| J1: question -> delegation | In ordinary language, ask about a real error and its location, then request its repair without restating the supplied material or choosing a shell/agent. | The answer does not create an unwanted work. The subsequent brief carries the correct material, boundaries and criteria; explicit approval precedes execution. Record intake failures even if no work ID is created. |
| J2: independent work | Say "also investigate this other issue" to create B and continue drafting there while A executes. | Distinct workspaces, histories, tasks and artifacts. A advances without target changes or a hidden human scheduler; a mixed new-work/change-plan request asks which work to change rather than retargeting B's draft. |
| J3: real finding -> changed arrangement | Let A investigate and discover a fact affecting its initial approach. | The worker records the finding; the coordinator obtains its body/evidence and records an appropriate plan/action. No copied log or manually supplied next step. |
| J4: internal clarification | An executor needs a fact already present in approved material. | Correct internal question/answer and acknowledged continuation; no unnecessary human decision. Include the early-answer-before-yield timing case. |
| J5: failed check -> fresh submission | A real required check fails. | Captured diagnostic bytes reach coordination/execution, correction is bound to the finding, and a fresh result is checked against its own content. No weakening of criteria. |
| J6: actual human judgment | A needs a genuine scope/input choice while the user is drafting in B. | Persistent A attention survives routine events and B inbox refresh. Answer is applied to A; returning to B restores its draft. Same-grant scope revision is proposed and explicitly approved, not silently applied. |
| J7: inspect -> contribute -> hand back | Before final delivery, inspect A, take over, make a small real edit, and hand it back. | Accurate workspace, settled old writer, explicit human ownership, real immutable manual capture, and renewed relevant work/checks. Record the current conservative invalidation cost honestly. |
| J8: final revision -> actual delivery | Request an explanation/report correction on a candidate, review its replacement, then accept A and inspect and explicitly accept B's report. | Two verified deliveries with explicit human acceptance and Completed work state. New fixed candidate/version with coherent evidence and explicit retained/invalidated outputs. Code has a real location and version; report contents are readable. Acceptance does not mean external publication. |
| J9: status -> rationale -> confirmed approach change | While B advances, ask "what is happening overall?", "where is A executing?", "why this approach for A?", discuss an alternative, then explicitly reject A's strategy and approve a compatible replacement after reviewing impact. | Status, execution location and rationale refer to actual work/plan/evidence and distinguish unknowns. Discussion alone does not alter scope. Preview identifies target, retained/invalidated outputs and unsettled effects; only approval applies an actual alternative compatible plan, invalidates incompatible undispatched actions and reconciles unsettled effects. B continues with its draft and target intact. |

If there is no naturally occurring check failure or internal clarification,
record that the scenario did not exercise that gate. Use another known-bad
fixture/project variant; do not insert false evidence and call it autonomous.

For **full current continuous-pilot completion**, J1-J9 are named required
gates, alongside the applicable R1-R9 defect gates. Each journey gate must pass
on identified evidence in the declared continuous journey; retain separate
same-build evidence for component-defect gates. Separate diagnostic rounds do
not assemble an uninterrupted pass. In particular, J8's user-requested report
revision cannot substitute for J5's real failed-check/diagnostic/rework loop.
A narrower probe may retain its own useful result, but is not full pilot
completion. Earlier round narratives above remain historical observations.
Require two verified, explicitly human-accepted Completed deliveries on the
same identified build as J1-J9; neither a candidate alone nor acceptance text
without the committed state closes this gate.

### 3.1 Task-centered testcase matrix

These `T-*` identifiers are specification cases, not release `C` IDs or new
Pester titles. R1-R9 remain distinct component-defect regressions; the matrix
requires their downstream product outcomes. "Existing protection" names
regressions/gates to retain, not a claim that their live coverage has passed.
New cases below are **planned coverage, not implemented or credited by this
documentation change**. Execution is **NOT RUN** here; preserve historical
round results separately rather than relabeling them.

| Case / contract | Exact trigger | Real boundary | Deterministic oracle | Negative control | Existing protection | US / AC / journey | Stage / coverage |
|---|---|---|---|---|---|---|---|
| T-01: Discuss first, approve work once | Ask about a real error at an identified location; then "fix that here" and approve the brief | Native conversation -> intake -> approved work -> real coordinator dispatch | No work on discussion alone; one work ID on approved creation with prior material/location/criteria and an authorized dispatch | Ambiguous location or authority asks for clarification and does not execute; retry does not duplicate work | R1; round-2 MCP initialization and round-3 plan admission gates | US-01/US-13; AC-95; J1 | Current pilot; planned, NOT RUN |
| T-02: Add concurrent work without losing a pending draft | While A runs and B has a draft, request new C and an ambiguous change to "that work" | Conversation intent separation -> guarded target confirmation -> independent work scheduling | No change applied until target/impact approval; approved new work has its own ID; B draft/caret/selection/target survive; A advances in real events | Cancel, stale preview, or switch target during confirmation changes neither work nor draft | R5/R6/R8; round-8 editor isolation | US-01/US-06/US-07; AC-92; J2/J6 | Current pilot; planned, NOT RUN |
| T-03: Explain overall status and why this approach | Ask global progress, A's execution location, then rationale for A while B executes | Work/plan/evidence state -> coordinator response -> human view | Every state/location/decision claim resolves to current work/plan/evidence IDs; blocked/unknown facts are explicit; persisted state agrees with the response and B advances | Missing evidence must not become invented progress, location or rationale; discussion does not dispatch a change | R3/R4/R5/R6; round-6 transport | US-02/US-04; AC-91; J9 | Current pilot; planned, NOT RUN |
| T-04: Ordinary discussion is not an instruction to change scope | Ask "what would an alternative look like?" and discuss tradeoffs while A runs | Human conversation -> intent/admission -> work and plan state | Scope/approved plan/target unchanged after the response and bounded observation; ordinary authorized progress still occurs | A later explicit change request requires its own preview/approval rather than being ignored as chat | R1/R6/R8; frozen-confirmation gates | US-04/US-07; AC-93; J9 | Current pilot; planned, NOT RUN |
| T-05: Reject a strategy and approve a compatible replan | Say "do not use that approach for A"; review impact and approve replacement while B advances | Conversation -> impact preview -> guarded plan admission -> dispatch and effect reconciliation | Exact approved plan/version; incompatible undispatched actions cannot run; old in-flight effects are settled/reconciled before conflicting work; evidence lineage retained; B unchanged | Discussion-only, cancelled/stale approval or weaker criteria cannot install the replacement; no duplicate side effects on replay | R1/R2/R8; fixed-input and writer-settlement gates | US-07; AC-94; J3/J9 | Current pilot; planned, NOT RUN |
| T-06: Product arranges authorized execution and internal handoffs | Approve repair and let a worker discover a real fact, request known context, fail a check, and correct it | Real coordinator -> authorized resource/ACP dispatch -> worker MCP -> native check -> coordinator rework | Resource identity/location/authority recorded; finding and captured diagnostics actually read; acknowledged continuation, fresh result/check and next action occur without copied logs, shell/provider choice, session compaction or manual scheduling | Unavailable/unauthorized resource yields an explicit blocker, not broader authority; failed criteria cannot be weakened | R1/R3/R4/R9; rounds-3/4/5 admission, acknowledgement and check gates | US-13; AC-95; J1/J3/J4/J5 | Current pilot; planned, NOT RUN |
| T-07: Genuine human judgment reaches only its work | A needs a new goal/scope choice while user drafts in B; answer A after ordinary B events and inbox refresh | Decision attention -> native user reply -> guarded A continuation | Pending attention persists; exact answer and approved same-grant change belong to A; B draft preserved and work advances | Already supplied facts stay internal (J4); stale/foreign answer cannot resume A | R3/R5/R6; round-8 draft preservation | US-04/US-07; AC-91/AC-93; J4/J6 | Current pilot; planned, NOT RUN |
| T-08: Inspect and voluntarily contribute without becoming the scheduler | Inspect intermediate A, take over, edit, hand back with a summary; review revised delivery | Native inspect/ownership transfer -> immutable capture -> automatic continuation/checks -> human acceptance | Settled writer and recorded human contribution; renewed checks refer to changed content; final accepted work has verified goal evidence and real location/version; retained outputs identified | Read-only inspection grants no writer; changed/missing candidate content blocks acceptance; B remains independent | R2/R7/R8; rounds-5/6/7 capture, report revision and handback | US-08/US-10/US-13; AC-95; J7/J8 | Current pilot; planned, NOT RUN |
| T-09: Continue work across authorized managed runtime replacement | Replace/adopt an authorized existing managed runtime after a known progress/acknowledgement boundary | Runtime lifecycle -> ownership settlement -> restored acknowledged context -> work/evidence state | Same work and evidence identity; old owner settled; resumed context acknowledged; no duplicate work/effect | Unrelated OS process, revoked resource or stale continuation is not adopted | R8/R9; round-4 continuation/revocation; J7 settlement is related, not replacement coverage | US-10/US-13; AC-96; OS-C | Later qualification; planned, NOT RUN; adapter support unqualified |
| T-10: Accept only a verified OS/application state outcome | In a future qualified adapter, approve restoring an isolated test service and inspect its resulting health before acceptance | Scoped authority -> real service mutation -> effect capture/readback -> acceptance | Typed effect provenance identifies service, authority, before/after state and verification; independent health readback matches the approved goal; unknown/missing/changed state blocks acceptance | A report saying "done", command exit zero, or mutation of another service is not effect evidence; production services remain untouched | R2 fixed-content integrity is related, not OS-effect coverage | US-08/US-14; AC-97; OS-A | Future adapter/type qualification; planned, NOT RUN; outside current LocalCode/Report |
| T-11: Recover uncertain effects without blind replay | Interrupt OS-A after mutation but before acknowledgement; reconnect/retry | Real local/external effect -> interrupted observation -> reconciliation -> authorized recovery | Effect state is reconciled before replay; no duplicate mutation; ownership/provenance retained; resources outside authority unchanged | Unknown effect cannot be assumed absent or rolled back on an unrelated resource | R8/R9 settlement/replay guards are related, not OS recovery coverage | US-10/US-14; AC-98; OS-A recovery | Future adapter/type qualification; planned, NOT RUN; outside current LocalCode/Report |
| T-12: Organize test files safely and verify the outcome | Approve a preview organizing files within an isolated test-owned directory; interrupt after a move and request recovery | Conversation approval -> scoped filesystem mutation -> reconciliation -> independent manifest/readback -> acceptance | Exact approved destinations and file hashes match; no lost/duplicate files; effect provenance and reconciled retry establish final state before acceptance | Destination collision, changed source, unknown move result or path outside the approved root blocks unsafe mutation/acceptance; unrelated files remain unchanged | R2 integrity and R8 ownership are related, not file-organization effect coverage | US-08/US-10/US-14; AC-97/AC-98; OS-B | Future adapter/type qualification; planned, NOT RUN; outside current LocalCode/Report |

For discussion quality, evaluate understandable language separately, but verify
its factual claims against product-owned state/evidence. Model text cannot prove
routing, authority, progress, rework, effect completion, or acceptance.

**Future scenarios follow product section 11.2:** OS-A restores an isolated
test service (T-10/T-11; replaces this plan's earlier FUTURE-OS-1 label), OS-B
safely organizes test files (T-12), and OS-C adopts/replaces an authorized
existing managed runtime (T-09). All remain unqualified under AC-96 through
AC-98, not renumbered J1-J8 journeys or Report candidates relabeled as OS
outcomes. Before running,
define the supported adapter/effect type, test-owned resource, allowed mutation,
independent state oracle, recovery policy and acceptance guards. Never infer
generic application/OS capability from the current LocalCode/Report pilot.

### 3.2 Result and coverage semantics

| Execution status | Meaning |
|---|---|
| PASS | The named contract crossed its required real boundary and all oracles/negative controls passed on identified evidence for the declared build and stage. |
| FAIL | The supported product was exercised and violated the contract, including incorrect progress, acceptance, or required human orchestration. A timeout caused by product behavior is not an external skip. |
| BLOCKED | An identified external prerequisite (authorization/quota, foreground access, test-host readiness) prevented the required execution. Record the cause and last reached boundary; no pass credit. |
| NOT RUN | The scenario or required boundary was not exercised. A missing naturally occurring J4/J5 trigger is NOT RUN, not a pass. |

Track coverage maturity separately: implemented component/native coverage,
planned/not implemented coverage, and outside-supported-adapter qualification
are not execution results. A committed supported requirement with missing
implementation is an open product gap, not an optional adapter exclusion or
silent skip. Current required gates remain sign-off blockers until demonstrated;
future T-09/T-10/T-11/T-12 qualification does not inflate the current pilot's totals.

## 4. Human-assistance ledger

Record every intervention, not just the final outcome:

| Time / work | Category | Exact action | Why needed | Would work advance without it? | Evidence IDs |
|---|---|---|---|---|---|
| Fill during run | Goal clarification / business judgment / voluntary inspection or contribution / unnecessary prompting / information copying / manual scheduling / repeated goal / internal shell-provider selection / session-context administration / internal binary handoff | User input or manual action | Concrete reason | Yes / no / unknown | Request, event, report, artifact, candidate |

Unnecessary next-step prompting, information copying, manual scheduling,
re-explaining an established goal, choosing an internal shell/agent, compacting
sessions/context, and manually mapping deployed binaries for an internal handoff
are responsibility-transfer failures when needed for ordinary product progress.
Necessary goal clarification, genuine human decisions and voluntary inspection
or contribution are allowed and counted separately; do not relabel required
workarounds as voluntary inspection. Record test-operator setup/provenance in a
separate ledger so collecting hashes for the test does not itself penalize the
product, or conceal a product-required internal handoff.

Report component conformance, live-model behavior, and UI observations
separately. A scripted coordinator cannot earn the autonomous-journey result.

### 4.1 Task-centered unassisted accepted delivery rate

Use [product section 12.1](agent-center-product.md) as the metric contract:

**unique works in Completed state with explicit human acceptance, a verified
goal outcome and zero required human orchestration / all approved in-scope
unique works in the predeclared cohort through the fixed observation window**.

- Freeze scope/build/capabilities, task classes, allowances and the observation
  window before running. Keep every approved in-scope work in the denominator,
  including failed, blocked, unfinished and all postapproval cancelled works. Show
  each reason separately; do not delete hard cases or extend the window to
  improve the score.
- Count a unique work ID at most once. Revisions, retries, replacement candidates
  and repeated acceptance attempts are not new successful works. A human
  acceptance without verified goal evidence, without Completed state, or with
  required orchestration is not numerator success.
- Report raw numerator/denominator and per-work evidence/assistance alongside
  the rate. A zero denominator is **N/A**, never 100%. Do not pool builds or
  confuse two accepted reports with autonomous verified task completion.
- Track natural-language requests through successful work creation as a separate
  intake funnel: request IDs, created work IDs, pre-approval questions,
  initialization failures and unavailable external preflight prerequisites.
  Requests that never create work must remain visible even when the delivery
  denominator is zero; pre-approval questions/preflight blockers are separately
  reported, not invented approved works.
- Record necessary decisions/clarification/voluntary inspection separately from
  product-required assistance and test-operator setup. Unknown assistance cannot
  establish an unassisted success.
- Record observed elapsed time, human effort and resource cost with measurement
  method; do not credit an aspirational numeric speed threshold without measured
  evidence. Future performance targets remain separate from current acceptance.

The metric describes a cohort, not full closed-loop sign-off by itself. Full
current pilot completion additionally requires all named current required
journey gates J1-J9 and applicable R1-R9 gates to pass with true autonomous
coordination on the same identified build, plus two verified, explicitly
human-accepted Completed deliveries. This includes current AC-91 through AC-95,
the conversational main path and the non-substitutable real failed-check J5 loop.

## 5. Packaged integration and release-report wiring

Use `test\e2e\ItE2E` for process/package/UI assertions. Before running live:

```powershell
$env:ITE2E_PACKAGE = 'Dev' # only after explicit selection
pwsh -File test\e2e\bootstrap.ps1 -Check
```

This documentation change adds no suites, release IDs or automation credit.
The T-* cases and J9 are roadmap/specification targets until their real coverage
is implemented and demonstrated. Existing native paste suite titles and their
release mappings remain unchanged; native editor passes are not business
autonomy passes.

In a subsequent implementation change, when adding packaged regression cases,
use the exact R1-R9 checklist titles
above where the suite actually crosses the required boundary. Add unchecked
`[E2E]` rows to `doc\release-check-list.md`, assign IDs with
`test\e2e\Set-ChecklistIds.ps1`, and update the suite table. Do not manually
invent/renumber checklist IDs or mark a case covered merely because a Rust
test with a similar name passed.

Run the added suite through `Invoke-ItE2EReport.ps1`, verify the generated
checkboxes, and verify incremental `Update-ReleaseReport.ps1` mapping.
Keep real-model quota-consuming trials outside default published/CI discovery.

## 6. Sign-off and output

The next verification report must include:

- Exact build/package/provider provenance and settings/source preservation.
- One result per R1-R9 and J1-J9: PASS, FAIL, BLOCKED, or NOT RUN, with evidence;
  T-* coverage maturity/stage separately, including explicit future exclusions.
- Actual regression names, commands and logs; no reused green counts from an
  older executable.
- Before/after acceptance state for B1/B2; received report/diagnostic contents
  for B3; visible attention and draft preservation for B4; real ownership and
  capture sequence for B5; terminal state plus reason for B6.
- The full human-assistance ledger, separate operator-setup ledger, frozen
  cohort/intake funnel, raw metric counts and remaining product limitations.
- Cleanup outcome for test-owned resources only.

Do not sign off if any confirmed defect still reproduces, if the deployed
build cannot be identified, or if required internal progress depended on human
orchestration. Native test-host failures before test execution are BLOCKED,
not product passes or failures. Missing provider approval/quota makes the
real-model stage BLOCKED; a connected product behaving incorrectly is FAIL,
not SKIP.
