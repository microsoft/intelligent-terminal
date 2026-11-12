# WSL agents independently of terminal profiles

Base: `microsoft/intelligent-terminal`, `main`,
`ed841e9ce117fc3b73c33701172933abcdd28854`.
Worktree: `Q:\official\intelligent-terminal-worktrees\wsl-agent-selection`.
Branch: `dev/yeelam/wsl-agent-selection`.

## Requirement and acceptance

A user working in PowerShell or Command Prompt must be able to choose an
installed WSL agent directly in global Agent settings, without changing their
terminal profile. The global default must also work in WSL profiles. A profile
can override that default with an exact Windows or distribution-specific agent.
Agent pane and command-palette delegation are separate selections. Also generate
native agent profiles for discovered WSL agents.

The clarified performance requirement separates discovery from ACP use.
Explicitly opening Agent settings actively probes registered distributions,
even if this starts a stopped distribution, to show an accurate installed-agent
list. Ordinary startup, settings-file loading/reloading, and native profile
generation reuse saved/cached evidence without these expensive Linux probes.
Discovery must not authorize unused ACP processes: neither a running distro nor
finding its executables permits Windows-style chat/history pairs. Automatic WSL
ACP eligibility requires the exact configured default provider and distribution,
or actual use of that exact provider/source.

Deliver source-backed investigation, a minimal MVVM design, a reasonable
unit-test inventory, implementation, an isolated worktree Dev deployment, and
a fork Copilot review loop with independently assessed feedback. Preserve the
regular installation, settings, sessions, hooks, and unrelated worktrees.

## Findings in the baseline

| Boundary | Evidence | Actual gap |
| --- | --- | --- |
| Global picker | `src\cascadia\TerminalSettingsEditor\AIAgentsViewModel.cpp:527-615` | Builds bare provider entries from Windows availability only. There is no distribution discovery or execution-source selection. |
| Profile picker | `src\cascadia\TerminalSettingsEditor\ProfileViewModel.cpp:85-166,398-475` | Already has qualified `host:<agent>` and `wsl:<distro>:<agent>` entries, but only probes a distribution when the profile command is recognized as WSL. A Windows profile explicitly clears WSL candidates. |
| Persisted identity | `src\cascadia\TerminalSettingsModel\MTSMSettings.h:81-90`; `src\cascadia\inc\AgentPaneBackend.h:13-83` | Global settings store the provider but not its source. Profiles store both. Changing labels alone cannot fix runtime selection. |
| Global launch | `src\cascadia\TerminalApp\TerminalPage.cpp:1807-1851,4451-4471` | Global command generation and helper arguments explicitly default to Windows. |
| Profile launch restriction | `src\cascadia\TerminalApp\TerminalPage.cpp:2884-2916,4401-4437` | A WSL backend is rejected when the pane's reported shell differs from the selected distribution. This intentionally old guard also excludes PowerShell/cmd and a different WSL distro. |
| Settings reload | `src\cascadia\TerminalApp\TerminalPage.cpp:2537-2650,2850-2990` | Reconciliation tracks provider/model/profile backends, but no global execution source. A source change for the same provider would otherwise be missed. |
| Delegation | `src\cascadia\TerminalApp\TerminalPage.cpp:2298-2350`; `tools\wta\src\cli\delegate.rs:185-220,463-537` | WTA already accepts exact host/WSL delegation. C++ supplies WSL only for a profile override; Windows cwd is not translated for explicit WSL delegation. |
| Model discovery | `src\cascadia\TerminalSettingsEditor\AIAgentsViewModel.cpp:1887-1998`; `tools\wta\src\protocol\acp\probe.rs:55-108,151-174` | Settings probes always start on the host. Model cache entries are keyed by provider, so they cannot distinguish Windows and WSL accounts/catalogs. |
| WSL execution | `tools\wta\src\agent_source.rs:22-90`; `tools\wta\src\protocol\acp\spawn.rs:584-690` | Already source-aware: the master process pool distinguishes source and launches ACP in an explicit distribution. No new transport or second orchestrator is needed. |
| Working directory | `src\cascadia\inc\AgentSourceUtils.h:211-241`; `tools\wta\src\agent_source.rs:104-188` | Helper and agent cwd are already separate, but the original Rust drive conversion fabricated `/mnt/<drive>`. That is not evidence that the directory is mounted or accessible. Linux paths from a different distribution cannot be assumed to name the same workspace. |
| Session MCP | `tools\wta\AGENTS.md:35-58`; `tools\wta\src\master\session_mcp.rs` | A distro-local relay already reaches the master-owned endpoint. Terminal actions must continue targeting the owning helper's actual shell, not be retargeted to match the agent. |
| MVVM ownership | `src\cascadia\TerminalSettingsEditor\MainPage.cpp:1469-1495` | Settings already has asynchronous availability discovery and distributes results to profile VMs. Extend that existing owner rather than introducing a discovery service or putting process work in XAML code-behind. |

## What is not a fundamental blocker

ACP over Windows-to-WSL stdio, distribution-specific process pooling, MCP relay,
canonical provider policy checks, and source-qualified session identity already
exist. An agent's execution environment need not equal its target terminal's
environment. For example, a WSL Copilot process can request a command in an
owning PowerShell pane through the existing confirmation-gated terminal tools.
Its own shell tools still run in WSL; these are intentionally different paths.

## Constraints and failure behavior

- Never put `wsl:Ubuntu:copilot` into the canonical provider setting/policy ID.
- A selected missing distribution or failed probe must not silently switch to a
  host agent. Keep the saved choice visible as unavailable.
- No custom-provider-in-WSL editor or native CLI profile customization redesign
  is required. Existing custom command settings remain Windows-owned.
- Preserve host BYOK behavior. WSL uses its own provider environment; do not
  transfer Windows credentials or pretend a host BYOK model is a WSL catalog.
- Discover registered user distributions, not merely the current profile or
  currently running ones. Exclude utility distributions consistently with
  existing WSL profile generation. Explicit Agent-page navigation may start
  distributions for bounded executable-presence probes; ordinary loading must
  not. Retain saved choices without implying current availability. Discovery
  starts no ACP/model probes, installs no agents, and changes no hooks or global
  registrations.
- Treat discovery failure as unknown, log it, and retain saved selections.
  Bound subprocesses and ignore stale UI publication after navigation/reload.
- Preserve explicit tab overrides, stashed panes, profile precedence, policy,
  qualified session ownership, and Windows helper working directories.
- Cross-distro POSIX paths are not portable. A launch without trustworthy cwd
  uses the selected distribution's home, not an unrelated same-looking path.

## Evidence still required

Implementation and validation results belong in `validation.md`. This document
describes inspected baseline source, not a claim that the requested behavior,
deployment, or review has already passed.

## Reconciliation with the earlier cwd PR

The user identified #265, **wta: support WSL agent cwd namespaces**, head
`152e9c36f6f66784c342c485fe9b840c34e55ffd`. Its preserved diff and namespace
module are local evidence beside this document. It correctly distinguishes
the helper's Windows working directory from the ACP agent's namespace, preserves
a genuinely selected System32 directory, bounds `session/new` retries, and
avoids replaying `session/load`. However, both its `windows_to_mnt` and
`mnt_to_windows` functions assume the conventional mount root. Importing that
PR unchanged would retain the exact gap the user called out.

The current master also has newer replacement/routing rollback, session MCP
capability ownership, generation-scoped history, and exact WSL-use eligibility.
The older PR's master/history call sites cannot safely replace those paths.
Instead, adapt its relevant namespace and retry behavior to those existing
owners. Reuse an already-completed per-instance history result for opaque
custom wrappers; do not start an additional history process or inventory just
to discover a namespace. A namespace alone cannot identify a distro.

For an explicitly selected WSL source, `wslpath -a -u` runs **inside that exact
distro**, with the Windows path passed as a positional argument. Test the
result with `-d` in the same process: a syntactically translated path is not
proof that a drive is mounted. A failed/unavailable mapping uses the target
home with a diagnostic; it must not fabricate a project path. Source-qualified
Linux/WSL-UNC paths retain their existing handling. No inverse `/mnt` heuristic
is valid without corresponding source/mount evidence.

Read-only live inspection on the already-running Ubuntu distro confirmed that
both `C:\Windows\System32` and this worktree's `Q:` path translate and exist in
this environment. This proves only the actual conventional-root environment,
not custom mount roots. Deterministic mapping tests supply `/windows/c/...`
and an unmapped-drive failure without changing any shared distro configuration.
