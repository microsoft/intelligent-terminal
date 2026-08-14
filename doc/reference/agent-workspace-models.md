# Agent workspace models

Research date: 2026-08-12

## Conclusion

Intelligent Terminal is not unusual for using ACP's `cwd` plus
`additionalDirectories`; Zed already maps multi-root projects to that model.
It is unusual in **where those roots come from**:

- IDE clients start from a relatively stable project or workspace.
- Warp's first-party Agent follows terminal and Git-repository context without
  using ACP as its integration boundary.
- Intelligent Terminal must reconcile independent, changing pane directories
  with one long-lived Agent session per tab.

The resulting session/global grant model is therefore client policy layered on
top of ACP, not a protocol extension.

## ACP baseline

ACP v1 defines one absolute `cwd` as the primary root and optional absolute
`additionalDirectories` as extra roots. The effective ordered roots are
`[cwd, ...additionalDirectories]`; relative paths still resolve against
`cwd`.

`additionalDirectories` is capability-gated and exists only on session
lifecycle requests. The client must send the complete intended list again on
load or resume. ACP deliberately defines no mid-session mutation RPC and does
not make roots a sandbox or an operation approval.

ACP permission requests are also client-mediated. `ToolKind` is a category for
display and policy, and clients may automatically allow or reject requests.
ACP does not prescribe a persistent client policy.

Sources:

- [ACP Session Setup](https://agentclientprotocol.com/protocol/v1/session-setup)
- [Additional Workspace Roots RFD](https://agentclientprotocol.com/rfds/additional-directories)
- [ACP Tool Calls and Permission Requests](https://agentclientprotocol.com/protocol/v1/tool-calls)

## Client comparison

| Product | Workspace source | Multiple roots | Runtime directory change | Permission model |
|---|---|---|---|---|
| Zed ACP | Project worktree list | Yes. First ordered worktree is `cwd`; remaining roots become `additionalDirectories` when supported. | No ACP mutation path found; roots are composed for new/load/resume. | External Agent owns native tools/config; Zed may mediate ACP/tool forwarding. Zed's first-party Agent has separate per-tool regex rules. |
| JetBrains AI Assistant ACP | IDE project/session | Public documentation does not specify its `cwd` or `additionalDirectories` mapping. | Not documented. | External Agent integration and MCP exposure are documented; filesystem approval policy is not. |
| Qt Creator ACP Client | User-selected working directory at connect time | No `additionalDirectories` implementation found in the current ACP client source. | Reconnect or create/load with another working directory. | ACP permission requests are surfaced; proposed edits are reviewed in chat. |
| CodeCompanion.nvim | Neovim's current working directory | No `additionalDirectories` implementation found. | A later new/load reads the then-current `getcwd()`; active-session behavior is not documented. | Interactive permission UI with diff preview. |
| Warp Agent | Active terminal/PTY and Git repository context | Not modeled as ACP roots. `@` file search spans the current Git repository. | Naturally follows the terminal session; new window/tab/pane cwd inheritance is configurable. | Global Agent Profiles plus per-session approvals; command allow/deny lists inspect command text. |
| Intelligent Terminal | Owning tab's active pane at Agent-session creation | Yes: global roots plus ACP-session roots. | Explicit `/add-dir`; the ACP primary `cwd` remains stable. | Client-owned read/create/input policy uses ACP `ToolKind`, locations, and effective roots. |

### Zed

Zed is the closest ACP comparison. A project can contain multiple root folders,
and each Agent thread is tied to a project. Its ACP implementation preserves
the project's ordered worktree list: the first root becomes `cwd`, and the
remaining roots are sent as `additionalDirectories` only when the Agent
advertises support. It repeats that mapping for new, load, and resume.

This is project-driven rather than permission-driven. Adding a folder changes
the project model; Zed does not document a session/global directory-grant
lifetime UI equivalent to Intelligent Terminal's.

Sources:

- [Zed External Agents](https://zed.dev/docs/ai/external-agents)
- [Zed Parallel Agents and Multi-Root Projects](https://zed.dev/docs/ai/parallel-agents)
- [Zed Windows and Projects](https://zed.dev/docs/windows-and-projects)
- [Zed ACP root mapping](https://github.com/zed-industries/zed/blob/6ae52316bedfc46e07ad740d647c669206853503/crates/agent_servers/src/acp.rs)
- [Zed Tool Permissions](https://zed.dev/docs/ai/tool-permissions)

### JetBrains and Qt Creator

JetBrains documents ACP Agent installation, subprocess configuration, MCP
exposure, and a WSL limitation, but does not publicly describe how project
roots become ACP lifecycle parameters. It should not be treated as evidence
for either multi-root support or persistent path grants.

Qt Creator exposes a single **Working directory** before connecting. Its
current public ACP client source creates and loads sessions with that directory
and contains no `additionalDirectories` usage. Files can still be attached as
prompt context; attachment is not a workspace grant.

Sources:

- [JetBrains ACP](https://www.jetbrains.com/help/ai-assistant/acp.html)
- [Qt Creator ACP Client](https://github.com/qt-creator/qt-creator/blob/master/src/plugins/acpclient/AcpClientDescription.md)
- [Qt Creator ACP source](https://github.com/qt-creator/qt-creator/tree/master/src/plugins/acpclient)

### Neovim

ACP is available through plugins rather than Neovim itself. CodeCompanion is a
documented example: session list, new, and load use `vim.fn.getcwd()` as one
`cwd`; its current implementation has no `additionalDirectories` usage. A
later lifecycle request can therefore observe a changed Neovim cwd, but the
plugin does not document changing an active ACP session's root.

Sources:

- [ACP Clients](https://agentclientprotocol.com/get-started/clients)
- [CodeCompanion ACP Support](https://github.com/olimorris/codecompanion.nvim/blob/main/doc/agent-client-protocol.md)
- [CodeCompanion ACP implementation](https://github.com/olimorris/codecompanion.nvim/blob/9a8e8602a7c72d10a827880fbb6fcd8bfa3830c7/lua/codecompanion/acp/init.lua)

## Warp

Warp's own Agent is not hosted through ACP. The same first-party Agent harness
runs in the Warp app, Warp Agent CLI, and Oz cloud platform.

Its local interaction model is terminal-driven:

- Agent and shell commands share one input surface.
- Full Terminal Use attaches the Agent to the active PTY, including an already
  running interactive process.
- New windows, tabs, and panes can start in home, the previous session's
  directory, or a configured directory.
- Inside a Git repository, `@` context searches from the repository root even
  when the shell is in a subdirectory.

Warp therefore combines terminal state with repository context rather than
treating the current shell directory as a strict authorization root.

### Does Warp have a global Agent?

Not as one process-wide conversation or one Agent session shared by every
pane. Warp has:

- the same Agent product across app, CLI, and cloud;
- global reusable Profiles, rules, skills, and permission defaults;
- a management view aggregating local conversations and cloud runs; and
- Oz orchestration for independent background tasks and child agents.

The unit of execution remains a conversation or run with its own context.
Warp's documentation does not describe a persistent global allowed-directory
list comparable to Intelligent Terminal. Its persistent controls are
operation-oriented Profiles and command/MCP allow/deny lists; directory and
repository context is selected by the terminal session or execution
environment. Local Agent documentation does not describe a directory-level OS
sandbox. Oz is different: hosted runs use isolated, disposable containers
whose environments declare one or more repositories.

Sources:

- [Agents in Warp](https://docs.warp.dev/agents/getting-started/agents-in-warp/)
- [Working Directory](https://docs.warp.dev/terminal/more-features/working-directory)
- [Full Terminal Use](https://docs.warp.dev/agents/capabilities/full-terminal-use/)
- [Agent Profiles and Permissions](https://docs.warp.dev/agents/capabilities/agent-profiles-permissions/)
- [Using @ to Add Context](https://docs.warp.dev/agents/local-agents/agent-context/using-to-add-context/)
- [Warp Agent CLI](https://docs.warp.dev/agents/cli/)
- [Oz Platform](https://docs.warp.dev/platform/overview/)
- [Oz Environments](https://docs.warp.dev/platform/environments/)
- [Warp-hosted execution](https://docs.warp.dev/platform/warp-hosting/)
- [Managing Agents](https://docs.warp.dev/platform/managing-cloud-agents/)

## Implications for Intelligent Terminal

1. Keep `cwd` stable per ACP session. Automatically following every shell
   `cd` would change relative-path semantics and trust shell-reported data.
2. Use explicit grants for extra pane or repository directories. This matches
   ACP's multi-root model without inventing a mutation RPC.
3. Keep path scope separate from operation approval. Warp and Zed also expose
   operation policy independently from context selection.
4. Preserve per-tab ownership even though the Agent process is shared. Shared
   transport is an optimization, not a shared workspace.
5. Treat capability absence honestly. Local grants can guide Intelligent
   Terminal policy, but an Agent that does not advertise
   `additionalDirectories` has not received those roots through ACP.
