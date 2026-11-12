# Unit-test inventory

These are behavior cases, not a mandate to test trivial getters, framework
bindings, or every private helper. Prefer table-driven cases in existing test
files and existing fake ACP/process boundaries. No new test framework is needed.
Implementation/execution coverage is recorded separately in `validation.md`;
listing a case here does not claim that its code exists or passed.

## Settings and conversion

| ID | Input/action | Required observation |
| --- | --- | --- |
| S1 | Old JSON lacks source settings | Both sources resolve to host; existing JSON remains compatible. |
| S2 | Save/copy/reload Windows and WSL source settings | Exact provider and distro survive, independent ACP/delegate values remain independent. |
| S3 | Explicit unknown source, empty WSL name, unsafe name, malformed qualified selection | Invalid source is not host fallback; no launchable binding. |
| S4 | Source-qualified choice conversion | Round trip Windows, Ubuntu, Debian, known providers; no provider ID pollution or distro aliasing. |
| S5 | Policy blocks provider/custom agents | Same restriction for host and every WSL distro; raw saved value is not a bypass. |

## Discovery and picker view models

| ID | Input/action | Required observation |
| --- | --- | --- |
| V1 | No WSL installed | Windows choices and custom editor remain usable; no synthetic WSL choice. |
| V2 | Multiple distros with different installed agents | All valid choices are labelled by exact distro, deduplicated and deterministic. |
| V3 | PowerShell, cmd, WSL, profile defaults | Same discovered WSL candidates are available, independent of profile command. |
| V4 | Select global WSL ACP or delegate entry | Store bare provider plus exact source; only that selection changes. |
| V5 | Same provider, change host/Ubuntu/Debian | Selection and model probe change; equality does not short-circuit on bare provider ID. |
| V6 | Select Windows built-in/custom command after WSL | Source resets to host; unrelated custom settings and BYOK selection remain intact. |
| V7 | Saved agent/distro absent or discovery fails | Saved choice remains visible as unavailable; persistence is not rewritten. |
| V8 | Rebuild lists with a selected item | No reentrant null/first-item write resets settings; SelectedItem still represents the stored source. |
| V9 | Late discovery after navigation/settings replacement | Publish only to current live VMs on UI thread; old snapshot cannot mutate a new editing clone. |
| V10 | Inherit global from a profile | Label identifies provider and location; empty profile value remains inheritance, not a copied override. |
| V11 | Unrelated settings reload versus changed source | Preserve live editor for unrelated reload; reconstruct when the effective source changes. |
| V12 | Failed/partial/malformed discovery payload | Explicit unknown/error, not authoritative empty success; preserve unavailable selections. |
| V13 | Utility, unsafe, duplicate distribution names | Exclude utility/unsafe entries; no duplicate choices or command injection. |
| V14 | Cold ordinary load of previously generated native WSL profiles | Restore saved provider/source evidence without Linux execution. Explicit successful Settings discovery supersedes stale hints; per-source errors retain known metadata. |
| V15 | Open global Agents or profile agent-picker page | Actively probe registered distros, including stopped ones, on a background thread; publish current choices without creating ACP/model processes. |
| V16 | General Settings construction, settings-file reload, or reload restoring Agent-page navigation | No active WSL discovery; preserve current/saved evidence and unsaved editor state. |
| V17 | Navigate repeatedly while discovery is pending | One in-flight discovery; completion targets live VMs without forced global settings reload. |

## Binding, reload, and model behavior

| ID | Input/action | Required observation |
| --- | --- | --- |
| B1 | Global WSL agent with PowerShell/cmd source pane | Launch exact WSL provider/distro; helper stays Windows; terminal target stays original pane. |
| B2 | Global WSL agent with a different WSL profile | Agent runs in chosen distro; do not require shell equality or confuse cwd namespaces. |
| B3 | Profile host/WSL override and explicit tab override | Tab > profile > global, with no silent fallback on an invalid explicit selection. |
| B4 | Only global source changes | Change detected even for same provider; global followers update, explicit overrides retain source/session ownership. |
| B5 | Stashed global-following agent source changes | Correct binding on restore without focus theft or accidentally destroying unrelated sessions. |
| B6 | Global model inheritance | Only matching provider/source inherits; Windows override cannot inherit a different WSL catalog's model. |
| B7 | Probe returns host/Ubuntu/Debian catalogs | Independent cache keys; identical provider IDs do not merge catalogs or current model IDs. |
| B8 | Probe A completes after selection B | A is discarded; B's preparing/error/model state is not overwritten. |
| B9 | WSL probe success, preparation/session/cleanup failure, retry | Source flows through startup key and process cleanup; accurate preparation/error UI; bounded retry. |
| B10 | Host BYOK configured, select WSL then host | WSL shows native models and uses distro environment; saved host BYOK resumes on host. |
| B11 | Persist/restore WSL agent on Windows tab | Qualified identity restores correct distro, not merely the provider or current shell. |
| B12 | Missing/blocked selected WSL agent | Failure stays attached to chosen source; no accidental master/default host launch. |
| B13 | Existing Windows agent behavior | Native model hot update, provider rebind, custom editing, YOLO policy, hooks/session availability unchanged. |
| B14 | WSL native model A to B while host BYOK remains saved | Snapshot retains the WSL native model; source-qualified model update reaches only followers. |
| B15 | Explicit `/agent` WSL request from PowerShell/cmd or another distro | Existing window/tab ownership and policy checks hold; no obsolete same-shell restriction. |

## Rust decisions and protocol boundaries

| ID | Input/action | Required observation |
| --- | --- | --- |
| R1 | WSL enumeration UTF-16/UTF-8, BOM, CRLF, empty/malformed output | Correct bounded parsing or explicit error; no garbage distro names. |
| R2 | All-distro probe versus existing single-distro mode | Compatible single-distro payload; all mode reports each exact source and its failures. |
| R3 | Distribution probe timeout/failure | Other results retained, failure reported, concurrency bounded, no installation/hook mutation. |
| R4 | WSL model probe | Source-aware start/registry key; absolute target Linux cwd; host default remains unchanged. |
| R5 | Delegate global WSL from Windows path | Launch and registered session use translated Linux cwd, not raw Windows path. |
| R6 | Delegate same-distro versus different-distro POSIX cwd | Reuse only same known namespace; otherwise target home; native Windows launch unchanged. |
| R7 | Source-qualified process/session identity | Host and WSL same provider do not share process or session ownership; existing session MCP routing remains exact. |
| R8 | Ordinary startup/settings load with registered unused WSL agents | No active distro discovery, Linux profile-generation probe, or unused ACP/history startup. |
| R9 | Distro already running or started by Settings discovery | Running/discovered state alone authorizes no chat/history ACP pair; Windows eager behavior remains unchanged. |
| R10 | Exact WSL provider/source selected as default or actually used | Only that pair becomes eligible; another provider in the same distro and the same provider in another distro stay lazy. |
| R11 | Running-state query fails or races with shutdown | State uncertainty grants no ACP permission; explicit Settings discovery/use remains possible and reports real failures. |
| R12 | WSL native agent profiles | Stable provider+distro GUID, exact launch distro, policy filtering, separate native customization, no accidental Windows provider profile duplication. |
| R13 | Settings load/reload while explicit discovery is slow | Generate immediately from cached/saved evidence; no extra Linux probe, dispatcher wait, forced reload, or lost unsaved edits. |
| R14 | Persisted stashed non-default WSL conversation on app restart | Retain deferred owner/session/view/cwd/geometry without a helper/ACP/distro start; serialize it again, omit it when session persistence is disabled, and materialize it only on explicit open/resume. |
| R15 | WSL native agent versus explicit shell command | Managed native agents never enter shell-integration sweeps; a user override to an interactive WSL shell retains existing behavior. |
| R16 | Passive all-WSL CLI versus explicit `--start-stopped-wsl` | Passive mode skips stopped distros; active Settings mode checks native executables in every registered eligible distro, without installation, models, or ACP. |

## ACP cwd namespace debt (#265)

| ID | Input/action | Required observation |
| --- | --- | --- |
| C1 | Rooted Windows drive, including extended-length form; target maps it under `/windows/c` | Use the distro-supplied path, never a fabricated `/mnt` value; no extra home query on success. |
| C2 | `wslpath`/directory check fails, times out, or the Windows drive is not mounted | Diagnose and use actual distro home; syntax-only translation is insufficient. |
| C3 | Native POSIX, Unicode, same/foreign-distro WSL UNC, true UNC, drive-relative and relative paths | Preserve only the known source namespace; foreign/ambiguous values use home, relative values resolve there. No accidental drive-relative mapping. |
| C4 | Complete same-instance session/list contains Windows, POSIX, mixed, relative, UNC, empty or no rows | Infer only unambiguous consensus; partial or other-generation evidence never locks a namespace. |
| C5 | Built-in Windows agent with real System32 cwd; known WSL agent with an already-normalized cwd | Preserve actual cwd; existing host history startup and WSL eligibility do not change. |
| C6 | Opaque custom wrapper, empty/unsupported/in-flight history or stale namespace evidence | No extra history ACP/list request or wait; preserve original namespace first when unknown, with at most one opposite-namespace fallback. Never guess a distro/mount root. |
| C7 | Primary deterministic cwd preflight error versus echoed data, authentication, unrelated path-prefix/file error or timeout | Only the cwd rejection permits a second creation; at most two distinct attempts. |
| C8 | First creation rejected, fallback succeeds or consumes remaining timeout | One total deadline, one completion telemetry event, and registry cwd equals the successful candidate. |
| C9 | Real resume fails after cwd resolution | Exactly one session/load; preserve routing/MCP rollback and cleanup reserve. |
| C10 | Orphan/already-loaded rebind with a different helper cwd and a same-ID foreign-provider row | No orphan load/probe; preserve actual qualified cwd and current metadata, without borrowing the foreign row. |

## Integration/manual proof (not unit-test substitutes)

Open the deployed isolated Settings page, choose WSL globally, save/reopen, then
open agent pane from PowerShell and cmd. Check actual agent process/distro,
owning shell, cwd, native model list, ordinary chat and a confirmation-gated shell
action. Exercise profile override, source switch, stash/restore, and delegation.
Verify keyboard and accessible selection labels. Verify the isolated PFN/layout,
binary hashes, exact COM identity, and unchanged regular package registration.
Use installed production hooks without installing/updating/removing them.
