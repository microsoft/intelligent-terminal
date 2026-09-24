# ACP Agent Validation

Validation is complete only when the built package runs the newly built WTA
binary and the live logs prove the intended agent command and ACP behavior.

## Static Review

1. Search for exhaustive built-in agent lists and confirm the new ID appears
   everywhere required.
2. Confirm Rust and C++ ACP/delegate registries agree.
3. Confirm fixed C++ array sizes match their entries.
4. Confirm agent-pane launch and Settings model probe use the same ACP command.
5. Confirm telemetry accepts only the canonical ID, not arbitrary commands.
6. Confirm ADML prose, built-in count, and textbox hint are current.
7. Confirm documentation does not claim unsupported hooks/history features.
8. When session listing or resume is supported, confirm the canonical ID has a
   typed `CliSource` and survives helper/master wire round-trips.
9. When hooks are supported, confirm the packaged bundle, CLI filter,
   install/status/uninstall/upgrade paths, onboarding, Settings row, and
   canonical hook `CliSource` all include the agent.
10. Inspect `git diff --check` and the full diff for unrelated changes. Account
   for this repository's existing CRLF files before treating every reported
   line as newly introduced trailing whitespace.
11. Check host and WSL discovery independently when their native executables or
    arguments differ. An installed interactive CLI is not proof that a separately
    distributed ACP server is available. Keep one shared protocol implementation.
12. Check native permission-mode and usage registries, host/WSL catalog isolation,
    and first-run behavior for ACP-only providers with no delegate or hook support.
13. Check the current main branch again before publication; new provider telemetry
    buckets and release-checklist IDs may have appeared during a long integration.

## Automated Tests

Add or update tests for:

- profile lookup by ID, executable, extension, and full path;
- ACP launch command construction and adapter identification;
- ACP model flags versus delegate model flags;
- auth command generation and host-argument handling;
- resume/new-session metadata when supported;
- session source parsing, filtering, wire round-trips, labels, and exact resume
  dispatch when session management is supported;
- hook bundle resolution, install/status/uninstall/upgrade, ownership
  protection, partial-install repair, lifecycle event mapping, child-session
  filtering, and ACP-mode suppression when hooks are supported;
- Settings hook-status parsing and detected/not-installed visibility;
- direct Windows delegate command shape;
- PowerShell 7 and Windows PowerShell 5.1 delegate quoting;
- WSL delegate quoting and multiline prompts;
- invalid or empty delegate executable rejection;
- canonical identity versus interactive/ACP executable aliases;
- case-insensitive configured IDs through command resolution and profile gates;
- mixed-case hook CLI arguments through canonical payload and WSL source enrichment;
- unsafe and overlong provider conversation IDs at hook publication and CLI resume;
- empty mounted-workspace metadata with actual Windows/WSL hook cwd preservation;
- stale host-side WSL variables and conflicting per-file plugin ownership;
- shared-config cleanup ownership and explicit refusal when the native manager is absent;
- missing companion executables, earlier partial PATH entries and native WSL symlinks;
- source-specific master command reconstruction and Helper reconnect;
- observed Session MCP tool-name qualification, master-bound server identity,
  stale/foreign-name rejection, and retained final Terminal action confirmation;
- explicit rejection of unsupported built-in delegation without changing custom commands;
- policy filtering or settings serialization when those paths changed.

Run the WTA suite from the repository root:

```powershell
cargo test --target x86_64-pc-windows-msvc --manifest-path tools\wta\Cargo.toml
```

Do not treat a successful build as a substitute for tests; WTA test-only code
is not compiled by the C++ build.

## Build

If an output is locked, identify the executable paths first and stop only the
specific PIDs running the exact output being rebuilt. Do not terminate every WTA
or Terminal process by name.

```powershell
cargo build --target x86_64-pc-windows-msvc --manifest-path tools\wta\Cargo.toml
```

Always use the explicit target for a package-validation cycle because
CascadiaPackage prefers that output and can otherwise deploy a stale
`wta.exe`.

Build Terminal through the repository's razzle environment:

```powershell
cmd.exe /c "tools\razzle.cmd && bcz no_clean"
```

Use the Visual Studio/MSBuild version required by the current repository. If
`.slnx` is not recognized or the toolset mismatches, fix the selected Visual
Studio environment rather than changing project files.

Deploy `CascadiaPackage` using its generated recipe and the repository's current
deployment helper. Preserve application data and close only the selected package's
owned processes. A registration pointing at another layout requires an explicitly
approved transition, not an automatic destructive reinstall. Verify source,
recipe/staging, installed binary hashes, and the actual launched package before
trusting UI results.

## Live ACP Verification

1. Select the new built-in agent in Settings.
2. Open the agent pane and confirm the intended logo and display name in light,
   dark, and high-contrast modes.
3. Confirm the spawned WTA master command contains the exact intended ACP
   command and canonical `--agent-id`.
4. Confirm the actual agent process is the native ACP server or intended
   adapter, not the normal TUI or a stale binary.
5. In `wta-main_master.<UTC-date>.log` or fallback `wta-main_master.log`,
   and the current `wta-main_helper-<pid>.<UTC-date>.log` or fallback
   `wta-main_helper-<pid>.log`, verify:
   - ACP initialize succeeds;
   - the expected agent/version is reported;
   - `session/new` succeeds;
   - model discovery succeeds or is explicitly unsupported;
   - a prompt streams a response and reaches a terminal stop reason;
   - cancellation and pane close do not tear down unrelated sessions;
   - no unexpected error or panic is present.
6. Exercise model selection and reconnect. Verify the selected model reaches
   the ACP session through the protocol or supported server flag.
7. Exercise unauthenticated startup, the advertised login flow, and credential
   refresh. Verify logout returns the agent to the expected unauthenticated
   state.
8. If session management is supported, open `/sessions`, select a historical
   row from the new agent, and verify Enter chooses the intended CLI/ACP resume
   path rather than `UnknownCli`. Confirm the resumed tab starts with the stored
   session title and the helper log records the typed source.
   If ACP and ordinary CLI have separate stores, verify each with its own actual
   session identifier, including WSL routing. Do not retarget provider homes or
   copy credentials to manufacture cross-interface resume compatibility.

Also verify a fresh agent process can create a session using the established
authentication. During browser authorization, keep the ACP process and stdin alive
and let the browser complete the callback; do not actively connect to the OAuth
listener as a readiness probe.

Run the native command for each supported execution source. When a live runtime
is unavailable, distinguish that prerequisite from product behavior and state
which platform remains unverified. Do not weaken Windows builds/deterministic
coverage or silently substitute WSL for a selected Windows source.

Keep quota-consuming provider checks outside the publishable E2E suite and CI.
Use deterministic fixtures or zero-inference discovery/session checks there;
real model/tool acceptance must use the exact deployed product through normal
entry points and remain a distinct result.

Exercise a real Session MCP action, not only chat text. A provider's extra permission
dialog can reveal an unrecognized scoped tool-name format. Confirm that only the
current master-bound MCP invocation is recognized, the helper still presents the
normal action confirmation, and the actual side effect occurs in the owning
terminal/source rather than an agent-owned shell.

Packaged logs live under the app package's
`LocalCache\Local\IntelligentTerminal\logs\<package-version>` directory.
Relevant files include `terminal-agent-pane.log`,
`wta-main_master.<UTC-date>.log` or fallback `wta-main_master.log`,
`wta-main_helper-<pid>.<UTC-date>.log` or fallback
`wta-main_helper-<pid>.log`, `wta-probe.<UTC-date>.log` or fallback
`wta-probe.log`, and `wta-delegate.<UTC-date>.log` or fallback
`wta-delegate.log`.

## Delegate Verification

Run this section only when the agent is in `BuiltinDelegateAgents`.

1. Delegate a prompt from a Windows shell and, when supported, WSL.
2. Confirm `wta-delegate.<UTC-date>.log` or fallback `wta-delegate.log`
   shows the intended executable, model, prompt form, and shell path without
   leaking prompt contents or credentials.
3. Confirm the new tab opens an interactive agent TUI with the initial prompt.
4. Wait for the first task to finish and confirm the tab remains open and can
   accept another prompt.
5. Verify paths with spaces and multiline prompts.

If the delegate exits successfully and the tab disappears, the integration is
probably using a one-shot command. Find the CLI's interactive initial-prompt
form; do not suppress Terminal's normal close-on-success behavior as a
workaround.

## Session Hook Verification

Run this section only when the agent has a bundled session hook/plugin.

1. Install through onboarding or Settings, then confirm `wta hooks status
   --json` reports the agent and the packaged bundle version.
2. Restart the interactive CLI so its plugin loader sees the new files.
3. Run the normal interactive CLI in a regular Terminal pane, submit a prompt
   that executes a tool, and verify `/sessions` shows one row with the canonical
   source, cwd, and title.
4. Submit another prompt in the same session and verify it updates or rebinds
   the existing row rather than creating a duplicate.
5. Trigger permission/input, error, idle, and exit/delete paths that the CLI can
   produce; confirm WTA receives the expected notification, stop, error, and
   session-end events.
6. Start the agent through the ACP pane and verify the hook is suppressed; the
   ACP session must appear exactly once through normal helper/master tracking.
7. Remove hooks from Settings, including after temporarily removing the CLI
   from `PATH`, and confirm only managed files are deleted.
8. Inspect `wta-ensure-host.log` and the master/helper logs in the packaged
   versioned log directory for the complete native hook event path.

Verify hook payload field names and lifecycle meanings with real callbacks:
`Stop` can mean a partial background completion, not necessarily idle, and a
shared plugin directory can also serve a non-CLI frontend. Preserve source and
cwd through native publication, master state, saved layouts and resume. Validate
the published event, not merely a guard's zero exit code. Keep inference-based
callback verification local-only.

## Policy Verification

Test `AllowedAgents` with the canonical ID:

1. Unconfigured policy exposes the agent normally.
2. An allowlist containing the ID exposes it in every supported ACP/delegate
   selector.
3. An allowlist excluding the ID hides or blocks it consistently.
4. Match IDs case-insensitively if that is the existing policy contract.

Record the tested CLI/adapter version, ACP command, test count, build
configuration, package identity, and key log evidence in the PR description.
