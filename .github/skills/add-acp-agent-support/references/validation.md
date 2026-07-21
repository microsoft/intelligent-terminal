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
8. Inspect `git diff --check` and the full diff for unrelated changes. Account
   for this repository's existing CRLF files before treating every reported
   line as newly introduced trailing whitespace.

## Automated Tests

Add or update tests for:

- profile lookup by ID, executable, extension, and full path;
- ACP launch command construction and adapter identification;
- ACP model flags versus delegate model flags;
- auth command generation and host-argument handling;
- resume/new-session metadata when supported;
- direct Windows delegate command shape;
- PowerShell 7 and Windows PowerShell 5.1 delegate quoting;
- WSL delegate quoting and multiline prompts;
- invalid or empty delegate executable rejection;
- policy filtering or settings serialization when those paths changed.

Run the WTA suite from the repository root:

```powershell
cargo test --manifest-path tools\wta\Cargo.toml
```

Do not treat a successful build as a substitute for tests; WTA test-only code
is not compiled by the C++ build.

## Build

Resolve and stop only the specific live WTA process IDs before rebuilding:

```powershell
Get-Process wta -ErrorAction SilentlyContinue |
    ForEach-Object { Stop-Process -Id $_.Id -Force }
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

Deploy `CascadiaPackage` using F5 or its generated
`CascadiaPackage.build.appxrecipe`. If the first command-line deployment
reports `ManifestChanged`, retry once and require a successful clean reinstall.

## Live ACP Verification

1. Select the new built-in agent in Settings.
2. Open the agent pane and confirm the intended logo and display name in light,
   dark, and high-contrast modes.
3. Confirm the spawned WTA master command contains the exact intended ACP
   command and canonical `--agent-id`.
4. Confirm the actual agent process is the native ACP server or intended
   adapter, not the normal TUI or a stale binary.
5. In `wta-main_master.log` and the current
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

Packaged logs live under the app package's
`LocalCache\Local\IntelligentTerminal\logs\<package-version>` directory.
Relevant files include `terminal-agent-pane.log`, `wta-main_master.log`,
`wta-main_helper-<pid>.log`, `wta-probe.log`, and `wta-delegate.log`.

## Delegate Verification

Run this section only when the agent is in `BuiltinDelegateAgents`.

1. Delegate a prompt from a Windows shell and, when supported, WSL.
2. Confirm `wta-delegate.log` shows the intended executable, model, prompt
   form, and shell path without leaking prompt contents or credentials.
3. Confirm the new tab opens an interactive agent TUI with the initial prompt.
4. Wait for the first task to finish and confirm the tab remains open and can
   accept another prompt.
5. Verify paths with spaces and multiline prompts.

If the delegate exits successfully and the tab disappears, the integration is
probably using a one-shot command. Find the CLI's interactive initial-prompt
form; do not suppress Terminal's normal close-on-success behavior as a
workaround.

## Policy Verification

Test `AllowedAgents` with the canonical ID:

1. Unconfigured policy exposes the agent normally.
2. An allowlist containing the ID exposes it in every supported ACP/delegate
   selector.
3. An allowlist excluding the ID hides or blocks it consistently.
4. Match IDs case-insensitively if that is the existing policy contract.

Record the tested CLI/adapter version, ACP command, test count, build
configuration, package identity, and key log evidence in the PR description.
