# wtcli detached-session management

## Summary

Add two commands for inspecting and terminating shell panes that are still
running after their tab or window was detached:

```text
wtcli list-detached-sessions
wtcli kill-detached-session <shell-session-guid>
```

The commands operate on **live detached panes**, not on the saved snapshots
returned by `list-shell-sessions`.

## Terminology and identity

Three identifiers are involved:

| Field | Meaning |
| --- | --- |
| `SESSION_ID` | The live pane's connection session GUID; also exposed to the shell as `WT_SESSION`. Reported for diagnostics and for the pane-targeted commands. |
| `GROUP_ID` | The in-memory identifier for the detached tab. Multiple pane sessions may share one group. This is included in JSON output for diagnostics. |
| `SHELL_SESSION_ID` | The `shell_sessions.id` primary key in the durable-session SQLite database. All panes detached from the same saved tab share it. This is the input to `kill-detached-session`. |

The human-readable list is one row per pane:

```text
SESSION_ID                             PID     TAB          SHELL_SESSION_ID
84DD76D9-830F-4649-A77C-F649559CE96A  21908   tmux test    4367e91d-0706-4374-8edc-73bdf1a01f0d
AA52A683-4977-4D49-9704-F2CB4144B2BB  27776   tmux test    4367e91d-0706-4374-8edc-73bdf1a01f0d
```

JSON output:

```json
{
  "detached_sessions": [
    {
      "session_id": "84dd76d9-830f-4649-a77c-f649559ce96a",
      "pid": 21908,
      "tab_title": "tmux test",
      "group_id": "859e9c8c-fec9-4f92-b9b8-b50bd4ca55d8",
      "shell_session_id": "4367e91d-0706-4374-8edc-73bdf1a01f0d"
    }
  ]
}
```

`PID` is `0` for a connection type that does not expose a local root process.

## Command behavior

### `list-detached-sessions`

- Lists only sessions currently held by `ContentManager` with no attached
  `TermControl`. A tab that opted into keep-running but is still open has a
  window, so it is not detached and does not appear here — `list-shell-sessions`
  is where its `KEEP_RUNNING` flag shows up.
- Does not read `shell-sessions.db`; the database can contain historical
  snapshots that are no longer running.
- Produces an empty table/array when nothing is detached.
- Supports the existing global `--json` option.

### `list-shell-sessions`

Saved rows come from the database, which knows nothing about live state, so the
server annotates each one:

- `keep_running` — the session is detached with no window, **or** its tab is
  open and opted into keep-running. Both halves are needed: without the second,
  the flag would blink off the moment a session is restored.
- `opened` — some open tab is currently bound to this durable id.

The agent pane's `/shell-sessions` view reaches wta-master directly instead of
going through this server, so it computes the same two marks itself in
`tools/wta/src/protocol/acp/client.rs`. The definitions must stay in step.

### `kill-detached-session <shell-session-guid>`

- Takes the durable `SHELL_SESSION_ID`, so the command and the notification-area
  menu act on the same thing: a saved tab. Both close every pane in it.
- A tab that was opted into keep-running but never saved has no durable id; its
  group is keyed by the tab's stable id instead, and that key is accepted too so
  the tab stays reachable.
- The detached tab disappears from the notification-area menu.
- The command exits nonzero when the GUID is invalid or is not currently
  detached.
- Closing the final detached session allows an otherwise idle, windowless
  `WindowEmperor` to exit.

## Data flow

### Capturing the durable database id

`_intellterm.wta/shell_sessions/save` already returns:

```json
{
  "id": "<shell_sessions.id>",
  "revision": 1,
  "forked": false
}
```

`TerminalPage::_PersistShellSession` currently treats the response as a success
boolean and ignores its body. It will parse the response and pass the actual
returned `id` to `_DetachShellPanesForKeepRunning`.

Using the returned id is important:

- the first save creates a new record;
- a stale revision may fork into a new record;
- therefore the request's optional `id` is not necessarily the database row
  that was actually committed.

The detached tab group stores this `shell_session_id` beside its title and pane
membership.

### ContentManager projection

Add a projected `DetachedSessionInfo` value with:

- `SessionId`
- `GroupId`
- `TabTitle`
- `ShellSessionId`
- `Pid`

`ContentManager::DetachedSessions()` returns one value per detached pane.

`ContentManager::DiscardKeptGroup(groupId)` ends every session in a detached
tab. `WindowEmperor` resolves the caller's `SHELL_SESSION_ID` to that group by
scanning `DetachedSessions()` for a matching `ShellSessionId`, falling back to a
direct `KeptGroups()` key match for a tab that has no durable id yet.

`ContentManager::DiscardKeptSession(sessionId)` remains as the single-pane
primitive underneath, and reports whether a live detached session was found and
closed.

### Classic COM API

Append two methods to the end of `ITerminalProtocol`; existing vtable slots must
remain stable:

```idl
HRESULT ListDetachedSessions([out, retval] BSTR* json);
HRESULT KillDetachedSession([in] GUID shellSessionId);
```

`TerminalProtocolComServer` reads live data from the process-wide
`ContentManager`. It does not require a visible window, so both commands work
while the emperor is headless.

The returned COM JSON is an array. `wtcli` is responsible for wrapping it as
`detached_sessions` for the global `--json` output, following the existing
`list-shell-sessions` pattern.

## Error handling

| Condition | Result |
| --- | --- |
| No running emperor / COM server | Existing wtcli connection error |
| Empty detached-session set | Success with empty output |
| Invalid GUID text | wtcli diagnostic and exit code 1; no COM call |
| Well-formed but unknown/non-detached GUID | `HRESULT_FROM_WIN32(ERROR_NOT_FOUND)`, diagnostic, exit code 1 |
| Session exits between list and kill | Once every pane of the tab has exited the group is reaped, so the kill reports not found; this is normal race handling |
| PID unavailable | List row remains valid with `pid: 0` |

## Alternatives rejected

### Extend `list-shell-sessions --running`

Rejected because saved shell sessions are durable database records, while
detached sessions are live in-memory processes. Combining them would make
stale snapshots and running panes look like one resource type.

### Query `shell-sessions.db` directly

Rejected because the database does not prove liveness and cannot terminate the
in-memory connection.

### Route requests through `SendEvent`

Rejected because the event channel has no natural request/response contract,
typed errors, or stable command semantics. The append-only classic COM surface
is already the integration boundary for wtcli.

## Verification

1. Detach a single-pane tab:
   - list contains one row with the correct pane GUID, PID, tab title, and
     durable database id;
   - kill by `SHELL_SESSION_ID` terminates the shell and removes the row.
2. Detach a two-pane tab:
   - list contains two rows with different `SESSION_ID` values;
   - both rows share `TAB`, `GROUP_ID`, and `SHELL_SESSION_ID`;
   - killing that one `SHELL_SESSION_ID` closes both panes, removes the tray
     group, and permits headless exit — the same outcome as the tray's Close.
3. Race:
   - let the shell exit naturally, then kill its old GUID;
   - command returns not found without affecting another session.
4. Output:
   - verify stable human column order;
   - verify exact JSON field names and empty-array behavior.
5. Compatibility:
   - existing wtcli commands still activate against the append-only COM
     interface;
   - build the Debug package and exercise the commands against a deployed
     headless emperor.
