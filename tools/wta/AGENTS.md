# WTA development

These rules apply to `tools/wta/`. The repo-level `AGENTS.md` is the canonical
source for build, deployment, supported providers, runtime paths, and log names.
Do not duplicate those facts here.

## Roles and ownership

WTA is a Rust binary with three launch modes:

- **Master** (`--master <pipe>`): lazily owns a pool of agent CLI subprocesses
  and their ACP stdio connections, accepts helper named-pipe connections, and
  routes each ACP SessionId to its owning helper. Helpers with the same
  master-derived agent key share a process. Implementation: `src/master/mod.rs`.
- **Helper** (`--connect-master <pipe>`): one ratatui UI per agent pane. It is
  an ACP client of master and owns pane-local UI state and `ShellManager`.
  Implementation: `src/helper/mod.rs` and `src/app.rs`.
- **CLI command**: one-shot commands such as `list-panes`, `capture-pane`,
  `resolve-command`, `delegate`, `hooks`, and `sessions`. Dispatch:
  `src/cli/`.

Bare `wta` without a role flag or subcommand is invalid. The session MCP
listener is a master-owned service, not a fourth launch mode.

## Protocol boundaries

### ACP

ACP means Agent Client Protocol and is used on two hops:

1. Master is the ACP client of the agent CLI over stdio.
2. Helper is an ACP client of master over the named pipe; master acts as the
   ACP agent and multiplexes requests.

The owning helper services ACP client requests such as permission and
`terminal/*` operations. Ordinary agent-reported tool calls are display-only:
WTA does not own those processes or invent process controls for them.

### Session MCP

Master owns one loopback Streamable HTTP listener and publishes an independent
server name and bearer capability for each eligible ACP session. WSL sessions
use an on-demand distro-local relay to that listener. The endpoint exposes:

- `run_command`
- `open_workspace`
- `run_command_in_workspace`
- `delegate_task`
- `request_user_input`

Treat the bearer capability as the session identity. Revoke it when the exact
agent CLI instance exits. Route requests through `session_to_helper`; the MCP
endpoint must not execute terminal actions.

### WT protocol

All Windows Terminal operations use `CliChannel`, which invokes `wtcli.exe`;
`wtcli` activates the COM `IProtocolServer` discovered through `WT_COM_CLSID`.

- WTA wrapper: `src/shell/wt_channel/cli_channel.rs`
- CLI client: `src/tools/wtcli/main.cpp`
- IDL: `src/cascadia/TerminalProtocol/TerminalProtocol.idl`
- COM server: `src/cascadia/WindowsTerminal/TerminalProtocolComServer.cpp`

Do not treat `WT_COM_CLSID`, `pipe-id`, or `set-env` as security boundaries.
The human-facing WTA CLI intentionally does not expose direct keystroke
injection; internal `ShellManager::wt_send_input` uses `wtcli send-keys`.

## Agent commands

The Terminal settings layer resolves built-in IDs to ACP command lines.
`src/cascadia/inc/AcpModelUtils.h` is authoritative for those commands and
adapter versions; do not copy them into WTA code or documentation.

Custom agents arrive as a resolved `--agent` command plus their canonical
`custom:<name>` ID. Do not re-parse a known ID from an adapter command when the
host supplied `--agent-id`.

## State and routing invariants

- `session_to_helper` is the authoritative ACP SessionId routing map.
- Per-tab state mutations must preserve both tab and window identity.
- Helper disconnect, agent exit, pane close, and master restart are distinct
  lifecycle events; do not collapse them into one generic failure.
- `ShellManager` owns ACP client terminals. Agent-reported ordinary tool calls
  remain agent-owned.
- Command resolution must preserve the active pane's cwd, host PATH, shell,
  and PowerShell profile behavior. Return `exists`, `not_found`,
  `indeterminate`, or `unsupported` precisely.
- Runtime state and cache paths must go through `runtime_paths.rs`.

## Session management

The registry models activity and liveness separately:

- Activity: `Idle`, `Working`, `Attention`, or `Error`
- Liveness: `Live`, `Ended`, or `Historical`

Agent-pane and shell-pane sessions have different focus/resume behavior.
Keep routing decisions in the pure `session_mgmt::decide_enter_action`
boundary and side effects in its dispatch callers.

The picker currently defaults to shell-pane sessions through
`MVP_SESSIONS_ORIGIN_FILTER`; `WTA_SESSIONS_SHOW_AGENT_PANE=1` is the debug
override. Do not change that product behavior incidentally.

Detailed tracking and resume behavior belongs in:

- `doc/specs/hybrid-agent-session-tracking.md`
- `doc/specs/per-cli-history-filtering.md`
- `doc/specs/wsl-session-management.md`

## Build and test

Run the explicit-target commands in the repo-level `AGENTS.md` from the
repository root. Do not alternate between host-target and explicit-target Cargo
outputs in one worktree because the package project prefers the explicit-target
binary.

Run the WTA test suite for every behavior change covered by, or deserving,
unit tests. A successful Cargo build or C++ build does not compile
`#[cfg(test)]` code.

Use the smallest relevant test while iterating, then run:

```powershell
cargo test --target x86_64-pc-windows-msvc --manifest-path tools/wta/Cargo.toml
```

If live processes lock the output, stop only PIDs whose executable paths exactly
match the target being rebuilt. Do not kill all WTA processes by name.

## Dependencies and generated notices

CI resolves the `ms-prod-1.93` pin in `rust-toolchain.toml` through MSRustup.
The documented repo-root local commands use the installed active Rust toolchain
while still loading the repo's static-CRT target configuration. Keep code
compatible with Rust 1.93 and avoid dependencies that do not support static CRT.

When `Cargo.toml`, Cargo features, or `Cargo.lock` changes the shipped
dependency graph, regenerate and commit both `tools/wta/cgmanifest.json` and the
generated WTA block in `/NOTICE.md`:

```powershell
$env:RUSTUP_TOOLCHAIN = 'stable'
pwsh -File .\build\scripts\Generate-WtaThirdPartyNotices.ps1
```

The generator requires PowerShell 7+. Generated artifacts are not edited by
hand.

## Rust implementation conventions

- Localize user-facing strings through `t!(...)`.
- Use structured `tracing` fields and stable targets; do not log credentials,
  bearer capabilities, or full provider configuration.
- Initialize logging once near process startup and flush it before any
  `std::process::exit`.
- Preserve explicit error context across async/process/protocol boundaries.
- Add tests around reducers, routing decisions, protocol translation, and
  parsing rather than testing private implementation details.
- Format with `cargo fmt`; address relevant Clippy findings without broad,
  unrelated cleanup.

See `.github/instructions/rust-wta.instructions.md` for file-scoped coding
rules and `tools/wta/README.md` for human-facing CLI and diagnostics.
