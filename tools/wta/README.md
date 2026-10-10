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
`wta-master` singleton (owns the shared agent CLI pool) and one
`wta-helper` per agent pane (renders this TUI and speaks ACP to master over a
named pipe). Helpers selecting the same agent identity, source, and command
share one agent process. Master warms installed, policy-allowed native host agents
other than Gemini in the background at startup; Gemini and other selections
remain on-demand. Bare `wta` with
no subcommand and neither `--master`
nor `--connect-master` exits with an error — there is no standalone agent / TUI
mode.

The default agent is Copilot; the agent and model come from Windows Terminal
settings (`acpAgent` / `acpModel`) and are passed through to master via `--agent`
/ `--agent-id` / `--acp-model`.

Delegate launches likewise receive a resolved `--delegate-agent` command and
its separate canonical `--delegate-agent-id` (including `custom:<name>`).
Helper bootstrap and hot settings updates preserve that pair with the delegate
model. Recommendations and session MCP delegation mark new tabs and supported
splits as native-agent content at creation, independently of task titles or
session hooks; ordinary shell workspaces remain unclassified.

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

### Sidebar Agent sessions

Master discovers installed, policy-allowed Windows-host agents in the background
as soon as its named pipe is ready, without waiting for the sidebar to open. It checks
the native agent CLI and required `npx` prerequisite before starting ACP, reuses
matching connections in the agent pool, and merges each supported `session/list`
response into the registry. No chat session or prompt is created by discovery.
Gemini is excluded before availability checks and ACP startup, even when installed
and policy-allowed. Explicit Gemini chat selections remain supported, and Gemini
sessions already registered by other paths are not filtered out of the sidebar.

Discovery never automatically installs a native agent CLI. The pinned Claude and
Codex ACP adapters are separate: their cache presence is not checked, and the
existing `npx -y` launch behavior may download and bootstrap an uncached adapter
during initial startup or a later refresh that starts a provider. This is allowed
and may require network access; discovery is not an offline-only operation. See
[Installing dependencies](../../doc/installing-dependencies.md) for the native CLI
and ACP wrapper prerequisites.

Sidebar Agent sessions runs
`wta sessions list --origin shell --json --include-status`.
This only reads the current registry snapshot; it never starts an agent or waits
for an ACP history query. Master synchronizes initialized, listing-capable pooled
connections every five seconds, including already-connected WSL and custom agents.
Each connection has one refresh in flight; history and title updates share its
single response. Failed queries retain prior rows and back off up to 60 seconds.
The opt-in JSON object contains `sessions` and `history_status` (`loading`, `ready`,
or `error`), with optional `history_error_kind` to distinguish timeout-only failures;
ordinary `--json` output remains one session per line.

An activity hook cannot claim or alter a pane owned by another live session,
even if its raw session ID exists in another provider or source. Nested CLI
workers can inherit the parent's pane identity; their synthetic session starts
and errors must not end, unbind, or change the parent's status. Explicit
session-start hooks still replace a pane's session, and activity in an
unowned pane retains its normal discovery behavior.

Live rows may also include response-only `owner_window_id` and `background_tab`
fields from the exact bound pane's context. `background_tab: true` means the
pane belongs to a kept-running whole tab; activating it restores that entire
tab. Only an explicit `false` with a different owning window enables the
other-window action. Missing or malformed membership is unknown, not evidence
that the pane is attached elsewhere. These fields are refreshed per response,
not stored as registry ownership or lifecycle state.

Sessions registered as live in master keep their detailed Idle, Active,
Waiting for input, or Error status. Historical/Ended native host Copilot rows
are not live IT registrations; they may instead receive the response-only
`InUse` status, displayed as **In use**, from their exact default-universe SDK
session directory. This requires a
matching PID marker, a live native `copilot.exe` created before that marker,
and an actively held `inuse.<pid>.hold` lease (observed with Copilot SDK
1.0.80). A stale marker, released lease, inaccessible process,
or nonmatching provider/source/universe leaves the original status unchanged.
External sessions do not expose detailed activity: this probe does not read
`events.jsonl` or infer Idle/Active from turns. Marker and lease reads have a
two-second budget. This does not mutate registry state,
infer a window/pane owner, or enable a running-location indicator.

The initial
discovery stays `loading` until all eligible host providers finish. Providers that
do not support listing are skipped, while initialization or listing failures
produce `error`. Later refreshes retain the last completed status until they finish.
The sidebar shows available rows immediately, shows a loading indicator while an
empty snapshot is still loading, and displays "No agent sessions found" only after
a successful empty result. Non-timeout errors remain visible alongside available
rows. Query timeouts are logged without an error banner: cached sessions remain
usable, or the initial loading indicator remains until a result is available.
Mixed failures are not treated as timeout-only. Failed refreshes do not clear
previously displayed sessions. This does not depend
on the agent pane's chat connection or hooks being ready.

History activation keeps an operation ID until its outcome is confirmed.
If the activation CLI times out, the sidebar checks the receipt using
`wta sessions activate --status-only` with the same identity, target window, and
`--activation-id`. This is a read-only status request: `pending` and `unknown`
never start another focus or restore. Master continues an accepted activation
after the requesting CLI disconnects. Retrying an unresolved row checks the
same receipt, including after closing and reopening History; it does not generate
a fresh activation ID. See
[session tracking](../../doc/specs/hybrid-agent-session-tracking.md) for receipt
retention and refresh cancellation/backoff behavior.

These native-provider ACP processes remain in the master pool after History closes;
there is no History-specific idle timeout or eviction. Further refreshes reuse them,
and concurrent windows share one discovery pass. Registry and discovery-status changes notify the sidebar,
with a 60-second snapshot poll while the view is open in vertical layout as a fallback. Opening the
view still fetches immediately. Unavailable or failed
providers do not clear other providers' rows or overwrite live activity and pane
bindings. Failures are logged under `master_history`; listing never installs a native
agent CLI or starts an interactive login flow.

This discovery covers built-in agents on the Windows host. It does not start WSL
distributions or discover arbitrary custom commands; sessions already in the registry
remain visible according to the requested origin filter.

Initial host discovery runs at master startup, after a confirmed host-agent
installation, or on an explicit `wta sessions refresh` request. There is no unconditional
periodic installation scan. Failed host-agent startup discoveries are retried through the
same discovery worker, with delays of 5, 10, 20, 40, then at most 60 seconds after
each failure (checked on the existing five-second history timer). Retries recheck
installation and policy and do not restart healthy resident providers.
`wta sessions refresh --json` bypasses this startup backoff, schedules discovery and returns the
current snapshot with `history_status`; it does not wait for discovery to finish.
The removed `--all-agents` flag is no longer accepted. F5 in a helper's session view
explicitly refreshes that helper's bound connection without discovering other agents.
Ordinary helper reads are also snapshot-only. Their 60-second open-view fallback runs
only in nonvertical layout; vertical layout uses the Sidebar fallback instead.
Live layout changes and helper-ready runtime configuration update this selection per
window without reconnecting ACP. Push updates remain immediate in either layout,
and returning to nonvertical layout immediately refreshes an already-open helper view.
History-query retries do not restart or initialize agents; startup discovery retries
are separate and retain the existing ACP initialization timeout.

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
| Type + Enter | Send a prompt, or queue it while the agent is busy or connecting |
| Ctrl+C | Copy selected text; otherwise cancel streaming / quit |
| Ctrl+V / configured Paste shortcut | Paste text or attach a clipboard image to the chat draft |
| Right-click | Follow the effective `rightClickContextMenu` and `copyOnSelect` settings |
| Alt+V | Attach a clipboard image to the chat draft |
| Ctrl+Z | Undo the latest edit in the focused chat draft |
| Ctrl+Y | Redo an undone edit in the focused chat draft |
| Up / Down | Browse prompt input history |
| Mouse wheel | Scroll chat (hold Alt to scroll one line) |
| Click draft text | Move the draft caret to the clicked text cell |
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

Image paste accepts screenshots and copied image files. Images appear as inline
attachment tokens and are sent with the next prompt, not immediately. The agent
must advertise image support; otherwise the pane shows a warning and leaves the
draft unchanged. Ordinary text and non-image file paths retain their paste
behavior. Alt+V remains an image-only shortcut.

Ctrl+V follows the terminal action map: an explicitly unbound or reassigned
shortcut is not overridden. With `rightClickContextMenu` enabled, right-click
opens the terminal context menu instead of copying or pasting. Otherwise, with
`copyOnSelect` disabled, right-click copies selected text or pastes when nothing
is selected. With `copyOnSelect` enabled, mouse selections are copied on release
without removing the highlight, and right-click pastes without re-copying an
already-copied selection. These settings also update in existing and stashed
agent panes when terminal settings reload.

Draft undo groups contiguous typing; paste, cut, selection replacement, deletion,
and idle draft clearing are separate edits. Cursor, selection, focus, and view
changes separate typing groups without adding text edits. A new edit after undo
discards redo. Each tab keeps its own in-memory edit history, without a fixed step
limit; this is separate from submitted prompt history and screen scrollback.
Browsing prompt history preserves the original draft's edit chain, while editing
a recalled prompt starts a fresh chain. Submission and session reset discard the
old edit history: undo does not reverse submitted agent or tool actions.

Undo/redo handles these keys only while the chat draft owns input. Permission
dialogs retain their existing Y/N and Enter shortcuts, including Ctrl+Y for
quick allow; a permission choice does not consume the draft's redo history.

Single clicks in draft text follow the visible wrapped or scrolled input row.
Wide characters and image attachment tokens keep valid editing boundaries.
Clicking dismisses full-draft selection without editing text or discarding redo;
dragging and double/triple clicks retain their text-selection behavior.

### Pending prompts

Each agent pane keeps an in-memory queue for its current conversation. You can
submit another prompt while a reply is streaming without interrupting that reply.
User requests run in submission order, one ACP turn at a time; separate messages
are not merged or injected into an active turn. Text and image attachments belong
to the request that was submitted, not to the next draft.
Disconnected or failed agents do not accept new requests: the draft and its
attachments stay in the editor with a connection error. Automatic Autofix and
diagnostics activation also reject requests in these states, reporting the error
in the owning tab rather than leaving work queued that would block `/restart`.
Requests can be accepted again once the agent is connecting.

Pending requests appear as one dim-gray count line directly above the input box,
independently of chat scrolling: `1 message queued` or `N messages queued`
(en-US). The count includes waiting user and automatic requests, increases on
enqueue, and decreases when a request is dispatched or removed. Active turns do
not count; an empty queue has no status line. There are no message previews,
buttons, or queue-management shortcuts (Alt+R, Alt+S, Alt+D).
The input and active permission/action remain usable. A user message that must wait receives an
Info notification confirming it was queued. Immediate sends and automatic
Autofix warm-up do not produce this notification.

Ordinary Up/Down history navigation does not remove a queued request.

Capturing context for an Autofix request is preparation, not queueing: it
produces neither a queued count nor an enqueue notification, even while the
agent is connecting or busy. Only successful capture admits the request to the
queue. A captured request that must wait then counts as queued; typed `/fix`
requests also receive the usual enqueue notification.

Error detection and its clickable diagnostics hint do not wait for ACP to
connect. With automatic suggestion off, detection alone does not enqueue work;
activating the hint captures context and then queues the requested fix until the
agent is ready.
Once accepted into the queue, the hint immediately switches to the non-interactive
pending state; it does not wait for the request to start running.
Repeated activation of the same detected failure does not add another request,
including while the session is still connecting. A later fresh failure remains
eligible for its own activation.
Cancelling active analysis requested from a detected diagnostic restores its actionable Detected hint unless a newer failure, shell progress, or source-pane closure has superseded or invalidated it.

The count refers to the pending queue, not the chat history. Queue capacity is
bounded; when a request does not fit, its draft remains in the editor.
`/stop` and user cancellation cancel the active turn but keep unsent user requests
in a stopped queue. Request failures also stop automatic sending so dependent
follow-ups do not run after a failed task. Waiting automatic Autofix requests are
discarded. A stopped queue uses the same count line. New input can join it
without restarting automatic sending. The underlying recall, resume, and discard
logic is retained, but the count-only UI deliberately exposes none of these
actions. **Temporarily, stopped requests have no recovery or discard UI.**

Permissions, clarification questions, and unresolved action cards still need
your response before another prompt starts. They are not queued prompts.
Session and configuration changes must not silently send pending input to a
different conversation; switching stays blocked while requests are waiting.
Connection recovery keeps retained input stopped.
An interrupted request whose delivery is uncertain is never automatically retried.

With automatic error suggestions enabled, shell failures received while the
helper is running can wait for the agent to connect or finish its current turn.
Automatic requests run after explicit user requests. While the queue is stopped,
new failures can show diagnostics but do not automatically start analysis.
Repeated pending failures
from the same source pane are coalesced, and shell progress or pane closure
invalidates obsolete requests. Prompt redraw markers alone do not represent new
shell work and do not discard a waiting fix. When WTA handles an Autofix trigger,
it awaits capture of the source pane's output, shell, and working directory as
part of queue admission, before processing the next helper event. Capture does not
wait for agent readiness or the current turn to finish. A typed `/fix` uses the
same admission-time capture. All preparations from one helper event share a
one-second capture deadline, so an unresponsive terminal read cannot hold up helper
event processing for wtcli's 30-second deadline. Expiry cancels the read subprocess
and discards unfinished preparations through the usual capture-failure warning;
it never admits a late result. Dispatch consumes the frozen evidence without
reading the pane again. This is a snapshot when WTA handles the trigger, not an
atomic snapshot at the terminal's command-finished marker. Any failed capture
shows a warning in the agent pane without creating a queue entry or stopping
existing work. The diagnostics hint stays detected and can be activated again
directly, without recalling or discarding a failed request. Cancelling during
preparation discards it; successfully captured unsent user requests retain the
existing stopped-queue behavior.
Disabling automatic suggestions leaves detected errors available for manual analysis.

Input history preserves the `/fix` command prefix but does not retain image
attachments. Resubmitting a recalled `/fix` captures the current source context
again, rather than reusing evidence from the earlier request.

Hiding the agent pane or dragging its tab between windows preserves the queue.
The queue is not persisted across helper/app exit or crashes, and it cannot
recover shell events emitted before the helper subscribed. Concurrent side
questions are not supported: additional prompts are follow-up turns in the main
conversation.

### Session MCP approvals and action history

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

## Interactive delegate tabs

`wta delegate` without a prompt opens the configured interactive agent CLI in
a fresh tab. The Agents sidebar's default `+` button uses this same delegation
path, including provider, model, policy, and explicit host/WSL source selection;
it does not open an assistant pane or resume a conversation. The normal Tabs
`+`, explicit profile dropdown entries, and existing shortcuts are unchanged.

The sidebar passes `--preserve-sidebar-view` to create the delegate tab in the
background and then focus its returned pane through the existing protocol focus
path. This preserves the selected sidebar page and search state. Other delegate
calls retain ordinary foreground tab creation.

Split Pane and Duplicate Pane retain the original terminal behavior in both
Tabs and Agents views. They use the ordinary split direction, size, and profile
rules without inspecting agent/session identity or invoking WTA delegation.
AI assistant panes remain fixed panels and cannot themselves be split.

Explicit CLI delegation can still split a live agent terminal:
`wta delegate --split-pane <pane> --split-session <current-session>
--delegate-agent <provider>` validates that pair against one live master row
and uses its exact host or WSL distro before launching a fresh interactive
instance through the existing delegate builders. The old session ID is only a
guard, never a resume argument. Missing, ambiguous, unknown-source, unavailable,
and unsupported/custom targets fail rather than launching a default shell.
Host splits carry the resolved project directory in an encoded PowerShell
wrapper that starts the existing delegate command with an explicit native
working directory; the split protocol itself has no cwd argument. The wrapper
preserves native arguments and exit status, including paths with spaces and
shell metacharacters. WSL retains its existing distro-specific `--cd` launch.
The sidebar `+` launcher uses bounded output capture and surfaces failures
through the sidebar's existing error presentation.

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
| `wta-main_master.<UTC-date>.log` | `wta-master`: agent CLI pool, pipe accept loop, per-helper routing |
| `wta-main_helper-{pid}.<UTC-date>.log` | each `wta-helper`: pipe connect, ACP init, prompts, agent responses, TUI lifecycle |
| `wta-cli.<UTC-date>.log` | short-lived CLI helpers (`list-*`, `capture-pane`, `listen`, `sessions`) |
| `terminal-agent-pane.log` | Agent-pane chrome (C++ TerminalApp side) |
| `wta-ensure-host.log` | Background host startup / COM connection / SharedWta lifecycle |
| `wta-acp-debug.log` | ACP protocol debug trace |
| `wta-delegate.<UTC-date>.log` | `?<prompt>` and interactive delegate creation |
| `wta-probe.<UTC-date>.log` | Agent/model/session capability probes |
| `wta-install-hooks.<UTC-date>.log` | Hook installation and upgrade diagnostics |
| `wta-panic.<UTC-date>.log` | Synchronous panic backstop when the normal buffered record may not flush |
| `hook-trace.log` | Shell-hook event diagnostics |

Rust WTA streams with dated names rotate daily and retain up to three matching
files. If a daily writer cannot initialize, that stream uses the fixed
`wta-<stream>.log` name in the same directory. Per-PID helper logs are also
reclaimed after three days.

Set `WTA_LOG=debug` for verbose output (debug builds default to `debug`, release
to `info`). The F12 debug panel in the TUI shows protocol traffic live without
tailing log files.

## Project Structure

```
tools/wta/src/
+-- main.rs                    Entry point, role/CLI dispatch, protocol discovery
+-- master/mod.rs             wta-master: owns the agent CLI pool, multiplexes helpers
+-- helper/mod.rs             wta-helper: per-pane entry (reuses the TUI over a pipe)
+-- app.rs                     TUI state machine, event loop, per-tab sessions
|   +-- app/autofix.rs         Autofix detection + suggestion
|   +-- app/prompt_queue.rs    Pending prompts, Autofix snapshots and dispatch gates
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
After loading, the pane header and model picker use the restored session's
agent-reported model, when available, without switching it to the current
default model. Settings still supplies the requested model for new sessions
and later model changes; an existing confirmed selection stays visible until
the agent confirms the switch.

### Diagnosing a missing current-shell pane

Default logs record failures without requiring `WTA_LOG=debug`:

- `terminal-agent-pane.log`: the actual server PID/window/tab, requested source,
  and why pane selection failed (for example, `active_agent_without_source`,
  `selected_pane_has_no_session`, or `explicit_source_unresolved`). Exceptions from
  the page-context query are logged once at the COM boundary with their HRESULT.
- `wta-main_helper-{pid}.<UTC-date>.log` (or the fixed
  `wta-main_helper-{pid}.log` fallback): `pane_context_unavailable` reasons distinguish
  protocol failure, an agent pane, and unresolved legacy lookup.
  `pane_context_response_contract_error` records invalid responses.
  `prompt_has_no_bound_pane` identifies the affected helper/prompt;
  `terminal_action_no_active_target` records rejection at the action check.

Use **Report a bug** to collect these in the existing log ZIP. These new lines
omit commands, terminal output, titles, and working directories; other existing
logs may contain private data, so inspect the ZIP before sharing it. These are
failure-time observations, not a history of how pane/source state changed.

### Adding a new WT protocol method

1. Declare the method in `src/cascadia/TerminalProtocol/TerminalProtocol.idl`
2. Implement it on `TerminalProtocolComServer` (`src/cascadia/WindowsTerminal/TerminalProtocolComServer.cpp`)
3. Add a `wtcli` subcommand in `src/tools/wtcli/main.cpp` that calls the new method
4. Add a `CliChannel::request` arm in `tools/wta/src/shell/wt_channel/cli_channel.rs` mapping a method name to the new `wtcli` subcommand
5. Rebuild WT, wtcli, and wta

## Architecture Notes

- **ShellManager** owns local terminals and the active `WtChannel`
- **CliChannel** shells out to `wtcli.exe` per call; ordinary requests use `CoCreateInstance` to reach WT's COM server. All methods, including `send_input` (via `wtcli send-keys`), go through this path. Managed event listeners instead use `wtcli --json listen --existing-only`, obtaining the already-running class factory through the ROT without activating Terminal.
- **Protocol discovery**: `WT_COM_CLSID` env var, inherited from the WT-spawned conpty
- **CLI subcommands** call `CliChannel::connect()` directly; no ShellManager needed
- **Pane identity** is discovered at startup via PID matching (list all panes, find ours)

### Event listener startup and retry

Managed listeners never create a Terminal server, on initial startup or retry.
If the running factory is not yet published or has been revoked during shutdown,
the listener reports a connection failure without falling back to COM activation.
WTA retains its bounded backoff: eight consecutive unstable attempts stop retries;
a subscription healthy for 30 seconds resets the count. A first post-subscription
failure retries immediately, still with `--existing-only`. Readiness is reported
only after `Subscribe` succeeds, and late recovery notifies the owning channel.
Missing listener readiness does not block chat, Autofix, listing, or resume.
Servers without a published running factory cannot support managed listeners;
public `wtcli listen` without this flag retains activating/headless compatibility.

### Managed notifications

WTA's event publisher always invokes `wtcli publish --stdin --existing-only`.
Passive notifications, including shutdown-time session-registry changes, reuse
the already-running COM factory and never start a replacement Terminal.
Missing or closing/incompatible factories produce the existing publication
warning; there is no activation fallback or change to chat/session startup.
Public `wtcli publish` without the flag retains normal activation. Ordinary
managed read requests and explicit interactive creation remain unchanged.

# Native interactive CLI creation

Delegation and native session resumes pass the resolved provider ID through
`wtcli --agent-provider`. Terminal stores that identity with the terminal content
before displaying the tab or split, so the Agents view and provider icon do not
wait for CLI startup hooks. This does not create a conversation ID, activity
state, or history row. Older protocol servers reject this capability explicitly.
Host-configured delegation carries the provider separately from its executable
through `wta delegate --delegate-agent-id`. Custom commands require that explicit
identity; their executable basename is not treated as a configured provider.
Tabs, panes, Recent Sessions, and the agent picker share the six committed
transparent PNG assets in `CascadiaPackage\AgentIcons\Masks` and the existing
monochrome bitmap tint pipeline. `AgentIconResources.xaml` supplies fresh control
instances for template consumers; it does not maintain separate vector artwork.
