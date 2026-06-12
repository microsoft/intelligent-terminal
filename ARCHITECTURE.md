# Architecture

For an up-to-date architectural overview of Intelligent Terminal, see:

- [`AGENTS.md`](./AGENTS.md) — concise overview of the shipped architecture:
  WTA helper+master, the COM `IProtocolServer` integration surface, `wtcli`,
  the per-tab pre-warmed agent pane, autofix, hooks auto-upgrade, log layout,
  and the build flow.
- [`doc/specs/Multi-window-agent-pane.md`](./doc/specs/Multi-window-agent-pane.md)
  — authoritative spec for the per-tab + per-window agent pane routing model
  (helper-per-tab, stash/restore, cross-window drag, owner-lock).
- [`tools/wta/OVERVIEW.md`](./tools/wta/OVERVIEW.md) — WTA crate overview.
- [`doc/specs/`](./doc/specs/) — feature-level specs (agent OOBE, agent
  failure handling, connection resilience, LLM agent event integration, …).
- [`doc/wtcli-commands.md`](./doc/wtcli-commands.md) — `wtcli` reference.

Inherited Windows Terminal / OpenConsole architecture documents live under
[`doc/`](./doc/) (ORGANIZATION.md, terminal-v2-roadmap.md, the process model
spec, etc.). Those describe the underlying Windows Terminal codebase that
Intelligent Terminal forks from.
