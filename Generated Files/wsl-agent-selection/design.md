# Design: source-aware agent selection

## One provider identity, one execution source

Keep `acpAgent` and `delegateAgent` as canonical provider IDs. Add
`acpAgentSource` and `delegateAgentSource`, defaulting to `host`, with
`wsl:<registered-distribution>` for an explicit WSL source. Existing settings
therefore preserve Windows behavior without migration.

Example:

```json
{
  "acpAgent": "copilot",
  "acpAgentSource": "wsl:Ubuntu",
  "delegateAgent": "claude",
  "delegateAgentSource": "host"
}
```

Reuse `AgentPaneBackend` for conversion between provider/source fields and
qualified picker/cache identities. Do not invent a second parser or overload a
provider ID with location. Append the new WinRT properties to preserve ABI.
Reject malformed explicit sources instead of treating them as Windows.
Custom command editor saves explicitly select `host`.

## Reuse the existing Settings discovery owner

Extend the existing MainPage background availability path with one explicit
all-WSL probe when navigating to global Agent settings or the profile Base page
containing agent pickers. Merely constructing Settings or restoring navigation
during a settings reload does not probe. Coalesce navigation while a probe is
in flight. Publish an immutable snapshot to current live global/profile view
models on the UI thread; those VMs build entries and notify bindings. XAML
contains no discovery or source-encoding logic.

WTA owns enumeration and installed-agent detection, reusing its per-distribution
probe. Retain the existing single-distribution command for compatibility and add
an all-distribution mode with bounded concurrency. Settings explicitly adds
`--start-stopped-wsl`: this may start registered distributions to check native
executables, but never starts ACP/model probes or installs anything. The CLI's
default passive mode retains compatibility. Return per-source results/errors;
failed detection is not authoritative absence. Order distributions/providers
deterministically.

All three surfaces use the same result: global ACP, global delegation, and
profile ACP/delegation. Existing `AgentEntry` carries a qualified selection ID;
reuse the localized `agent + location` format. Global Windows entries can keep
their bare IDs for custom-entry compatibility while displaying their location.
Keep a selected unavailable source as an explicit unavailable entry. Discovery
must not rewrite persisted settings or accidentally select a different item
while an observable list rebuilds.

For a registered but stopped/failed source, saved native-profile provider/source
metadata also supplies unavailable selectable hints. This is reused evidence,
not a claim that the CLI is still installed. A successful explicit source probe
supersedes those hints. No separate persistent discovery database is needed.

## Runtime precedence and ownership

Resolve exactly once through the existing binding resolver:

```text
explicit tab selection
  > nonempty profile backend
  > global provider + global source
```

Reuse that result for both initial helper creation and settings reconciliation.
The source determines ACP launch location, not the owning terminal's profile.
Remove the obsolete same-shell restriction at the binding boundary, not
session-ownership or policy guards. Preserve exact provider/distro in helper
arguments, restore identity, process-pool keys, and session operations.

Include global source in change snapshots and request identity. A source-only
change rebinds affected global followers in place, without restarting the shared
master or retiring unrelated explicit overrides. Resolved helper/rebind metadata
always carries the exact source; master argv is only legacy fallback, not an
authorization table. Global-model inheritance requires the
same provider and source, not merely the same provider name.

Delegation resolves the same global/profile precedence and passes its explicit
source to WTA. Translate Windows cwd for WSL; reject cross-distro POSIX reuse and
resolve the target distribution's home when no trustworthy cwd is available.
Never silently fall back to a host process.

## WSL startup cost and generated profiles

Generate native WSL agent profiles through the existing dynamic agent-profile
generator, preserving stable provider-plus-distribution identities and the
separation between native profile customization and pane-managed ACP arguments.
Generation itself never starts a VM or probes Linux. Restore known provider/source
metadata from persisted native profiles on cold ordinary loads. Explicit Agent
settings discovery may start distributions and supplies fresh installed-agent
evidence. Preserve saved customizations; failed source probes retain previous
evidence, while successful absence supersedes stale saved hints.

Managed native agent commandlines are not interactive shells. Exclude them from
the shared WSL shell-integration snapshot so reconciliation cannot run a shell
probe or uninstall through an agent CLI. Explicit shell-command overrides keep
the existing shell-integration behavior.

`CascadiaSettings::DiscoverWslAgentsAsync()` performs the explicit bounded probe
off the UI thread, updates the native-profile cache, and returns the same JSON
snapshot to MainPage. Generation only reads that cache. New profiles materialize
on the next Save/ordinary reload; discovery must not force a global reload that
discards unsaved edits. There is no WSL timer, implicit refresh callback, or
startup/reload probe. Keep the existing host discovery cache unchanged.

Do not treat WSL like eager Windows ACP providers. A stopped non-default WSL
source is ineligible for automatic chat/history ACP startup. Running a distro,
including as a result of Settings discovery, grants no ACP permission. Require
the exact default provider/source or actual use of that exact pair before
automatic history initialization. Selecting Copilot/Ubuntu does not authorize
Claude/Ubuntu or Copilot/Debian. Put this decision before preparation, ACP
processes, and history probes; retain host eager behavior. Never shut down or
terminate a distribution to test this policy in the shared environment.

The tab-creation path is deliberately more conservative: automatically prewarm
a WSL helper only for the explicitly configured global WSL source. Other WSL
sources start on explicit pane open/resume, even if currently running. This
avoids adding per-tab WSL queries or trusting a stale running snapshot. Windows
prewarm is unchanged. The helper also refuses a stashed WSL startup without the
explicit authorization flag before any home/preflight/ACP command.

Persisted stashed non-default WSL conversations remain deferred layout data on
their owning tab, not a fake pane or helper. Preserve their session/source/view,
cwd and geometry during subsequent saves; explicit open materializes that same
record. Session persistence disabled omits the record. A changed effective
backend invalidates the old deferred owner; a native-model hot update does not.

Registered/running snapshots can race external distro changes; report real
per-source discovery failures without inventing atomic guarantees. Running-state
queries are discovery evidence, never automatic ACP authorization.

## Source-specific models

Extend `probe-models` with optional `--wsl-distro`. Keep the old host default.
Reuse existing source-aware preparation/startup/cleanup, passing a Linux cwd to
WSL `new_session`. Retry accounting must include source.

Use the existing model cache with a qualified key for WSL and its historical
bare-provider key for Windows. Capture the full key and probe generation before
background work. A result for Ubuntu must not populate Windows or Debian, and
an old probe must not overwrite a later selection. WSL native models must not
be replaced by host BYOK entries; host BYOK state remains saved for switching
back to Windows.

## ACP working-directory namespaces (#265)

Keep one source-aware cwd resolver for helper startup and delegation. Preserve
valid POSIX and same-distro WSL UNC values. For rooted Windows drives, ask the
selected distro's `wslpath` and verify the resulting directory exists there.
The mount root is configuration, not a string-format rule. Resolve relative
values against the actual distro home; use that home with a warning when a
Windows directory cannot be mapped. Reject foreign-distro UNC reuse. No cwd
translation is added to settings loading, profile generation, or discovery.

At the master ACP boundary, the explicit source is authoritative. Host built-in
agents keep the Windows namespace and legitimate reported cwd, including
System32. For opaque host custom wrappers, reuse only unambiguous cwd-namespace
consensus from an already-completed, same-instance history inventory. Mixed,
partial, unavailable or ambiguous evidence grants no namespace certainty.
Never parse launcher commands or infer a distro from a POSIX path. Without
distro identity, the opposite-namespace candidate is an explicit local fallback
(`/tmp` or Windows home), not an invented mapping of the user's project.

`session/new` has at most two distinct candidates under one absolute deadline,
including any master-side cwd resolution. Retry only a primary deterministic
cwd preflight rejection tied to the attempted directory or a cwd field; echoed
request data, authentication, unrelated file errors, and timeouts never permit
a second creation. Emit one completion telemetry result and register the cwd
that actually succeeded.

Resolve a real `session/load` once within its existing deadline and rollback
reserve; never replay the RPC with another cwd. Keep routing pre-registration,
MCP cancellation and replacement rollback unchanged. Orphan/already-loaded
rebinds preserve the actual qualified registry cwd and current title/timestamp,
not the new helper's cwd. An orphan fast path performs no cwd probe or ACP load.
The adaptation needs no dependency, second history cache, or new lifecycle
service. Unsupported WSL-custom-provider editing remains outside this feature.

## Existing WSL installation behavior

The user clarified: preserve installation assistance that already exists for WSL,
but do not introduce new WSL installers or bootstrap flows. Existing Claude/Codex
ACP command construction uses `npx -y` adapters for both execution sources, so
an explicit existing launch can already obtain its adapter. Discovery and
profile generation never install anything. Preserve existing source-aware
preparation behavior, subject to exact provider/source authorization, rather than
silently converting previously supported launches to installed-only behavior.
Antigravity already has managed Linux ACP preparation; this change reuses that
existing lifecycle and introduces no new provider installer.

## Deliberate non-goals

No new MVVM framework, discovery daemon, transport, dependency, generic plugin
layer, WSL hook installer, cross-profile native-agent customization, automatic
credential copying, or broad refactoring. Extend current seams and test the
decisions and observable effects.

## Delivery

Build the owned worktree's exact Windows-target WTA and native dependency graph.
Use temporary uncommitted package/window/COM identity substitutions per
`doc\dev-worktree-package.md`; extract the resulting package to an immutable
private layout and register that exact manifest. Preserve regular Dev/Store
settings and production-installed hooks. Record package identity, binary hashes,
actual interaction proof, review snapshot/dispositions, and limitations.
