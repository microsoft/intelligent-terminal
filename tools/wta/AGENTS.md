# WTA Agent Architecture

## What is WTA?

WTA (Windows Terminal Agent) is a Rust binary that bridges AI agent CLIs with
Windows Terminal. It is built around a **helper + master** architecture (see
`doc/specs/Multi-window-agent-pane.md`) and runs in one of three roles, selected
at startup by flags / subcommands — **there is no standalone agent / TUI mode and
no MCP server**; bare `wta` with neither `--master` nor `--connect-master` exits
with an error.

- **`wta-master`** (`--master <pipe>`, spawned once by the C++ `SharedWta`
  singleton) -- the ACP **multiplexer**. Owns the *single* `ACP/stdio`
  connection to the agent CLI subprocess (Copilot, Claude, Gemini, Codex, or a
  custom command), listens on a named pipe, and fans per-helper ACP sessions
  onto that one agent CLI. Implementation: `src/master/mod.rs`.
- **`wta-helper`** (`--connect-master <pipe>`, spawned once per agent pane by
  Windows Terminal) -- the per-pane **TUI**. Drives the ratatui chat UI (`app.rs`)
  but, instead of spawning its own agent CLI, speaks ACP/JSON-RPC to master over
  the pipe. *From the helper's perspective, master is the agent.* Entry:
  `src/helper/mod.rs` → `run_default_tui_over_pipe`.
- **CLI helpers** (`wta list-panes`, `wta capture-pane`, `wta new-tab`,
  `delegate`, `hooks`, `sessions`, …) -- one-shot WT-control commands for humans
  and for agents that can shell out. Direct keystroke injection is not exposed by
  the CLI. Dispatched in `src/main.rs`.

The helper side owns `ShellManager`, which services the agent CLI's ACP
`create_terminal` / permission requests by routing to either a local subprocess
or a Windows Terminal pane. WT pane operations use a `WtChannel` abstraction with
a single implementation today:

- `CliChannel` shells out to `wtcli.exe`, which calls WT's COM `IProtocolServer`.
  All WT operations — including `send_input` (via `wtcli send-keys`) — go through
  this path.

## System Diagram

```
            Windows Terminal (WindowEmperor, one WT process)
              |  spawns once (SharedWta)      |  spawns per agent pane
              |  --master <pipe>              |  --connect-master <pipe>
              v                               v
        +--------------+   named pipe   +------------------+
        | wta-master   |<-------------->| wta-helper       |  (one per pane)
        | (singleton)  |  ACP/JSON-RPC  | TUI: app.rs +    |
        | master/mod.rs|                | helper/mod.rs    |
        +------+-------+                +--------+---------+
               |  ACP/stdio                      |  ShellManager
               v                                 |  (create_terminal /
         Agent CLI                               |   permission)
      (copilot/claude/                           v
       gemini/codex)                        CliChannel
                                                 |
 Human / agent shell-out:                        v
   wta <subcommand>  ----------------->  wtcli.exe -> COM IProtocolServer
   (main.rs CLI helpers)                         |
                                                 v
                                    TerminalProtocolComServer
                                                 |
                                                 v
                                         Windows Terminal
```

## Protocol Stack

### ACP (Agent Client Protocol)

ACP (`agent-client-protocol = "0.10"`, JSON-RPC 2.0) is spoken on **two hops**,
because of the helper+master split:

- **master ↔ agent CLI** (stdio): master is the ACP **client** of the agent CLI
  subprocess — the same role legacy single-process wta used to play. It spawns
  the agent CLI and owns its stdin/stdout.
- **helper ↔ master** (named pipe): master is an ACP **agent** (server) to each
  helper, and the helper is the ACP **client**. Master forwards helper requests
  to the agent CLI and routes inbound `session_notification`s back to the helper
  that owns the session (`session_to_helper` map in `src/master/mod.rs`).

Implementations: agent-CLI client + helper-side `WtaClient` in
`src/protocol/acp/client.rs`; the master multiplexer in `src/master/mod.rs`.

The agent sends requests (`create_terminal`, `request_permission`, etc.) which
ultimately land on the owning helper's `WtaClient`; session notifications
(message chunks, tool calls, plans, status changes) flow agent CLI → master →
helper.

Key ACP message types handled:

- `session/update` -- agent message chunks, tool calls, plan entries
- `request_permission` -- permission dialog with options (allow/reject)
- `create_terminal` / `terminal_output` / `wait_for_terminal_exit` -- agent-managed shells
- `release_terminal` / `kill_terminal` -- cleanup

### WT COM Protocol

All WT operations flow through `wtcli.exe` to WT's out-of-process COM server.

- Client wrapper: `src/shell/wt_channel/cli_channel.rs`
- CLI executable: `src/tools/wtcli/main.cpp`
- IDL: `src/cascadia/TerminalProtocol/TerminalProtocol.idl`
- WT-side server: `src/cascadia/WindowsTerminal/TerminalProtocolComServer.cpp`
- Discovery: `WT_COM_CLSID`, injected into panes by WT

The COM surface exposes reads and mutations, including `list_*`, `read_pane_output`, `create_tab`, `split_pane`, `close_pane`, `focus_pane`, `send_input` (via `wtcli send-keys`), and event subscribe/publish.

## Agent Integration

### Copilot

```
wta --agent "copilot --acp --stdio"
```

Copilot speaks ACP directly (`--acp --stdio`). It is spawned by `wta-master`, not
by the helper. The agent reaches Windows Terminal by shelling out to the `wta` /
`wtcli` CLI helpers (which call WT's COM `IProtocolServer`); WTA no longer
generates an MCP config or runs an MCP server for the agent.

### Claude and Codex

Claude and Codex are launched through ACP adapters:

```
wta --agent "npx -y @agentclientprotocol/claude-agent-acp"
wta --agent "npx -y @zed-industries/codex-acp"
```

The Terminal settings layer resolves the built-in agent IDs to these adapter commands.

### Gemini

```
wta --agent "gemini --experimental-acp"
```

### Custom agents

Custom agents are configured through the Terminal settings (`custom:<cmd>` plus the stored custom command). WTA receives the resolved command line via `--agent`.

## CLI Helpers

Agents that can shell out, and humans debugging WTA, can use WTA as a small WT helper CLI:

| Command | Alias | WT protocol method |
|---|---|---|
| `list-windows` | `lsw` | `list_windows` |
| `list-tabs` | `lst` | `list_tabs` |
| `list-panes` | `lsp` | `list_panes` |
| `new-tab` | `neww` | `create_tab` |
| `split-pane` | `splitw` | `split_pane` |
| `capture-pane` | `capturep` | `read_pane_output` |
| `kill-pane` | `killp` | `close_pane` |
| `active-pane` | -- | `get_active_pane` |
| `wait-for` | -- | delegated to `wtcli wait-for` |
| `pane-status` | -- | `get_process_status` |
| `listen` | `mon` | COM event subscribe |

## Connection Discovery

`CliChannel` uses `wtcli.exe`, and `wtcli.exe` discovers WT through `WT_COM_CLSID`. WT injects this environment variable into pane shells.

`pipe-id` and `set-env` are diagnostic subcommands that surface the inherited `WT_COM_CLSID` value. They should not be described as a security boundary.

## Pane Identity

WTA discovers which WT pane it is running in by PID matching:

1. Call `list_windows` -> `list_tabs` -> `list_panes` through `CliChannel`.
2. Each pane has a `pid` field.
3. Match against `std::process::id()`.
4. Store `(pane_id, tab_id, window_id)` in app state.
5. Display the identity in the status bar.

## Logging

WTA writes structured logs under the package-private log dir, in a per-version
subfolder keyed by the package version:

```
…\Packages\<PFN>\LocalCache\Local\IntelligentTerminal\logs\<pkgver>\   (packaged)
%LOCALAPPDATA%\IntelligentTerminal\logs\                                (unpackaged / dev)
```

Per-process logs in the helper+master architecture:

- `wta-main_master.log` -- `wta-master`: agent CLI spawn, pipe accept loop,
  per-helper routing, `session_to_helper` updates, agent CLI exit detection
- `wta-main_helper-{pid}.log` -- each `wta-helper` (one file per PID): pipe
  connect, ACP initialize, `session/new`, prompts, agent responses, TUI lifecycle
- `wta-cli.log` -- short-lived CLI helpers (`list-*`, `capture-pane`, `listen`,
  `sessions`, …); daily-rotated, 3-day retention
- `wta-delegate.log` -- `wta delegate` (`?<prompt>` delegation)
- `wta-probe.log` -- `probe-models`
- `wta-install-hooks.log` -- `hooks install` / uninstall
- `wta-ensure-host.log` -- WT-side background ensure-running / SharedWta lifecycle
- `wta-acp-debug.log` -- low-level ACP JSON-RPC wire trace

Two files in the same dir are written by **non-Rust** producers:
`terminal-agent-pane.log` (C++ `AgentPaneLog`) and `hook-trace.log` (PowerShell
hooks). See the repo-level architecture notes for the full storage layout.

The log level is controlled by `WTA_LOG` (or `RUST_LOG`); if unset, debug builds
default to `debug` and release builds to `info` (`logging::default_filter_directive`).

## Build Rule

For normal local WTA development, always produce the binary at `tools/wta/target/debug/wta.exe`.

- Before running `cargo build` for WTA, kill any active `wta.exe` processes first. A live shared-host session can keep `target/debug/wta.exe` locked and make the build fail with `Access is denied`.
- Preferred PowerShell sequence:
  - `Get-Process wta -ErrorAction SilentlyContinue | Stop-Process -Force`
  - `cargo build --manifest-path tools/wta/Cargo.toml`
- Do not switch to an alternate `--target-dir` just to work around a locked `wta.exe` unless that is explicitly the task. The default expectation is to refresh `tools/wta/target/debug/wta.exe`.

## Test Rule

For any WTA change that is covered by — or should be covered by — unit tests,
**run the WTA test suite locally before committing/pushing.** `cargo build` and
the C++ F5 / `bcz` flow do **not** compile or run the `#[cfg(test)]` code, so a
green build says nothing about the tests.

- Kill any live `wta.exe` first (same as the Build Rule), then run from the repo
  root so the manifest's toolchain pin doesn't force the unavailable channel:
  - `cargo test --manifest-path tools/wta/Cargo.toml`
- All tests must pass before you push. CI runs the same `cargo test` and fails
  the build on any failure
  (`build/pipelines/templates-v2/job-build-project.yml`), so a local run just
  catches it earlier.
- Run the suite even when the change "looks trivial" — especially for tested
  logic like `protocol/acp/client.rs`, `ui/chat.rs`, and permission handling.
  The mock-ACP (`protocol/acp/mock_agent_tests.rs`) and render harnesses guard
  real behavior and have caught real regressions.

## Key Crates

| Crate | Version | Purpose |
|-------|---------|---------|
| `agent-client-protocol` | 0.10 | ACP client library |
| `tokio` | 1 | Async runtime |
| `ratatui` | 0.30 | TUI rendering |
| `crossterm` | 0.29 | Terminal I/O |
| `clap` | 4 | CLI parsing |
| `serde_json` | 1 | JSON handling |

---

## Third-party Rust crate attribution

`wta.exe` statically links many third-party crates. Their attribution lives
in two generated artifacts:

| Artifact | Purpose | Hand-maintained? |
|---|---|---|
| `tools/wta/cgmanifest.json` | Microsoft Component Governance manifest. CG tooling auto-discovers this file and ingests it for OSS compliance, vulnerability scanning, and IP review. Sits alongside the existing `oss/<name>/cgmanifest.json` files for the inherited C++ packages. | **No — generated.** |
| The `<!-- BEGIN wta-rust-deps -->` block in `/NOTICE.md` | Human-facing third-party notice text (one bullet per `(name, version)` plus one canonical license-text section per unique atomic SPDX identifier). | **No — generated.** |

Both artifacts are regenerated by a single PowerShell script that lives
next to `Generate-ThirdPartyNotices.ps1` (the existing MD-to-HTML converter
the build pipeline runs):

```powershell
$env:RUSTUP_TOOLCHAIN = 'stable'   # bypass the repo's rust-toolchain.toml pin
pwsh -File .\build\scripts\Generate-WtaThirdPartyNotices.ps1
```

The script requires **PowerShell 7+** (`pwsh.exe`). It fails fast with a clear
message under Windows PowerShell 5.1 because `ConvertFrom-Json` / `ConvertTo-Json`
indent differently across versions and Windows PowerShell 5.1's `ConvertFrom-Json`
caps payloads below the size of `cargo metadata` output for this dep tree.

The script:

1. Runs `cargo metadata --filter-platform x86_64-pc-windows-msvc` against
   `tools/wta/Cargo.toml` — cargo handles the target-cfg filtering, so the
   script keeps no hand-rolled OS allow / deny list.
2. Walks the resolved graph keeping only normal (non-dev, non-build) deps,
   then attributes every reachable `(name, version)` package — both versions
   of crates that appear in the lockfile twice (e.g. `syn` v1 + v2) are
   included.
3. Normalizes SPDX expressions (`X/Y` → `X OR Y`, sort tokens in pure-OR
   expressions, decompose composites like `(MIT OR Apache-2.0) AND
   Unicode-3.0` into atomic tokens so each required license text is
   reproduced exactly once).
4. Sources each license text from the local Cargo registry cache (extracted
   `~/.cargo/registry/src/.../<crate>-<version>/`, then the gzipped `.crate`
   tarball via bundled `tar.exe`), with a best-effort fall back to
   `raw.githubusercontent.com/<owner>/<repo>/HEAD/LICENSE` when the upstream
   tarball does not bundle one.
5. Writes `tools/wta/cgmanifest.json` (Component Governance Cargo schema,
   sorted alphabetically for stable diffs).
6. Splices the regenerated block into `/NOTICE.md` between the
   `<!-- BEGIN wta-rust-deps -->` and `<!-- END wta-rust-deps -->` markers,
   atomically and preserving the file's CRLF line endings.

### When to re-run

Re-run the generator and commit the result whenever any of the following
changes the Rust dependency graph:

- A direct dependency in `tools/wta/Cargo.toml` is added, removed, or
  upgraded.
- `cargo update` substantially shifts `tools/wta/Cargo.lock`.
- Feature flags on a direct dependency change in a way that pulls in or
  drops transitive crates.

Inspect the diff to `tools/wta/cgmanifest.json` and `/NOTICE.md` and
include both in the same commit as the underlying Cargo change.

### Boundary with `oss/<name>/`

The C / C++ packages under `oss/` are separately tracked source-code
mirrors maintained by hand (see e.g. `oss/chromium/MAINTAINER_README.md`).
Each has its own `cgmanifest.json` checked in alongside the imported
source. This generator covers only the Rust crates that ship inside
`wta.exe`; it does not touch the `oss/` tree.

---

## Session liveness + Enter routing (session management view)

The "Session management" view (a.k.a. `/sessions`) lists every
agent session that WTA knows about — both currently connected ("Live")
and replayed from on-disk history ("Historical" / "Ended"). Enter and
Shift+Enter on a row are routed through a closed-form state machine
that splits the legacy one-dimensional `AgentStatus` into a
**two-dimensional** model.

### The two axes

* **Activity** — `Idle | Working | Attention | Error`. Surfaces what
  the connected agent is *doing* right now. Driven by ACP
  `session/update` (`ToolCall`, `ToolCallUpdate`) on Class A
  sessions, and by shell-integration hooks on Class B.

* **Liveness** — `Live { pane_session_id } | Ended | Historical`.
  Surfaces whether the session is currently attached to a process.
  `Historical` = reconstructed from disk only; `Ended` = was Live
  in this WTA process at some point, then closed.

The legacy `AgentStatus` field is still the storage; `activity()`
and `liveness()` derive from it (see `agent_sessions.rs`).

### Class A vs Class B

* **Class A** (`SessionOrigin::AgentPane`) — created by WTA on behalf
  of an Intelligent Terminal agent pane (recorded in
  `agent-pane-sessions.jsonl`). For these rows the natural Enter
  target is the *same* agent pane; the natural Resume target is ACP
  `session/load` so the conversation rehydrates in-place.

* **Class B** (`SessionOrigin::Unknown`) — user ran the CLI directly
  (`copilot`, `claude`, `gemini`) in a normal pane. The natural
  Enter target is to focus that pane; the natural Resume target is
  the CLI's own `--resume` flag (because the CLI owns the
  conversation, not WTA).

### Liveness data sources (composite)

* **Class A** is composite: `liveness = Live` requires *both*
  `alive_mirror.lookup(sid).is_some()` (the master-pushed registry
  mirror via `intellterm.wta/session_added|removed` ext
  notifications) *and* no local `PaneClosed` tombstone. Local
  pane-close events trump the mirror so the row flips to `Ended`
  immediately, even if master's `session_removed` notification
  hasn't landed yet.

* **Class B** is tracked by a **hybrid** of three producers (full design:
  [`doc/specs/hybrid-agent-session-tracking.md`](../../doc/specs/hybrid-agent-session-tracking.md)):
  a real PowerShell **hook** owns a session outright; #266 **born-bound**
  (delegate `?<prompt>` / `/sessions` resume) owns only its pane binding; and a
  file/process **watcher** is the fallback — it surfaces user-typed sessions and
  supplies **status** for born-bound sessions that have no hook. The master keeps
  two disjoint sets, `hook_owned` and `born_bound`, so the three never
  double-track (`master/mod.rs`: `apply_watcher_event` / `handle_session_hook`).

### Status (Working / Idle / Attention) from the log

When a Class-B session has no hook, the watcher derives status from the CLI's
transcript (`session_watcher/classify_*.rs` -> `ToolStarting` = Working /
`ToolCompleted` = Idle / `Notification` = Attention):

* **Claude** — turn-based, keyed on `stop_reason` (a `user` record -> Working;
  assistant `stop_reason:tool_use` -> Working, `AskUserQuestion` -> Attention;
  `end_turn` -> Idle). Claude re-writes the same message id while streaming, so
  keying on content would flicker — `stop_reason` is stable.
* **Copilot / Codex** — turn-based over their append-only logs. Working is
  bracketed by the turn boundary (`assistant.turn_start`/`turn_end` for Copilot,
  `event_msg/task_started`/`task_complete` for Codex), not by the brief tool
  windows; a user-input tool *or* an explicit permission/escalation record
  (`permission.requested` / sandbox `require_escalated`) -> Attention.
* **Gemini — Working-only (turn-based Idle deferred)**: Gemini's
  `session-*.jsonl` is an append log (single-message records + `$set` ops),
  read by byte offset like the others. `classify_record` **skips every `$set`
  op** (crucially the start/resume `$set:messages` snapshot, so a resume can't
  replay history) and maps each activity record to Working: a `user` record
  (prompt or `functionResponse`), a `gemini` text record, or a `gemini` with
  `toolCalls` (`ask_user` -> Attention). It **never emits Idle** — Gemini writes
  no turn-completion signal and a completed `toolCall` doesn't mean the turn
  ended, so a row stays Working until `PaneClosed`. A clean turn-based Idle is
  deferred (needs a turn-end marker Gemini doesn't write).
* **Limitation (permission / ask-for-input)**:
  * **Claude** — no permission marker (only `permissionMode`), so a permission
    prompt (`Bash`/`Edit` in default mode) is indistinguishable from a running
    tool -> **Working** (only the explicit `AskUserQuestion` tool is Attention).
  * **Gemini** — the transcript is written **post-completion** (every on-disk
    `toolCall` is `status:success` with its result, and `ask_user` already holds
    the answer), so the wait window shows **Working**; the `ask_user` ->
    Attention mapping is kept but is typically superseded by the following result
    record. Reliable wait-state Attention needs hooks.
  * **Copilot / Codex** *do* surface permission waits as Attention via
    `permission.requested` / `require_escalated`.

### Cold-startup race

History scan and alive-mirror bootstrap arrive in either order. Both
paths post `AppEvent::AliveJoinUpgrade`, which calls
`AgentSessionRegistry::apply_alive_session_join` to upgrade
**Historical** rows whose ACP session_id is in the alive mirror to
`LivenessState::Live`. **Ended** rows (locally tombstoned by a
`PaneClosed` event in this process) are **not** upgraded — local
tombstones are authoritative, so a stale `session_added` broadcast
arriving after `PaneClosed` cannot resurrect a row that has no
demotion path back. Live rows are never demoted — preserves
tool/attention state. See `agent_sessions.rs::apply_alive_session_join`.

### Steady-state alive-mirror updates

Every `intellterm.wta/session_added` notification from master also
runs the incremental join synchronously in `app.rs` (calls
`apply_alive_session_join([(sid, pane)])` before the async mirror
upsert), so a Historical row matched by an arriving alive broadcast
becomes Live in the same tick. Every `intellterm.wta/session_removed`
notification calls `apply_master_session_ended(sid)` — the
counterpart of `apply_alive_pane_snapshot` for a single explicit
disappearance — which demotes a Live row to Ended, clears the pane
binding, and prunes `known_alive_panes`. Without these two
synchronous reducer calls, session management rows would stay frozen at whatever
state the last bootstrap snapshot saw.

### Enter / Shift+Enter dispatch

The pure-function core is `session_mgmt::decide_enter_action`. It
takes a `RowSnapshot` (origin + liveness + cli + agent capabilities)
and a `shift` boolean, and returns one of:

* `Focus { pane_session_id }` — hand off to `wtcli focus-pane`
  (which transparently restores a stashed agent pane after PR A).
* `ResumeInAgentPane { key, cli }` — new tab + agent pane + ACP
  `session/load(key)` (requires `loadSession` capability).
* `ResumeCliFlag { key, cli }` — new tab + plain pane running
  `<cli> --resume <key>` (requires the CLI to advertise a resume
  flag — Codex doesn't).
* `NotResumable { reason }` — surface a system message.

The table (matching `session_mgmt.rs` and the plan):

| Row state                  | Enter            | Shift+Enter      |
| -------------------------- | ---------------- | ---------------- |
| Any Live (Class A or B)    | Focus            | Focus (same)     |
| Class A dead (Ended/Hist)  | ResumeInAgentPane| ResumeCliFlag    |
| Class B dead (Ended/Hist)  | ResumeCliFlag    | ResumeInAgentPane|
| Unknown CLI                | NotResumable     | NotResumable     |
| Missing capability         | NotResumable     | NotResumable     |

Shift on Live is intentionally identical to Enter — agents forbid two
clients on one session, so any "force second copy" attempt would just
error out. Shift is a safety alias there.

### Dispatch boundary

`App::activate_agent_session_with_shift` (`app.rs`) is the only
caller of `decide_enter_action` from production code. It then
dispatches into the existing helpers (`dispatch_focus_pane`,
`dispatch_resume_in_agent_pane`, `dispatch_resume`) — those own
all the side effects:

* phantom-on-disk guard (`key_is_resumable_on_disk`),
* optimistic `SessionEvent::ResumeDispatched` state flip,
* `resume_in_new_agent_tab` event publish (for the WT side to open
  a new tab + reconcile the agent pane),
* on-failure `PaneClosed` rebroadcast (so a stuck Live row
  transitions to Ended after `wtcli focus-pane` returns NotFound).

### MVP origin filter (session management picker shows shell-pane sessions only)

The session management picker currently ships in MVP mode: it only surfaces
**Class B** (shell-pane) sessions — the user manually ran `copilot`
/ `claude` / `gemini` in a regular shell. **Class A** (agent-pane)
sessions stay in the registry so Enter routing, alive-mirror
reconciliation, `intellterm.wta/session_added|removed`, and
`wta sessions list` all keep seeing every row; they just don't
render in the picker and aren't reachable by the cursor.

The gate is a single constant — `app.rs::MVP_SESSIONS_ORIGIN_FILTER` —
threaded through `App::sessions_origin_filter` so that the three places
that have to stay in sync read the same value:

1. `App::agents_rows_for_tab` (cursor / Enter dispatch source of
   truth) — applies the filter to both the snapshot path and the
   registry-fallback path.
2. The post-history-scan auto-select and the Delete clamp (same
   file) — `iter_sorted_with_filters(cli, self.sessions_origin_filter)`.
3. `ui/agents_view::render` — applies the same retain to keep the
   rendered rows lined up with the cursor model.

`agent_sessions::OriginFilter` (`ShellOnly | AgentPaneOnly | All`)
is the public surface; `iter_sorted_with_filters` is the registry
API. `iter_sorted_filtered` is preserved as a thin wrapper
(`origin = All`) so existing call sites keep their behavior.

**Debug overrides** (no rebuild required):

| Surface | How to see everything |
|---|---|
| session management picker, single helper | `WTA_SESSIONS_SHOW_AGENT_PANE=1` in that helper's env |
| Out-of-band debug list | `wta sessions list` (defaults to `--origin all`) |
| Slice to shell only | `wta sessions list --origin shell` |
| Slice to agent-pane only | `wta sessions list --origin agent-pane` |

`wta sessions list` always asks master for the full registry and
filters client-side, so it can act as the eye-of-god view even
while every helper's session management picker stays in `ShellOnly`. The table
output gains an `ORIGIN` column (`Shell` / `AgentPane` / `-` for
untagged legacy rows); JSON output is unchanged because the field
was already serialized.

**Removing MVP gate.** When agent-pane session management is
ready, flip `MVP_SESSIONS_ORIGIN_FILTER` to `OriginFilter::All` and
delete `WTA_SESSIONS_SHOW_AGENT_PANE` handling in
`resolve_sessions_origin_filter`. No other call site needs to change.

