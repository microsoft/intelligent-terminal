# Rich Tab providers

Rich Tab providers publish structured tab metadata through `wtcli`. A provider
manifest selects either one-shot or persistent hosting.

## Persistent hosting

Persistent providers use manifest schema version 2 and protocol version 2:

```json
{
  "schemaVersion": 2,
  "protocol": {
    "minVersion": 2,
    "maxVersion": 2
  },
  "hosting": {
    "kind": "persistent",
    "controlProtocolVersion": 1
  }
}
```

The Terminal broker starts one provider process for each provider and Terminal
session. The process remains in a constrained Job Object and receives control
messages through stdin. Each message uses Content-Length framing:

```text
Content-Length: <UTF-8 byte count>\r\n
\r\n
<JSON payload>
```

The payload's `kind` is `start`, `refresh`, `lease`, or `stop`.
`start` and `refresh` include a protocol-v2 `request` and up to two single-use
publish `grants`. A `lease` message supplies replacement grants for the current
request. A `stop` message asks the provider to exit; the broker closes stdin and
terminates the provider Job Object if it does not stop within two seconds.

Each grant contains:

```json
{
  "requestId": "broker request ID",
  "lease": "opaque single-use token",
  "expiresInMilliseconds": 90000
}
```

To publish, the provider starts `wtcli provider publish --stdin` as a child
process, sets `WT_RICH_TAB_LEASE` to one received lease only for that child, and
writes a protocol-v2 snapshot to its stdin. The persistent root process does not
receive a lease through its environment. A lease is consumed by every publish
attempt, including malformed snapshots.

Providers must discard grants after a new `start` or `refresh` request, after
their advertised expiration, and when stdin closes. Context changes and
provider reloads restart the process and invalidate all grants from the old
instance.

Register mutable provider code for development with:

```powershell
wtcli provider register --dev C:\path\to\provider.json
wtcli provider enable <provider-id> --accept-code-execution
```

`wtcli provider test` supports one-shot providers. Persistent providers require
a running Terminal broker because their grants are broker-bound.

## Diagnostics

Rich Tab diagnostics are written as JSON Lines files in the current package
version's Intelligent Terminal log directory:

```text
rich-tabs-<pid>-<process-epoch>.jsonl
```

Each Terminal process owns its file, so normal, elevated, packaged, and
unpackaged instances never rotate one another's active log. Each file is capped
at 4 MiB with one 4 MiB backup. Closed runs older than seven days are eligible
for deletion, and closed-run housekeeping targets a 32 MiB total.

The default release log records refresh summaries, one-shot terminal outcomes,
state changes, and failures. Set `WTA_LOG=debug` to include individual
dispatches and successful publishes.
Repeated warnings from provider-controlled input are limited to three matching
records per minute; a later record or orderly shutdown reports
`suppressed_count`.

The principal event chain is:

```text
attachment_state
  -> context_state
  -> catalog_state
  -> refresh_plan
  -> request_state / instance_state
  -> publish_result
  -> composition_state
  -> page_handoff
  -> header_metadata_state
```

`refresh_plan` bridges a context or catalog change and provider execution.
`catalog_count`, `effective_count`, `filter_matched_count`,
`activation_supported_count`, `started_count`, and `coalesced_count` identify
why a refresh did or did not start work. `session_incarnation` prevents a
recreated session from being confused with an older session that reused the
same ID, while `catalog_revision` associates refreshes with the effective
catalog snapshot.

For example, to trace one anonymous Terminal session:

```powershell
$log = Get-ChildItem "$env:LOCALAPPDATA\IntelligentTerminal\logs" `
    -Recurse -Filter "rich-tabs-*.jsonl" |
    Sort-Object LastWriteTime -Descending |
    Select-Object -First 1

Get-Content $log | ConvertFrom-Json |
    Where-Object session -eq "s1" |
    Format-Table ts, event, state, reason, provider, request
```

The files intentionally omit working-directory paths, provider IDs, tab or
session GUIDs, publish leases, snapshots, field values, presentation text,
tooltips, accessibility text, stdout, stderr, environment values, and external
exception messages. Session, provider, and tab identities are process-local
aliases such as `s1`, `p1`, and `t1`.

`context_state` reports only the final context accepted by Terminal. It does
not distinguish a shell that omitted a CWD sequence from a sequence Terminal
rejected. `header_metadata_state` reports XAML logical state; it does not prove
pixel visibility, lack of occlusion, or that the user saw the metadata. Sink
shutdown is bounded and best effort: process termination, power loss, or a
blocked filesystem can omit queued records or the final shutdown marker.
