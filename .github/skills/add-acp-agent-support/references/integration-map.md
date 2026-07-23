# ACP Agent Integration Map

Use this map as a search guide, not a promise that filenames or symbols never
change. Search for all current built-in IDs before editing and follow the
nearest current implementation.

## Capability Matrix

Record these facts before implementation:

| Field | Evidence required |
|-------|-------------------|
| Canonical ID and display name | Stable lowercase ID and official product name |
| Executable and search order | Actual Windows shims (`.exe`, `.cmd`, and others if needed) |
| ACP ownership | Native CLI or named adapter package/repository |
| Exact ACP command | Long-running stdio command, including required subcommand/flags |
| ACP version behavior | Tested CLI/adapter version and protocol initialization result |
| Authentication | ACP in-protocol method or exact external login/logout/status commands |
| ACP model selection | ACP model API and/or server-start flags |
| Delegate command | Interactive initial-prompt syntax, model syntax, and argument order |
| Resume/new session | Exact flag/subcommand and identifier semantics, or unsupported |
| Session hooks | Official hook/plugin API, lifecycle events, install location, ACP-mode suppression, or unsupported |
| Installation | Official package ID/command and documentation URL |
| Branding | Official SVG source and license/usage terms |
| Known limitations | Hooks, history, WSL, models, auth refresh, or delegate omissions |

Do not infer capabilities from another agent. Exercise the installed CLI's
help and ACP behavior directly.

## WTA (Rust)

### `tools/wta/src/agent_registry.rs`

Add an `AgentProfile` and update tests for:

- canonical ID and display name;
- executable resolution;
- native ACP flags or full adapter launch command;
- ACP-specific versus delegate model flags;
- `AcpAuthFlow`;
- delegate prompt shape;
- install and auth guidance;
- resume and caller-chosen session ID support.

Also inspect command-to-agent identification, adapter aliases, known-ID tests,
and default/fallback behavior. If the new CLI needs a prompt shape the registry
cannot represent, extend the type generically and test existing agents for
regressions rather than hardcoding a one-off branch.

### `tools/wta/src/agent_check.rs`

Add the exact external login command only when external auth is real. Check
whether enterprise-host arguments are agent-specific and must be ignored.
Test login command generation.

### Session management

If the agent supports ACP `session/list`, ACP `session/load`, or an interactive
CLI resume command, wire its canonical ID through the complete session
subsystem. Updating `AgentProfile.resume_flag` alone is insufficient.

Search for every exhaustive `CliSource` match and update the applicable
surfaces:

- `agent_sessions.rs`: add the typed variant plus `parse` and `from_agent_id`;
- `app.rs`: map the typed source back to the canonical ID so resume capability
  checks and `<cli> <resume flag> <session id>` synthesis work;
- `session_registry.rs`: preserve the source through helper/master wire
  serialization instead of degrading it to `Unknown`;
- `session_history.rs`, `main.rs`, and `ui/agents_view.rs`: keep diagnostic and
  UI labels exhaustive;
- `wsl_acp.rs`: add ACP session discovery only when the agent's ACP server
  actually supports `session/list`.

Add regression tests for ID parsing, wire round-trips, current-agent filtering,
resume dispatch, and the exact CLI resume command. For CLI resume tabs, pass the
stored session title to `wtcli new-tab`; do not force suppression of later
application-title updates unless the product explicitly requires a fixed title.

The characteristic missed-mapping failure is:

```text
Cannot resume session <id>: its source agent is unknown to this build.
```

When this appears, inspect the helper log's
`activate_agent_session_with_shift` entry. A known built-in showing
`cli=Unknown("custom")` means a session conversion boundary is missing.

### `tools/wta/src/coordinator.rs`

Delegate support must preserve:

- executable and base arguments;
- model argument placement;
- interactive initial-prompt placement;
- multiline prompt integrity;
- quoting for direct Windows, pwsh, Windows PowerShell, and WSL;
- clear rejection of empty or invalid executable command lines.

Add assertions for the complete command shape, not merely the presence of the
agent name.

### `tools/wta/src/main.rs` and ACP modules

Update user-facing supported-agent lists and inspect ACP initialization,
authentication, model selection, and probing for agent-specific assumptions.
Do not add protocol special cases when profile metadata or ACP capabilities can
drive the behavior.

### Session hooks and history

Implement hooks only when the CLI exposes a documented hook or plugin API that
can observe normal interactive sessions. Inventory the complete lifecycle:

- bundle files under `tools/wta/wt-agent-hooks/<agent>`;
- canonical CLI filtering for `wta hooks install/status/uninstall`;
- startup auto-upgrade and its per-bundle version cache;
- ownership markers, partial-install recovery, and user-file protection;
- session, prompt, tool, notification, error, idle, and end event mappings;
- child/subagent filtering when internal sessions should not become rows;
- an ACP-mode guard so the shared agent-pane process does not emit duplicate
  hook-backed sessions;
- UTF-8 payload handling through every process boundary.

Follow the current implementations in `agent_hooks_installer.rs`,
`wt-agent-hooks`, and the session registry rather than assuming every CLI has a
marketplace. Some CLIs require a command-driven plugin install, while others
require a managed copy into a global plugin directory. Install ownership
metadata last so it proves a complete install, and remove it last so failed
cleanup remains retryable.

## Terminal (C++/XAML)

### `src/cascadia/inc/AgentRegistry.h`

Add the agent to the ACP built-in list. Add it to the delegate list only when
interactive delegation is supported. Update fixed array sizes and preserve GPO
filtering through `FilteredAcpAgents()` and `FilteredDelegateAgents()`.

### ACP command resolution

Search `src/cascadia/TerminalApp/TerminalPage.cpp` and settings code for the
existing built-in ACP command mappings. Ensure the agent pane and model probe
resolve to the same exact ACP command:

- `TerminalPage.cpp` for the runtime agent launch;
- `TerminalSettingsEditor/AIAgentsViewModel.cpp` for model probing and setup
  state.

An ACP server may accept model changes through protocol even when its
interactive CLI accepts a `--model` flag. Do not append unsupported flags to
the server command.

### Settings, telemetry, and discoverability

Search these areas for explicit current-agent lists:

- `TerminalSettingsModel/CascadiaSettingsSerialization.cpp` for sanitized
  telemetry IDs;
- Settings Editor and TerminalApp resources for localized/fallback names;
- first-run experience and quick selector consumers of `AgentRegistry.h`;
- CLI help text and settings schema/default descriptions.

Keep custom commands classified as `custom`; never emit a path or arbitrary
command as a telemetry provider ID.

When hooks are supported, also update the first-run scoped hook install and the
Settings hook status/remove surface. Keep the WTA JSON status schema and the C++
parser synchronized. A detected CLI with no hooks should have an intentional
UI state, and an install left on disk after the CLI is removed must remain
removable.

### Branding

Search `TerminalApp/AgentPaneContent.cpp` and `.xaml` for logo selection and
visibility. Add an official, license-compatible vector asset under the
packaging asset area when appropriate. Ensure:

- deterministic mapping from canonical/display name to the new logo;
- an intentional unknown-agent fallback;
- light and dark theme contrast;
- high-contrast support through theme resources rather than a fixed fill;
- only the selected logo is visible.

## Policy and Documentation

`policies/IntelligentTerminal.admx` defines the generic `AllowedAgents`
`REG_MULTI_SZ`. Runtime filtering normally needs no per-agent schema change,
but update `policies/en-US/IntelligentTerminal.adml`:

- valid identifier list;
- displayed built-in count;
- textbox hint.

Update `README.md` and `doc/faq.md` for support, installation/auth, and honest
limitations. Search for every old built-in count or exhaustive agent list.
Do not include the agent in hooks/history documentation unless those separate
features are implemented.

## Useful Searches

Run narrow searches from the repository root:

```powershell
rg 'BuiltinAcpAgents|BuiltinDelegateAgents' src\cascadia
rg 'copilot|claude|codex|gemini' tools\wta\src src\cascadia policies README.md doc\faq.md
rg 'sanitizeProviderId|probe-models|build_login_cmd|delegate_prompt' tools\wta\src src\cascadia
rg 'enum CliSource|known_cli_id|SessionHookCliSource|clis_to_scan' tools\wta\src
rg 'hooks install|hooks status|hooks uninstall|agent_hooks_installer|wt-agent-hooks' tools\wta src\cascadia
rg 'AgentLogoKind|AgentName_' src\cascadia
rg 'AllowedAgents|built-in AI agents' policies
```

Replace the exemplar agent-ID expression with the current built-in set when
the repository evolves.
