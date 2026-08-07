---
name: add-acp-agent-support
description: 'Add first-class support for an ACP-compatible agent CLI to Intelligent Terminal. Use when integrating a new built-in AI agent, ACP server command, authentication flow, model selection, interactive delegation, session hooks, onboarding, Settings, branding, GPO policy, documentation, tests, build, deployment, or live ACP and hook log verification.'
---

# Add ACP Agent Support

Integrate a new built-in agent across WTA, Terminal UI, settings, policy,
telemetry, documentation, and validation without leaving agent-specific
behavior inconsistent between layers.

## When to Use This Skill

- Add a native ACP-speaking agent CLI to Intelligent Terminal.
- Add an ACP adapter for a CLI that does not speak ACP itself.
- Promote a custom ACP command into a first-class built-in agent.
- Add or repair ACP authentication, model probing, delegate, icon, or policy
  support for an existing agent.
- Add or repair session-tracking hooks, hook onboarding, Settings status, or
  hook runtime behavior for a built-in agent.

## Prerequisites

- Work on a feature branch, never directly on `main` or `master`.
- Read the repository and area-specific instructions before editing, including
  `tools/wta/AGENTS.md` and the Rust instruction files for WTA changes.
- Obtain the agent's official CLI documentation and install a pinned or known
  version for live verification.
- Confirm that the CLI either exposes a long-running ACP server over stdio or
  has a maintained ACP adapter. A normal interactive TUI is not an ACP server.

## Workflow

Track the work with a TODO list because the Rust and C++ registries, UI,
policy, tests, and live verification must stay synchronized.

1. **Define the agent contract.** Complete the capability matrix in
   [integration-map.md](./references/integration-map.md): executable, ACP
   command, native-vs-adapter ownership, auth, model selection, interactive
   delegate syntax, session resume, installation, and official branding.
2. **Prove ACP behavior before editing.** Run the exact ACP command and verify
   stdio initialization, session creation, model discovery, prompt streaming,
   cancellation, and shutdown. Record the tested CLI/adapter version.
3. **Inventory current integration surfaces.** Search for every existing
   built-in agent ID and follow the current patterns. Do not rely on old line
   numbers or assume the Rust registry is the only source of truth.
4. **Implement the WTA profile and command behavior.** Add the profile,
   ACP launch command, auth flow, model behavior, install guidance, and
   resume metadata. Wire the canonical ID through the session subsystem when
   session listing or resume is supported. Add delegate support only when the
   agent has a true interactive initial-prompt invocation.
5. **Wire Terminal surfaces.** Keep C++ built-in registries, ACP command
   resolution, settings/model probing, telemetry sanitization, branding, and
   policy-facing lists consistent. Use the detailed file map in
   [integration-map.md](./references/integration-map.md).
6. **Decide and implement session hooks.** Treat hooks as a separate capability:
   verify the CLI's official hook/plugin API, map lifecycle events to WTA,
   suppress hook emission from the ACP process to avoid duplicate sessions,
   and wire safe install/status/uninstall/upgrade behavior through onboarding
   and Settings. If the CLI has no suitable hook API, document session tracking
   as unsupported instead of inferring it from ACP support.
7. **Add focused tests.** Cover profile lookup, ACP command construction,
   identification, session source round-trips and resume dispatch, auth command
   generation, hook lifecycle and ownership behavior when implemented, and
   every delegate shell path that applies: direct Windows, PowerShell 7,
   Windows PowerShell 5.1, and WSL.
8. **Update user and administrator documentation.** Document support,
   installation/auth requirements, limitations, delegate behavior, and the
   `AllowedAgents` identifier. Do not advertise hooks or history integration
   unless they were implemented and tested.
9. **Validate end to end.** Follow
   [validation.md](./references/validation.md), including the WTA test suite,
   explicit-target WTA build, Terminal build, package deployment, live ACP
   prompt, auth path, model selection, hook-backed session tracking when
   supported, delegate behavior, GPO filtering, and log inspection.
10. **Prepare the PR.** Keep the diff scoped, cite the tested agent version and
    ACP command, describe native-vs-adapter and hook support, list unsupported
    features, and link the tracking issue with a closing keyword when
    appropriate.

## Decision Rules

| Capability | Required decision |
|------------|-------------------|
| ACP launch | Prefer the CLI's native stdio mode. Use a maintained adapter only when native ACP is unavailable; pin the adapter when unbounded updates could break startup. |
| Authentication | Use `InProtocol` only when ACP advertises and completes authentication. Use `External` when a separate CLI/provider login is required, then verify the running ACP process can refresh credentials. |
| Models | Distinguish flags accepted by the ACP server process from flags accepted by the interactive delegate CLI. Prefer ACP model APIs when the server supports them. |
| Delegation | Use an interactive TUI invocation with an initial prompt. If the only interface is one-shot, omit first-class delegate support or explicitly ask the user to accept an auto-closing tab. |
| Resume | Configure resume/new-session metadata only after proving the exact CLI syntax and identifier semantics. Also add the agent to the session source type and every conversion boundary; profile metadata alone does not make a session resumable. |
| Hooks/history | Treat these as separate integrations. ACP compatibility alone does not imply shell hooks or historical session support. Use only the CLI's documented hook/plugin API, define ownership-safe install semantics, and suppress duplicate tracking from ACP-mode processes. |

## Gotchas

- **Never substitute a TUI or one-shot command for the ACP server command.**
  The agent pane requires a long-running JSON-RPC/ACP stdio process.
- **Never use a one-shot delegate subcommand when an interactive prompt flag
  exists.** A successful one-shot process exits and Terminal closes its tab.
- **Preserve the CLI's argument grammar.** Top-level flags, subcommands, model
  flags, and prompt arguments may require a specific order.
- **Test every quoting layer.** Multiline prompts and paths with spaces pass
  through different escaping in direct Windows, pwsh, Windows PowerShell, and
  WSL launches.
- **Keep both registries synchronized.** Update Rust execution metadata and
  C++ discoverability/GPO arrays together, including compile-time array sizes.
- **Do not stop at `AgentProfile` for session support.** Add the canonical agent
  to `CliSource`, parsing/filtering, wire conversions, labels, resume command
  synthesis, and ACP/WSL session discovery where supported. Otherwise
  `session/list` rows can appear as `Unknown("custom")` and Enter fails with
  "source agent is unknown to this build" even though the agent profile has a
  valid resume flag.
- **Do not run session hooks inside the shared ACP process.** ACP sessions are
  already tracked by the helper/master path; hook emission there creates
  duplicate session rows. Gate the plugin using a reliable ACP-mode signal and
  verify normal interactive sessions still emit.
- **Never overwrite user-owned hook files.** Use a managed-file marker or
  manifest, commit ownership metadata last, preserve it until uninstall
  cleanup succeeds, and make partial installs repairable.
- **Keep the hook lifecycle complete.** Adding a runtime script is insufficient:
  update install, status, uninstall, startup auto-upgrade, onboarding, Settings,
  packaged bundle contents, tests, documentation, and diagnostics together.
- **Separate ACP and delegate model flags.** Passing a TUI-only model flag to
  the ACP server can prevent startup even when delegation works.
- **Treat agent IDs as telemetry-sensitive.** Add only the canonical built-in
  ID to the allowlist; continue bucketing custom commands and paths as
  `custom`.
- **Use official, license-compatible branding.** Make header artwork respond
  to light, dark, and high-contrast themes; do not silently reuse another
  agent's logo.
- **Build the explicit Cargo target before packaging.** CascadiaPackage prefers
  `target/x86_64-pc-windows-msvc/.../wta.exe`; a stale binary there can shadow
  a fresh host-target build.
- **Update policy prose as well as runtime filtering.** `AllowedAgents` is
  generic, but its ADML identifier list and built-in count can drift.

## References

- [Integration surface map](./references/integration-map.md)
- [Validation and live verification](./references/validation.md)
