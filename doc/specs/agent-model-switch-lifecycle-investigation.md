# Agent model switching lifecycle investigation

## Summary

Changing the model in **Settings > Agents** currently rebuilds the Terminal
agent pane and restarts the process-wide shared WTA master. This is why a model
selection can resize terminal content, show an intermediate `Connecting...`
pane, disturb focus, and disconnect agent sessions in other tabs or windows.

The model change itself does not require a Terminal pane rebuild.

- Cloud model to cloud model should be applied in place through ACP
  `setSessionModel`.
- Cloud/local mode changes may require replacing the affected agent CLI and
  creating new ACP sessions because the local provider changes process launch
  configuration.
- Neither case should close or recreate the Terminal agent pane.

The current behavior was introduced by
[PR #554](https://github.com/microsoft/intelligent-terminal/pull/554),
which moved `acpModel` out of the runtime hot-update path and into the shared
master restart path. This partially reversed the behavior implemented by
[PR #219](https://github.com/microsoft/intelligent-terminal/pull/219).

## Reported symptom

After selecting a different cloud model in Settings and saving:

1. The existing agent pane disappears.
2. The terminal pane expands into the released space.
3. A new agent pane is split into the tab.
4. The new pane displays `Connecting...`.
5. Terminal, agent, and Settings content can briefly appear in an unexpected
   layout while XAML layout, focus, and connection initialization complete.

This is not only a rendering refresh. The application physically closes and
recreates nodes in the pane tree.

## Observed runtime timeline

The captured logs use UTC. The corresponding local timestamps below use UTC+8.

### Switch to `gpt-5.6-sol`

```text
15:54:26.411  emitting agent_config_changed (hot settings update)
15:54:26.435  _RebuildAgentStack: entered
15:54:26.439  _RebuildAgentStack: agent settings changed, rebuilding
15:54:26.443  _TeardownAgentPane: closing agent pane on tab
15:54:26.449  agent pane closed
15:54:26.485  releasing wta-master pid=5416
15:54:26.634  _OpenOrReuseAgentPane called
15:54:26.636  no agent pane on focused tab, creating new one
15:54:26.638  _AutoCreateHiddenAgentPaneShared: wta-master pid=10152
15:54:26.654  helper conpty child spawned
15:54:30.460  session bound, current_model_id=gpt-5.6-sol
```

The replacement master was launched with:

```text
copilot --acp --stdio --model gpt-5.6-sol
```

### Switch to `claude-opus-4.8`

```text
15:55:47.475  _RebuildAgentStack: entered
15:55:47.477  _RebuildAgentStack: agent settings changed, rebuilding
15:55:47.478  _TeardownAgentPane: closing agent pane on tab
15:55:47.480  agent pane closed
15:55:47.513  releasing wta-master pid=10152
15:55:47.628  _OpenOrReuseAgentPane called
15:55:47.630  no agent pane on focused tab, creating new one
15:55:47.632  _AutoCreateHiddenAgentPaneShared: wta-master pid=39916
15:55:47.646  helper conpty child spawned
15:55:51.011  session bound, current_model_id=claude-opus-4.8
```

The replacement master was launched with:

```text
copilot --acp --stdio --model claude-opus-4.8
```

## Current code path

### 1. Settings updates `acpModel`

`AIAgentsViewModel::CurrentAcpModelEntry` writes the selected model into the
Settings clone:

```text
src/cascadia/TerminalSettingsEditor/AIAgentsViewModel.cpp
```

`MainPage::SaveButton_Click` then writes the clone to `settings.json`:

```text
src/cascadia/TerminalSettingsEditor/MainPage.cpp
```

### 2. Settings reload is broadcast

The settings directory watcher calls `AppLogic::ReloadSettings`, and the
resulting `SettingsChanged` event is delivered to Terminal windows:

```text
src/cascadia/TerminalApp/AppLogic.cpp
```

`TerminalPage::_RefreshUIForSettingsReload` eventually calls:

```cpp
_RebuildAgentStack();
```

### 3. `acpModel` is treated as master identity

`TerminalPage::_CaptureAgentSettingsSnapshot` includes `AcpModel`, and
`_AgentSettingsChanged` compares it:

```cpp
return a.acpAgent != b.acpAgent ||
       a.acpCustomCommand != b.acpCustomCommand ||
       a.acpModel != b.acpModel ||
       a.customModelLaunch != b.customModelLaunch ||
       a.profileBackends != b.profileBackends;
```

`_RebuildAgentStack` then classifies a cloud model change as a master
configuration change:

```cpp
const bool cloudModelChanged =
    _lastAgentSettings.acpModel != current.acpModel;
const bool masterConfigurationChanged =
    customMasterArgsChanged ||
    cloudModelChanged ||
    customModelLaunchChanged;
```

### 4. The Terminal pane tree is rebuilt

For every affected tab, `_RebuildAgentStack` calls:

```cpp
_TeardownAgentPane(tabImpl);
```

`_TeardownAgentPane` calls `Pane::Close`. Closing the leaf causes its parent to
run `Pane::_CloseChild`, promote the remaining terminal content, and recompute
the split grid.

After restarting the master, the active tab is reopened through:

```cpp
_OpenOrReuseAgentPane(false, L"SettingsReload");
```

The creation path calls:

```cpp
tab->SplitPaneAtRoot(splitDirection, newPane);
```

The visible layout therefore transitions through:

```text
terminal + agent
    -> terminal only
    -> terminal + replacement agent
```

This explains why a model setting affects terminal dimensions and focus.

### 5. XAML work is split across multiple phases

The new agent pane contains a new `TermControl`. Its connection starts after
layout initializes its swapchain, and focus is posted through a low-priority
dispatcher callback. The terminal can therefore render intermediate visual
states between tree mutation and complete agent initialization.

`Pane::_CloseChildRoutine` already contains a model-switch-specific workaround:
agent pane closes are forced to complete synchronously because a deferred close
animation previously ran after the tree had already been split again and caused
a `TerminalApp.dll` access violation.

That workaround prevents a crash, but it does not make pane reconstruction an
appropriate model-switch mechanism.

## Cross-window impact

`SharedWta` is process-wide. A model change in one window can therefore affect
helpers owned by other tabs and windows.

Immediately before the observed restart, the master reported four live
helpers. Restarting the master caused other helpers to report:

```text
ACP I/O loop to master ended - pipe closed (master gone)
failure class="transport_lost"
```

The old master session registry was discarded. The replacement helper created
a new ACP session with a new session ID.

Settings reload is also delivered to every Terminal window. A window currently
showing the Settings tab may defer its own pane rebuild, while another window
with an active terminal tab can immediately restart the same process-wide
master. This makes the resulting connection and layout transitions difficult
to coordinate.

## Regression history

### PR #219: runtime model hot-update

Commit `0f3067eb29` was titled:

> Hot-reload acp-model & delegate settings without restarting the agent pane

It separated settings into:

- agent identity, which could require process replacement; and
- runtime settings such as `acpModel`, which were delivered through
  `agent_config_changed`.

WTA applied `acpModel` to live sessions using `setSessionModel`, preserving the
helper PID, pane, and ACP session.

### PR #554: model lifecycle restart

Commit `9fff1dfa89` was titled:

> Fix agent model switching lifecycle

Its intended product behavior was:

- Settings selects the global cloud/local mode and configured model;
- `/model` remains a session-local override;
- `/model` choices are limited to the active cloud/local mode; and
- BYOM provider UI remains usable.

To enforce that separation, the implementation moved `acpModel` back into
`AgentSettingsSnapshot` and treated every configured cloud model change as a
master launch configuration change.

That classification is too broad. It handles cloud-to-cloud changes as if they
were provider environment changes.

## Existing hot-update support

The WTA implementation still contains the runtime model-switch path.

`tools/wta/src/app_events.rs` accepts:

```json
{
  "method": "agent_config_changed",
  "params": {
    "acp_model": "model-id",
    "target_agent_id": "copilot"
  }
}
```

It calls `apply_global_acp_model`, which updates sessions that:

- follow the global ACP agent;
- use the matching agent ID; and
- do not have a pane-local `/model` override.

`tools/wta/src/protocol/acp/client.rs` then forwards the change through
`set_session_model`.

The C++ header also still says that model changes are handled by
`_EmitAgentRuntimeConfigIfChanged`, although the current C++ implementation no
longer includes `acpModel` in `AgentRuntimeConfigSnapshot`. The comments and
the implementation are inconsistent.

## Required lifecycle boundaries

### Cloud model to cloud model

Example:

```text
gpt-5.6-sol -> claude-opus-4.8
```

Expected behavior:

```text
Settings reload
  -> agent_config_changed(acp_model, target_agent_id)
  -> helper filters by agent/follow mode/local override
  -> ACP setSessionModel
  -> agent status refresh
```

The following must remain unchanged:

- Terminal pane tree;
- split ratio;
- focus;
- WTA helper process;
- WTA master process;
- ACP session, when the agent supports live model switching;
- unrelated tabs and windows.

### Cloud model to local/BYOM

Cloud-to-local is not equivalent to cloud-to-cloud.

A local provider includes launch configuration:

- base URL;
- model ID;
- Credential Manager credential ID;
- whether an API key is required; and
- provider-specific environment or generated configuration.

Terminal currently passes this data to the WTA master through:

```text
WTA_CUSTOM_MODEL_BASE_URL
WTA_CUSTOM_MODEL_ID
WTA_CUSTOM_MODEL_CREDENTIAL_ID
WTA_CUSTOM_MODEL_API_KEY_REQUIRED
```

The master resolves the credential and adapts the environment/configuration
before spawning the agent CLI. A running cloud agent CLI cannot generally be
converted to this provider with `setSessionModel`.

Expected behavior:

```text
Settings reload
  -> trusted provider configuration update
  -> replace only affected agent CLI pool entry
  -> create/rebind ACP sessions for helpers using that entry
  -> helper remains in the existing pane and displays Connecting
  -> agent status refresh
```

The agent process and ACP sessions may change. The Terminal pane and helper
should not.

### Local/BYOM to cloud

The inverse transition must remove provider launch configuration and replace
the affected agent CLI. It must not leave provider environment variables or
generated provider configuration on a cloud process.

Again, the helper and Terminal pane should remain.

### Local/BYOM provider changes

Changing endpoint, credential, API contract, or selected local model should
advance a provider configuration generation and replace agent CLI instances
using the old generation.

Unrelated pool entries must remain running.

## Recommended architecture

### Preferred design: master-owned provider reconfiguration

Add a trusted provider-configuration update path between Terminal and the WTA
master.

1. Terminal sends provider metadata and a Credential Manager credential ID.
   It does not send the plaintext API key through helper-visible events.
2. Master resolves the credential.
3. Master advances a provider configuration generation.
4. Master evicts only agent pool entries that use the previous generation.
5. Master starts a replacement agent CLI with the new configuration.
6. Existing helpers remain connected and bind new ACP sessions.
7. Helpers update their existing panes from `Connecting` to `Connected`.

The agent pool key currently consists of:

```text
(execution source, authoritative agent ID, command line)
```

Provider identity or configuration generation must also participate in
selection/eviction so cloud and local instances cannot incorrectly share one
CLI merely because their command lines match.

### Transitional design: preserve panes across master restart

If dynamic master reconfiguration is too large for the first fix:

1. Restart SharedWta with the new trusted environment.
2. Keep the stable master pipe name.
3. Make existing helpers reconnect to the replacement master.
4. Preserve every helper process and Terminal pane.

This removes the layout corruption, but it still interrupts all sessions in
the process-wide master. It is therefore a transitional solution, not the
desired isolation boundary.

## Proposed implementation split

### Phase 1: restore cloud model hot switching

- Move `acpModel` back into `AgentRuntimeConfigSnapshot`.
- Remove `acpModel` from the identity/master-restart comparison.
- Emit `acp_model` and `target_agent_id` in
  `_EmitAgentRuntimeConfigIfChanged`.
- Preserve pane-local `/model` overrides.
- Add coverage proving that cloud-to-cloud Settings changes do not call
  `_TeardownAgentPane` or `SharedWta::Restart`.

An empty model means "agent default". ACP does not currently provide a portable
reset-to-default operation, so that transition needs explicit behavior rather
than silently doing nothing.

### Phase 2: isolate cloud/local process replacement

- Introduce master-owned provider configuration generations.
- Replace only affected agent CLI pool entries.
- Rebind helpers without closing their panes.
- Define session/history behavior for provider transitions.
- Verify that unrelated agents, tabs, and windows remain connected.

### Phase 3: remove pane reconstruction from model lifecycle

- Ensure no model or provider transition calls `Pane::Close` or
  `SplitPaneAtRoot`.
- Reserve pane construction and destruction for explicit UI lifecycle:
  opening, stashing, moving, or closing the assistant.

## Behavioral matrix

| Transition | Agent CLI | ACP session | Helper | Terminal pane | Shared master |
| --- | --- | --- | --- | --- | --- |
| Cloud A -> Cloud B | Preserve | Preserve when supported | Preserve | Preserve | Preserve |
| `/model` override | Preserve | Preserve | Preserve | Preserve | Preserve |
| Cloud -> local/BYOM | Replace affected CLI | Recreate/rebind | Preserve | Preserve | Preserve |
| Local/BYOM -> cloud | Replace affected CLI | Recreate/rebind | Preserve | Preserve | Preserve |
| Local provider config change | Replace affected CLI | Recreate/rebind | Preserve | Preserve | Preserve |
| Agent identity/command change | Replace affected CLI | Recreate/rebind | Prefer preserve | Preserve | Preserve |
| Explicit agent pane close | Release when unused | Close/drop | Exit | Close/stash as requested | Preserve while referenced |

## Acceptance criteria

1. Switching between two cloud models never changes Terminal pane geometry.
2. Cloud-to-cloud switching preserves helper and master PIDs.
3. Cloud-to-cloud switching preserves the live ACP session when supported.
4. Cloud/local transitions show connection progress inside the existing pane.
5. Cloud/local transitions do not call `Pane::Close` or `SplitPaneAtRoot`.
6. A provider change affects only agent CLI pool entries using that provider.
7. Other tabs and windows do not receive `transport_lost`.
8. Pane-local `/model` overrides are not overwritten by a global Settings
   update.
9. Credentials remain master-owned and are never sent through helper-visible
   events or logs.
10. Switching back to native cloud mode removes all local-provider environment
    and generated configuration from the replacement CLI.
