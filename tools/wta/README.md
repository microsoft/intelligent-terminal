# WTA -- Windows Terminal Agent

A Rust TUI client and tmux-like CLI that connects AI agents to Windows Terminal.

Customization:
- See [CUSTOMIZATION.md](CUSTOMIZATION.md) for changing the agent model and runtime prompt.

## Quick Start

### Build

From the repository root:

```bash
cargo build --target x86_64-pc-windows-msvc --manifest-path tools/wta/Cargo.toml
```

The binary is output to
`tools/wta/target/x86_64-pc-windows-msvc/debug/wta.exe`. Always use the explicit
target in this repo: the package project prefers that output over the host-target
fallback.

### How WTA runs

WTA is normally launched **by Windows Terminal**, not by hand. WT spawns one
`wta-master` singleton (owns a lazily populated agent CLI pool) and one
`wta-helper` per agent pane (renders this TUI and speaks ACP to master over a
named pipe). Helpers selecting the same agent identity, source, and command
share one agent process. Bare `wta` with no subcommand and neither `--master`
nor `--connect-master` exits with an error — there is no standalone agent / TUI
mode.

The default agent is Copilot; the agent and model come from Windows Terminal
settings (`acpAgent` / `acpModel`) and are passed through to master via `--agent`
/ `--agent-id` / `--acp-model`.

When the agent pane is connected to Windows Terminal, the agent-facing contract is
the local `wta` CLI: the agent shells out to commands like `wta active-pane --json`,
`wta list-panes --json`, `wta capture-pane --json`, and
`wta resolve-command <name> --cwd <active-pane-cwd> --json`. Terminal-control commands talk to Windows
Terminal over the COM protocol; `resolve-command` inspects the user's real,
shell-context-selected sources (active working directory, host PATH and, for
PowerShell, the profile-loaded command environment).

Autofix sends the failing command's context without pre-querying similar command
names. Its prompt advertises `wta resolve-command` for agent-initiated diagnosis,
using the failing pane's shell and working directory. Command enumeration is
uncached and runs only when requested; there is no background refresh or
startup/tab-selection prewarming. Query failures or unsupported shell contexts
are not evidence that a command is missing. The prompt directs agents to propose
obvious typos in familiar commands (such as `gti status` -> `git status`) without
lookup, while using local evidence for unfamiliar commands or ambiguous corrections.

The packaged app registers `wta.exe` as an App Execution Alias. Before spawning
the host agent, WTA puts the current package family's alias directory first on
`PATH`; unpackaged builds use the running binary's directory. Agent prompts can
therefore use short `wta.exe` commands without selecting another installed
branding or reproducing a protected package path.

### tmux-like CLI

WTA exposes tmux-equivalent subcommands for controlling Windows Terminal from the shell. Useful for humans and AI agents that can shell out.

```bash
wta list-windows                          # list all WT windows
wta list-tabs                             # list tabs in first window
wta list-panes                            # list panes in first tab
wta active-pane                           # show focused pane
wta new-tab -c "pwsh.exe" -n "Build"      # create tab running pwsh
wta split-pane -H -c "pwsh.exe"           # split horizontal
wta capture-pane -t 3 -l 50              # read last 50 lines from pane 3
wta kill-pane -t 3                        # close pane 3
wta pane-status -t 3                      # check if running
wta wait-for -t 3 --timeout 30           # wait for pane 3 to exit
wta resolve-command which --cwd . --json  # resolve from cwd + PATH + shell-specific sources
wta list-windows --json                   # raw JSON output
```

Short aliases are supported: `lsw`, `lst`, `lsp`, `neww`, `splitw`, `capturep`,
`killp`, `setenv`, and `mon`.

When `-t` (target pane) is omitted, the active pane is used automatically.

### Protocol Discovery & Environment Setup

WTA finds Windows Terminal via the `WT_COM_CLSID` environment variable, which
WT propagates into every conpty child it spawns. You usually don't need to do
anything — just run `wta` inside a WT pane.

```bash
# Inspect the inherited value
wta pipe-id                               # print CLSID
wta pipe-id --json                        # JSON with metadata

# Re-export it into another shell session (rarely needed)
eval "$(wta set-env)"                     # bash/zsh
wta set-env -s powershell | Invoke-Expression   # PowerShell
wta set-env -s fish | source              # fish
wta set-env -s cmd                        # cmd (copy-paste output)
```

### Test connectivity

```bash
wta test-pipe
wta --test-pipe     # legacy flag, still works
```

Connects to the WT protocol, prints `list_windows` + `get_capabilities`.

## Protocol Connection

WTA discovers Windows Terminal via the `WT_COM_CLSID` environment variable. WT
sets this in its own environment at startup and propagates it to every conpty
shell, so any pane-launched process — including wta and wtcli — inherits it.

## Environment Variables

| Variable | Required | Description |
|----------|----------|-------------|
| `WT_COM_CLSID` | Yes* | Stringified GUID of WT's `TerminalProtocolComServer` COM class |
| `WTA_LOG` | No | Rust tracing filter, such as `debug` or `trace` |

\* Set automatically by WT when it spawns a conpty child. If you launch `wta` from outside WT, run `eval "$(wta set-env)"` to copy the value over (only useful when you've previously captured it from a WT shell).

## Global CLI Options

| Flag | Description |
|------|-------------|
| `--json` | Output raw JSON instead of human-readable tables |
| `--agent <CMD>` | Agent CLI command for ACP mode (default: `copilot --acp --stdio`) |

## TUI Controls

Tool rows keep a localized type label such as **Run**, **Read**, **Search**, or
**Edit** visible across pending, running, and completed states. Consecutive
successful Read, Search, Edit, and Delete calls collapse into one summary row;
click that row to inspect each call. ACP thought chunks appear in an expanded
**Think** block with muted italic text and a left rule. Each thinking phase
automatically collapses when an answer or tool activity starts, thinking ends,
or the turn completes or is canceled. Click its header to reopen it, including
in completed history. Ctrl+O toggles thinking in the selected history turn, or
the active/latest turn when none is selected. Phase duration is measured locally;
replayed thinking has no duration because ACP does not supply historical timing.
Each block retains the latest 4,000 Unicode characters. No thought text is
invented when a provider is silent. Synthetic waiting feedback uses only the
shimmering Thinking indicator above the input box, never a transcript row.
Expanded Edit details show bounded line-level `+`/`-` hunks computed from ACP
snapshots. Tool headers and groups can be expanded during the active turn as well
as in history. Expanded Search details wrap the provider's `rawInput.query`
(or its title when no query is supplied) and any returned text results. WTA
does not reconstruct queries or results omitted by the provider. Queries retain
the first 4,000 Unicode characters, all scrollable when expanded. Text results show
up to 12 wrapped lines, with `…` for omitted text. Expansion follows the tool
into completed history.

Chat follows new output while you are at the bottom. Scrolling up preserves your
reading position as text streams, tools update, and turns finish; scrolling back
to the bottom resumes following. Sending a prompt or clearing/loading a session
still resets the view. Streaming thinking retains its latest 4,000 characters;
your reading position follows the same retained text even when older text is
trimmed. If the text you were reading is removed or a thinking block collapses,
the view clamps to surviving content.

| Key | Action |
|-----|--------|
| Type + Enter | Send prompt to agent |
| Ctrl+C | Copy selected text; otherwise cancel streaming / quit |
| Up / Down | Browse prompt input history |
| Mouse wheel | Scroll chat (hold Alt to scroll one line) |
| Click a tool header | Expand or collapse that tool's details, live or completed |
| Click a thinking header | Expand or collapse that block, live or completed |
| Ctrl+O | Expand or collapse thinking in the selected/latest turn (or the active turn), and all live and completed tool details |
| Mouse drag | Select a continuous text range |
| Double / triple click | Select a word / line |
| PageUp / PageDown | Scroll chat |
| F12 | Toggle debug panel (pipe traffic viewer) |
| Shift+PageUp/Down | Scroll debug panel |
| Y / N | Quick allow/reject on permission dialog |
| Up / Down / Enter | Navigate permission options |

WTA automatically selects **Allow once** only when the tool matches the exact MCP
server currently bound to that ACP session by master. Master overwrites provider
metadata with that identity on each forwarded permission request and tool update;
correlated calls must match the session, call ID, and current server identity.
Terminal actions still require their action-card confirmation, and
`request_user_input` still presents its question. Foreign or missing identities
(even with the same tool name or server-name prefix) and requests without an
**Allow once** option keep the normal permission dialog. WTA does not grant
persistent approval automatically.

Pending and replayed command suggestions show only the command, without assuming
Run or Insert. After the user chooses, history uses the localized
`Run: <command>` or `Insert: <command>` label. Cancelling retains the command with
a localized cancellation status on the same line, not on the conversation title.
History has no suggestion counts, numbering, or recommendation checkmarks.

## Debug Panel

Press **F12** to open a side panel showing all JSON-RPC messages between WTA and Windows Terminal in real time.

```
[3456.1] >>> {"type":"request","id":"3","method":"list_windows","params":{}}
[3456.1] <<< {"type":"response","id":"3","result":{"windows":[...]},"error":null}
```

- Green `>>>` = request sent to WT
- Cyan `<<<` = response from WT
- Shift+PageUp/Down to scroll

## Debug Logs

WTA writes structured logs under the package log dir, in a per-version
subfolder: `…\LocalCache\Local\IntelligentTerminal\logs\<pkgver>\` when
packaged (or bare `%LOCALAPPDATA%\IntelligentTerminal\logs\` unpackaged):

| File | Contents |
|------|----------|
| `wta-main_master.log` | `wta-master`: agent CLI pool, pipe accept loop, per-helper routing |
| `wta-main_helper-{pid}.log` | each `wta-helper`: pipe connect, ACP init, prompts, agent responses, TUI lifecycle |
| `wta-cli.log` | short-lived CLI helpers (`list-*`, `capture-pane`, `listen`, `sessions`) |
| `terminal-agent-pane.log` | Agent-pane chrome (C++ TerminalApp side) |
| `wta-ensure-host.log` | Background host startup / COM connection / SharedWta lifecycle |
| `wta-acp-debug.log` | ACP protocol debug trace |
| `wta-delegate.log` | `?<prompt>` delegation flow |
| `wta-probe.log` | Agent/model/session capability probes |
| `wta-install-hooks.log` | Hook installation and upgrade diagnostics |
| `hook-trace.log` | Shell-hook event diagnostics |

Set `WTA_LOG=debug` for verbose output (debug builds default to `debug`, release
to `info`). The F12 debug panel in the TUI shows protocol traffic live without
tailing log files.

### Bug-report ZIP and privacy

The **Report a bug** action creates a uniquely named ZIP on the Desktop:

* `logs/` contains the existing runtime logs, including retained version folders.
  **These logs are not redacted.** They can contain prompts, commands, terminal
  output, paths, provider data, ACP payloads, or other sensitive information.
  Review the whole archive before sharing it.
* `diagnostics.json` is a separate, versioned, allowlisted metadata document.
  `schema_version: 1`, UTC collection timestamps, `collection_status`, and
  bounded `collection_errors` (`stage`/`code`, never exception messages) describe
  what was collected. `collection_error_count` counts failures even after the
  32-entry detail limit. Missing/partial data is not proof that a component was
  absent during the failed request.

The UI snapshot is taken synchronously in the invoking window before background
collection. It separates stable tab GUIDs from tab indexes and records pane
IDs/session GUIDs, focus, source flags, agent/stashed state, helper readiness,
and inherited profile `reloadEnvironmentVariables` values. Settings are limited
to known provider IDs (`custom:<anything>` becomes `custom`), known position and
confirmation values, feature flags, and policy booleans. Custom commands,
provider names, models, titles, cwd, terminal contents, raw configuration and
environment values are not included in this document.

Process records use pinned handles to the collecting Terminal, its owned WTA
master and current window's helpers—not a system process dump. They include
PID, Windows creation-time FILETIME, a derived PID/start-time instance ID,
architecture and package identity where available. Owner window/tab/pane
metadata describes **collection time**; helper-origin and actual connected-master
provenance are recorded in request/connect logs. An owned master is not assumed
to be the master a transferred helper currently uses.
Helper root handles are independently pinned during process creation/handoff,
before the ConPTY output thread can close the original handle. The query-only
duplicate API is synchronized separately from connection teardown; it never
duplicates the old borrowed `RootProcessHandle` or reopens a PID. The pin lasts
until the connection is destroyed, so a record can describe an exited root
process. Pin/acquisition failures are reported as unavailable, not guessed.

Binary entries distinguish actual process images, installed sibling
`wta.exe`/`wtcli.exe` candidates, and an already-loaded `OpenConsoleProxy.dll`.
Only fixed product roles and path categories are emitted, never absolute user
paths or arbitrary executable basenames. SHA-256 is a hash of **file bytes at
collection time**, not a Git commit or a hash of mapped process memory.
WTA's fixed `.wtadiag` PE section supplies its embedded Cargo version and
build-time Git commit without launching a probe. Builds without Git, older WTA
images without that section, and native products without an embedded commit say
unavailable; dirty working-tree state is not encoded, so a commit is not proof
of a pristine build. File version, package version and embedded build identity
are separate measurements.

Collection is capped at 128 tabs, 512 pane records, 64 relevant process handles,
16 distinct images, and 256 MiB / two seconds of read-loop time per image.
Only fixed local-drive product images are read. Failures and limits produce
safe error codes. The report does not enumerate other windows' UI or remote
process environments, export the registry, query agents, activate COM servers,
or launch other Terminal packages. Its expected CLSID is passive build
metadata; actual request-time `Authenticate` identities are authoritative.
`WT_COM_CLSID` is included only as a validated canonical GUID;
`WT_WTCLI_PATH` records override presence, not its value. Logging filter source
and effective maximum level belong to each role's startup log, not a report-time
guess at another process's environment.

Each action stages only its own `diagnostics.json` outside the log tree, removes
that file and its empty staging directory during normal completion/unwinding,
and gives its ZIP a unique name so concurrent reports do not share staging files.
An abrupt process exit can leave the report-only directory behind; it is not
included in later log bundles. If tar exits unsuccessfully while archiving logs,
collection retries once with metadata only, recording `logs_included: false`
and an explicit error. Each tar invocation has a 60-second wait limit; timeouts
are not retried.

### Diagnosing an unbound action target

Enable logging **before a fresh launch**, after closing all Intelligent Terminal
windows normally. Master inherits its launch environment, but helper ConPTY
profiles normally regenerate theirs from the registry. Setting only `$env:WTA_LOG`
can therefore enable master logging without enabling helper logging.

In a separate PowerShell window, retain both previous values for restoration:

```powershell
$previousUserWtaLog = [Environment]::GetEnvironmentVariable('WTA_LOG', 'User')
$previousProcessWtaLog = $env:WTA_LOG
[Environment]::SetEnvironmentVariable('WTA_LOG', 'debug', 'User')
$env:WTA_LOG = 'debug'
wtai
# After reproducing and closing Intelligent Terminal normally:
[Environment]::SetEnvironmentVariable('WTA_LOG', $previousUserWtaLog, 'User')
$env:WTA_LOG = $previousProcessWtaLog
```

`logging_configuration` records role/PID, the first valid filter source
(`WTA_LOG`, `RUST_LOG`, or default), its maximum level, and whether the ACP
dependency privacy cap applies. The maximum is an upper bound, not a claim
that every target is enabled. Raw directives (including dynamic field selectors)
are never printed. This INFO event respects filtering: `warn`/`off` can hide it.
The defaults and dependency privacy cap are unchanged.

Follow these metadata-only events:

String identities in the new Rust diagnostics are rebuilt as canonical GUIDs
(optional input braces are removed) or unsigned 64-bit decimal IDs. Missing
values say `absent`; all other values say `unsupported_redacted`. In particular,
opaque ACP session IDs remain valid for routing but are not logged or hashed;
correlate those events using the process/prompt and validated pane identities.
This is a logging-only schema and does not validate or change the source inputs.
Tool names come from the known action-tool enum, not request text.

* Helper `prompt_context` spans carry ACP session/prompt, owner tab/window,
  helper pane, and explicit source identities. `pane_context_acquisition_failed`
  reports stable codes such as `protocol_request_failed`,
  `response_contract_invalid`, `agent_pane_selected`, or
  `legacy_source_unresolved`. A failed protocol request alone does **not** prove
  absence of a source: inspect the C++ phase/HRESULT. `no_channel` and deliberate
  agent-command context skips are DEBUG, not warnings. Old protocol servers
  retain the observable `pane_context_legacy_fallback` path.
* `prompt_context_binding` records the final proposed binding, including an absent
  target; cancellation or channel-issue failure can still prevent its use.
  `prompt_binding_issued` confirms successful channel issuance with that target.
  `terminal_action_validation_rejected` records stage, known binding, tool,
  retryability and a stable reason code (`no_active_target`, `malformed_payload`,
  `policy_violation`, etc.). Pre-binding failures say `binding_known=false`;
  prompt/target identity is unavailable there, not inferred. Helper reply events
  record final retryability; UI validation events supply the precise typed code.
* `terminal-agent-pane.log` adds `pane_context_active`,
  `pane_context_source_candidate`, `pane_context_source_selection`,
  `pane_context_owner`, `pane_context_snapshot`, and `pane_context_com`.
  These distinguish source selection (`no_source`, `agent_pane_selected`,
  `no_session_id`) from metadata, last-command and buffer-capture failures.
  Explicit-source misses in individual windows are DEBUG; the COM aggregate
  not-found result is WARN. `pane_source_update` records source state around
  active-pane changes; `pane_source_collapse` records representation promotion
  before focus callbacks and after collapse. `pane_source_trigger` marks
  stash/restore before synchronous focus callbacks. Tab records use stable tab IDs;
  pane-local collapse records use terminal session IDs to join to owner records.
  Numeric pane IDs are tab-local; `4294967295` denotes an unavailable numeric ID.

For Rust → CLI → COM correlation, `pane_context_wtcli_started` supplies the
child PID inside the prompt span; `pane_context_wtcli_failed` retains the PID
and a safe failure category at WARN (spawn failure has no child PID).
CLI `pane_context_wtcli` brackets its COM call
with the same PID and creation time, and records the validated server identity
even when `GetPaneContext` fails. COM records a best-effort local-RPC `caller_pid` and
`caller_query_status`; only status zero establishes that PID link. If unsupported,
PID is zero and correlation is limited to timestamps, explicit source and owner
metadata. COM → UI snapshot records share window/source and the call's time
interval, **not** a propagated request ID; concurrent implicit requests can
remain ambiguous. No protocol 2.3 ABI or target-selection policy changed.

`Authenticate` now optionally returns `server_identity` (PID, creation time,
instance ID, package/product version and registered CLSID). `wtcli --json info`
exposes it; old servers without it still negotiate normally. Metadata is
type-checked and reconstructed from an allowlist rather than logged verbatim.
COM assigns a process-local `request_id` to every `GetPaneContext` call and
logs it on failures as well as success. Successful context JSON has an additive
`diagnostics` object carrying that ID and identity; WTA logs
`pane_context_server_provenance` without changing or rejecting the context.
Join request counters with the server instance ID, not with counters alone.
`helper_master_identity` records the actual connected named-pipe server PID and
best-effort creation time without recording the pipe address.

To distinguish the "no active pane" paths, follow the first failed phase rather
than treating the final MCP rejection as the root cause. Implicit context still
selects the globally most recent Terminal host, then its focused tab and source
pane; explicit context still searches for the given source session. A missing
source, wrong host, invalid metadata, or failed capture can all leave the prompt
without a binding and later produce `no_active_target`. The added diagnostics
differentiate these possibilities; they do not change focus, routing, fallback,
capture limits, or action acceptance, and a later report snapshot alone cannot
establish which condition caused an earlier failure.

New diagnostics exclude prompts, commands, tool arguments, terminal contents,
raw stderr, payload-dependent validation reasons and arbitrary protocol string
values. C++ records use bounded, exception-isolated writes with textual DEBUG/WARN
labels; they are not controlled by `WTA_LOG`. Existing ACP wire/debug-panel logs
can still contain content: review a complete log bundle before sharing it.

## Project Structure

```
tools/wta/src/
+-- main.rs                    Entry point, role/CLI dispatch, protocol discovery
+-- master/mod.rs             wta-master: owns the agent CLI pool, multiplexes helpers
+-- helper/mod.rs             wta-helper: per-pane entry (reuses the TUI over a pipe)
+-- app.rs                     TUI state machine, event loop, per-tab sessions
|   +-- app/autofix.rs         Autofix detection + suggestion
|   +-- app/turn_state.rs      Per-turn state machine
+-- event.rs                   Crossterm event reader
+-- coordinator.rs             Delegate (?<prompt>) execution
+-- agent_sessions.rs          Session registry (status / liveness model)
+-- session_watcher/           CLI-log status classification per agent
+-- theme.rs                   Color constants
+-- protocol/
|   +-- acp/client.rs          ACP client (agent-CLI side) + helper-side WtaClient
+-- shell/
|   +-- shell_manager.rs       Terminal abstraction (local subprocess or WT pane)
|   +-- wt_channel/
|       +-- mod.rs             WtChannel trait definition
|       +-- cli_channel.rs     wtcli subprocess (CoCreateInstance via wtcli.exe) — all methods
+-- ui/
    +-- layout.rs              Main layout (+ debug panel split)
    +-- chat.rs                Message rendering
    +-- input.rs               Input box with cursor
    +-- permission.rs          Permission modal dialog
    +-- agents_view.rs         Session-management (/sessions) view
    +-- debug_panel.rs         Protocol traffic viewer (F12)
```

## Development

### Prerequisites

- Rust toolchain (edition 2021)
- Windows Terminal with protocol server enabled (for WT integration)
- An ACP-compatible agent CLI (Copilot, Claude ACP adapter, etc.)

### Build and run

Run these commands from the repository root. CI resolves the
`tools/wta/rust-toolchain.toml` `ms-prod-1.93` pin through MSRustup; local
repo-root commands use your installed active toolchain, so changes must remain
compatible with Rust 1.93.

```bash
# A live process may lock the output. Stop only a PID whose executable path
# matches this target; do not kill every wta.exe by name.
cargo build --target x86_64-pc-windows-msvc --manifest-path tools/wta/Cargo.toml

# cargo build does not compile #[cfg(test)] code.
cargo test --target x86_64-pc-windows-msvc --manifest-path tools/wta/Cargo.toml
```

The TUI (master + helper) is launched by Windows Terminal as an agent pane — see
the C++ F5 / `bcz` flow in the repo `AGENTS.md`. From a WT pane you can exercise
the CLI helpers directly with the packaged `wta` app execution alias.

### Development workflow

1. Open Windows Terminal (with the agent pane / protocol server enabled)
2. Run `wta pipe-id` to verify `WT_COM_CLSID` is set
3. Open the agent pane (`>Toggle AI assistant` / `Ctrl+Shift+.`) — WT spawns the
   helper, which connects to master and renders this TUI
4. Press F12 to open the debug panel and see all protocol traffic
5. Interact with the agent -- watch requests/responses flow in real time
6. Use `wta list-panes`, `wta capture-pane` etc. in another pane for debugging

While connecting, the chat activity row shows WTA's current operation: preparing
the agent connection, connecting to the local coordinator, initializing the
connection, refreshing user authentication after login (when supported), reading
the coordinator's session registry snapshot, creating a session, or setting its
model (when requested). Initialization and creation include local preparation
and registration, not just waiting on the agent. `/restart` first shows
"Restarting agent" while old sessions retire. These are local operation
boundaries, not agent-reported progress: they do not expose internal MCP or
model-catalog loading, and reading the registry does not fetch agent history.
A queued session restore shows the actual connection stage first, followed by
short resume context; once connected, it shows only "Resuming session" until
the load completes. The pane does not become connected earlier, and these labels
do not reduce startup time.

### Adding a new WT protocol method

1. Declare the method in `src/cascadia/TerminalProtocol/TerminalProtocol.idl`
2. Implement it on `TerminalProtocolComServer` (`src/cascadia/WindowsTerminal/TerminalProtocolComServer.cpp`)
3. Add a `wtcli` subcommand in `src/tools/wtcli/main.cpp` that calls the new method
4. Add a `CliChannel::request` arm in `tools/wta/src/shell/wt_channel/cli_channel.rs` mapping a method name to the new `wtcli` subcommand
5. Rebuild WT, wtcli, and wta

## Architecture Notes

- **ShellManager** owns local terminals and the active `WtChannel`
- **CliChannel** shells out to `wtcli.exe` per call; `wtcli` does `CoCreateInstance` to reach WT's COM server. All methods, including `send_input` (via `wtcli send-keys`), go through this path.
- **Protocol discovery**: `WT_COM_CLSID` env var, inherited from the WT-spawned conpty
- **CLI subcommands** call `CliChannel::connect()` directly; no ShellManager needed
- **Pane identity** is discovered at startup via PID matching (list all panes, find ours)
