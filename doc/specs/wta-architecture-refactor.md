# WTA Architecture Refactor Plan

**Status**: In progress
**Scope**: `tools/wta`
**Strategy**: Incremental, behavior-preserving changes ordered by certainty and architectural return

## Why

WTA has a clear product architecture: a singleton master multiplexes ACP agent
connections, per-pane helpers own the TUI, and one-shot CLI commands expose
Windows Terminal operations. The leaf modules generally reflect that design,
but several root modules accumulated responsibilities as the product evolved.
The first refactor rounds have already reduced some of that concentration:

- `main.rs` now owns process startup, service-mode selection, and top-level CLI
  dispatch; helper runtime and all one-shot command domains have moved out.
- `cli/` owns WT operations, delegation, diagnostics, hooks, probes, and
  sessions commands behind focused execution APIs.
- `app.rs` still owns broad application orchestration and side effects, but
  neutral contracts, tab state, and input editing have moved out.
- `protocol/acp/client.rs` owns transport, UI-facing messages, prompt assembly,
  session orchestration, and turn telemetry.
- `master/mod.rs` owns pipe transport, routing, agent processes, session
  tracking, history synchronization, and recovery.
- `agent_hooks_installer.rs` contains every provider's install, status,
  uninstall, and upgrade behavior.

This makes filenames less predictive, introduces reverse or circular
dependencies, and increases the regression surface of otherwise local changes.

## Goals

1. Make each top-level file accurately describe the code it contains.
2. Establish a one-way dependency flow from entry points to application,
   protocol, and infrastructure modules.
3. Separate pure state transitions from I/O and process-level side effects.
4. Preserve all CLI, ACP, logging, session, and TUI behavior while code moves.
5. Keep every step independently reviewable and releasable.

## Non-goals

- No user-visible feature changes.
- No ACP wire-format changes.
- No CLI argument or command compatibility changes.
- No timeout, retry, logging, or process-lifetime policy changes unless a
  dedicated follow-up explicitly requires one.
- No one-shot rewrite of the crate hierarchy.

## Target dependency direction

```text
main / CLI
    |
    +-- helper ------------+
    |                      |
    +-- master             v
    |                    app/contracts
    +-- cli commands       |
                           v
                     protocol/acp
                           |
                           v
                    shell/platform
```

Shared event and request types must live in a neutral contract module rather
than forcing `app` and `protocol/acp/client` to depend on each other.

## Ordered roadmap

The order prioritizes high-certainty structural improvements before changes
that alter ownership or concurrency boundaries.

| Step | Change | Certainty | Benefit | Main risk |
|---:|---|:---:|:---:|---|
| 1 | Correct package metadata and relocate the helper runtime | High | High | Mechanical move mistakes |
| 2 | Extract neutral app event contracts | High | Very high | Import churn and event visibility |
| 3 | Extract tab state and input editing from `app.rs` | High | High | Accidental state-transition changes |
| 4 | Move failure classification into the ACP failure domain | High | Medium | Misplacing UI policy as protocol policy |
| 5 | Split CLI handlers out of `main.rs` | Medium-high | High | Output and exit-code drift |
| 6 | Split ACP client orchestration by responsibility | Medium | Very high | Session and cancellation ordering |
| 7 | Split agent hooks by provider and operation | Medium-high | High | Provider-specific behavior drift |
| 8 | Decompose master routing, agent pool, and tracking state | Medium | Very high | Lock ordering and lifecycle races |
| 9 | Consolidate and rename session-domain modules | Medium | Medium | Large low-signal import diff |
| 10 | Reorganize remaining protocol/platform utilities | Low-medium | Low-medium | Churn without immediate boundary gain |

## Current progress

| Step | Status | Tracking |
|---:|---|---|
| 1 | Complete | #504 — helper runtime ownership |
| 2 | Complete | #507 — neutral App contracts |
| 3 | Complete | #514 — tab state and input editing |
| 4 | Complete | #517 — ACP failure classification |
| 5a | Complete | #522 — CLI arguments and helper/master runtime configuration |
| 5b | Complete | #526 — probes, hooks, and sessions CLI handlers |
| 5c | Complete | WT one-shot handlers, output formatting, diagnostics, and delegation |
| 6–10 | Not started | Add focused issues only after scope review |

Umbrella tracking issue: #515.

## Step 1: Package description and helper ownership

**Status**: Complete

### Design

The helper module owns the complete per-pane runtime:

```text
src/helper/
  mod.rs       # public helper-mode entry point
  config.rs    # runtime-only helper startup configuration
  runtime.rs   # WT connection, terminal lifecycle, channel wiring, event loop
```

`main.rs` retains argument parsing and mode dispatch. It calls
`helper::run_helper_mode(config, pipe_name)` and does not contain helper TUI
implementation details.

### Code movement

Move the following helper-only implementation into `helper/runtime.rs`:

- `run_default_tui_over_pipe`
- `discover_pane_identity`
- `TuiRestoreGuard`
- `run_acp_tui_mode`
- `connect_to_wt_protocol`
- `spawn_restart_agent_stack_forwarder`
- `run_acp_app`

Keep `run_test_pipe` and `run_info_mode` in the CLI entry area because they are
one-shot diagnostic commands, not helper runtime behavior.

### Invariants

- Preserve statement order and function bodies except for module-qualified
  imports required by the move.
- Preserve `LocalSet`, task spawning, and channel topology.
- Preserve terminal raw-mode and alternate-screen restoration.
- Preserve `process::exit`, log flushing, and error-reporting behavior.
- Preserve all logging targets and messages.
- Preserve the `Cli` structure for now; extracting `HelperConfig` belongs to
  the later CLI-boundary step.

### Acceptance criteria

- `helper/mod.rs` no longer calls a crate-root helper implementation.
- `main.rs` contains no helper TUI runtime functions.
- The public command and argument surface is unchanged.
- Formatting, WTA tests, Clippy, and the WTA build pass.

## Step 2: Neutral event contracts

**Status**: Complete

Extract `AppEvent` and protocol/UI-neutral payloads such as permission and plan
records into an application contract module. Both the App reducer and ACP
client will depend on that module, removing the direct
`app <-> protocol/acp/client` dependency cycle.

Do not redesign event variants in this step. First establish ownership, then
make later event-model changes independently.

## Step 3: App state and input boundaries

**Status**: Complete

Move `TabSession`, input history, cursor editing, and command completion state
into focused modules. Keep application side effects and complex transitions on
`App` until the state extraction is complete.

The existing `app/turn_state.rs` and `app/autofix.rs` are the model: pure data
and small pure helpers, with orchestration outside the state type.

## Step 4: ACP failure-domain behavior

**Status**: Complete

Move predicates that only interpret `AgentFailure` and handshake stages into
the ACP failure module. Keep product recovery policy in `App`; protocol types
should answer classification questions without deciding which UI to show.

## Step 5: CLI boundary

**Status**: Complete

Introduce a `cli/` module for argument definitions, command dispatch, WT
operations, sessions, hooks, probes, and delegation. Reduce `main.rs` to:

1. Parse arguments.
2. Initialize logging, locale, telemetry, and process hooks.
3. Dispatch to master, helper, or one-shot CLI handlers.
4. Flush and return the final result.

### Completed slices

`#522` established the entry-point boundary:

- `cli/args.rs` owns Clap argument and subcommand types.
- `HelperConfig` and `MasterConfig` contain runtime-only startup values.
- Helper and master production code no longer depend on `Cli` or Clap.

`#526` extracts three cohesive one-shot command domains:

- `cli/hooks.rs` owns `hooks install/status/uninstall` execution and output.
- `cli/probes.rs` owns ACP model/source/session and host/WSL diagnostic probes,
  including their force-exit and log-flush behavior.
- `cli/sessions.rs` owns sessions-list access to `wta-master`, filtering,
  formatting, and delegate born-bound session registration.

This reduces `main.rs` from 2,219 to 1,480 lines. Top-level command matching
remains in `main.rs`; command output, exit codes, ordering, aliases, and
process-lifetime behavior are unchanged.

The final CLI-boundary slice extracts the remaining command domains:

- `cli/wt.rs` owns WT one-shot command plumbing, human/JSON output formatting,
  pipe-id, set-env, listen, and legacy protocol diagnostics.
- `cli/delegate.rs` owns launch policy, WSL routing, context enrichment, and
  born-bound session registration.
- `cli/mod.rs` provides narrow top-level routing to each command domain.

This reduces `main.rs` to process initialization, legacy-flag compatibility,
service-mode selection, CLI dispatch, and shutdown handling. Lower-level ACP,
WT, and session details remain private to their owning command modules.

## Step 6: ACP client decomposition

Split transport connection, request types, session orchestration, prompt
construction, ACP callbacks, and turn metrics. Preserve one orchestration
facade so callers do not depend on transport internals.

This starts only after neutral event contracts exist, otherwise the current
cycle would merely be spread across more files.

## Step 7: Agent hooks decomposition

Create provider modules for Copilot, Claude, Gemini, Codex, and OpenCode.
Shared bundle resolution, command execution, reporting, and upgrade-state
storage remain common infrastructure. Provider modules implement explicit
install, status, uninstall, and upgrade operations.

## Step 8: Master decomposition

Extract cohesive stateful components:

- `SessionRouter`: session-to-helper routes and helper disconnect cleanup.
- `AgentPool`: process selection, spawn, reuse, and reaping.
- `SessionTracking`: hook-owned, born-bound, orphan, and watcher state.
- `HistorySync`: host and WSL history refresh and title seeding.
- `PipeServer`: named-pipe security, accept loop, and helper serving.

This step must define and test lock ordering before moving ownership. It is the
highest-risk phase and intentionally follows the simpler boundary work.

## Step 9: Session-domain naming

After dependencies stabilize, consolidate the overlapping session modules
under a `session/` namespace with names based on responsibility, such as
`model`, `reducer`, `live_registry`, `activation`, `history_scan`, and
`acp_mapping`.

Renaming comes late because doing it earlier creates broad import churn without
first improving dependency direction.

## Verification policy

Every behavior-preserving step should:

1. Format only changed Rust files; full-crate formatting currently creates
   broad line-ending churn.
2. Run the complete WTA test suite.
3. Run Clippy for all targets.
4. Build the WTA binary.
5. Inspect the diff for changed constants, messages, timeouts, task ordering,
   channel ownership, error propagation, and visibility.

For concurrency-sensitive master or ACP steps, add focused tests before moving
ownership and perform live helper/master verification when the change reaches
runtime lifecycle code.
