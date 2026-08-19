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
