# Installed ACP agents from bundled registry metadata

## User experience and boundaries

If a supported ACP agent is already installed on the Windows host, it appears by name in the **existing Settings → Agents → Agent dropdown**, subject to policy. Select it there; no registry browser, preset picker or catalog setup step is required. The original **Add Custom / + Add New…** command-entry flow remains unchanged for commands outside automatic discovery.

Discovery uses bundled metadata, commands and packaged icons, derived from [`agentclientprotocol/registry@2a27ff18b5816f47d7ec538e22943ff194e05131`][registry]. It does not fetch a catalog or download/install a provider. There is no remote feed, default-catalog download, automatic update or new runtime registry-refresh machinery. “Offline” describes discovery and bundled assets, not an assurance that an agent's own service works without a network.

**Source-verified is not runtime-verified.** The local catalog has 30 discovery candidates, not 30 proven working installations. A candidate is shown only when its required local executable/shim is found. No provider was installed, launched, authenticated or resumed for this feature. Registry membership and executable presence do not certify ACP negotiation, credentials, subscription, model availability or provider compatibility. See the pull request for candidate-bound build/test evidence; provider-runtime certification is separate from local discovery.

The six existing built-ins—Copilot, Claude, Codex, Gemini, OpenCode and Antigravity—keep their existing identities, discovery, launch and authentication behavior. This feature adds no provider installation/authentication flow, hooks, native profiles, native-history scanner or guessed resume arguments. Linux distribution coverage below is research, not automatic WSL discovery or Windows-to-WSL command rewriting.

## Installed detection, identity and policy

The source of the local command list is [`AcpAgentCatalog.h`](../../src/cascadia/inc/AcpAgentCatalog.h). `Executable()` uses `SearchPathW` with explicitly configured PATH entries to find the listed agent's `.exe` or `.cmd` and rejects directories. Empty PATH components and the implicit current/application-directory search are excluded: a workspace file is not an installed agent. `Command()` quotes the resolved path and appends the bundled ACP arguments. It does not run `--version`, an agent, authentication, a hook probe or a package manager during discovery. Finding `npx`, `uvx`, Node or Python alone is **not** evidence that an agent is installed. Amp additionally requires `amp`; pi ACP additionally requires `pi`, besides their adapter executables.

Detection runs **once at application startup, shared by the process**, and **once on each navigation into Settings → Agents**. The SettingsModel DLL owns the in-memory snapshot, shared across settings clones, tabs and windows in that process. Opening another settings page, opening/scrolling the dropdown, typing, property evaluation and WTA's existing five-second custom-command configuration polling do not scan installed catalog executables. There is no discovery timer, watcher or new background worker. Installing or uninstalling a provider after startup does not change this snapshot until the next Agents-page visit (or application restart). Policy checks still apply immediately; a cached path does not guarantee that a later process launch will succeed if the executable has been removed.

Registry-derived entries use stable IDs `custom:acp-registry:<id>` (for example, `custom:acp-registry:bob`). The extra namespace separator avoids taking over executable-derived handwritten IDs such as `custom:acp-registry-bob`. They resolve to an installed command from the bundled metadata, not to an additional user-maintained named-command map or a package-download command. Original handwritten `acpCustomCommand` / `acpCustomCommands` behavior remains available; this feature does not require a new settings schema or custom-ID migration.

An unavailable explicit catalog selection keeps its ID but resolves to an empty command. It is not treated as “no selection”: the production resolver does not auto-detect a different provider, reuse an unrelated custom command or execute the logical ID. The shared-pane path rejects a selection with no launch command before acquiring a master/helper. Normal absent-selection and policy-allowed built-in fallback behavior is unchanged.

IDs from a newer catalog remain in this reserved namespace even if this build has no metadata for them. They keep the saved selection and model, never use legacy custom commands, and never reach executable model probes.

These are **custom agents**, not new built-in `AllowedAgents` values. [`AgentPolicy.h`](../../src/cascadia/inc/AgentPolicy.h) defines `AllowCustomAgents`: `0` blocks custom agents; `1` or an absent policy allows them. `AllowedAgents` is the built-in allowlist and does not individually allowlist these catalog IDs. Blocking custom agents hides these choices and prevents their effective selection/discovery; the handwritten custom flow is governed by the same existing policy. A found executable is trusted local executable configuration, not a sandbox.

Source relationships (explanatory evidence, **not runtime proof**):

* [`AppLogic.cpp`](../../src/cascadia/TerminalApp/AppLogic.cpp) initializes the snapshot on initial settings load, including the default-settings fallback, not on subsequent reloads or window creation.
* [`MainPage.cpp`](../../src/cascadia/TerminalSettingsEditor/MainPage.cpp) refreshes that snapshot only when entering Agents from a different page. It captures the actual current frame page before navigation: settings reload/reset while Agents remains open can reconstruct the view-model without another discovery pass. [`AIAgentsViewModel.cpp`](../../src/cascadia/TerminalSettingsEditor/AIAgentsViewModel.cpp) only appends snapshot IDs to the existing dropdown, with the original custom-entry flow.
* [`GlobalAppSettings.cpp`](../../src/cascadia/TerminalSettingsModel/GlobalAppSettings.cpp) reads the selected command from this process snapshot and applies current custom-agent policy. These property getters never perform executable lookup.
* [`CustomAcpAgentCommands.h`](../../src/cascadia/inc/CustomAcpAgentCommands.h) and [`wtcli get-settings --custom-acp-commands`](../../src/tools/wtcli/main.cpp) retain their original handwritten-command projection and policy behavior. Discovered dropdown choices are **not** added to that inventory, so discovery does not initialize unselected providers or populate the history warm-up pool. The selected pane uses the existing trusted launch-command path. No existing configuration-poll or startup-retry timer was changed.
* `IconPath()` uses packaged artwork; Copilot Language Server reuses existing Copilot artwork. Other registry icons and masks are bundled under [`AgentIcons/Registry`](../../src/cascadia/CascadiaPackage/AgentIcons/Registry), with [provenance/notices](../../src/cascadia/CascadiaPackage/AgentIcons/Registry/NOTICE.txt). No runtime icon download is needed; theme/accessibility rendering remains runtime-unverified here.

### Deliberate omissions

* `auggie`, `factory-droid`, `fast-agent` and `muse-code` require distribution-specific environment setup. They remain manual through the existing custom-command flow; automatic discovery must not silently drop their required environment variables.
* Kiro has no Windows launch in the pinned snapshot, so no Windows command is invented.
* The four quarantined entries (`agoragentic-acp`, `deepagents`, `minion-code`, `vtcode`) are not automatic candidates.
* The six built-in counterparts are not duplicated as new registry choices.
* The `kimi-code` and `kimi` distributions both expose `kimi acp`; they coalesce into one installed choice, `custom:acp-registry:kimi-code`. This does **not** merge their native history, versions or session identities.

### ACP is not native interactive resume

An ACP adapter/server command is not automatically a native interactive CLI command. This feature generates no new native profiles or native resume templates. `session/load` is negotiated ACP behavior using an ACP session ID; native resume may instead require a different ID, path, workspace scope or picker. Amp's `S-…` ACP IDs and `T-…` native thread IDs are a concrete counterexample to assuming equality. Existing custom ACP discovery/load and the product session-origin filter are not expanded into native resume support. Keep the full [native session research and source citations](acp-registry-native-sessions.md) as evidence, not as profile-configuration instructions.

## Complete pinned launch inventory

All 46 stable manifests are retained below as research, not 46 dropdown choices. Each ID identifies its pinned `<id>/agent.json` [source manifest][registry]. The research column records the distribution launch, **not a command the product downloads or executes for discovery**. Here `npx P …` abbreviates `npx --yes P …`; archive paths are relative to extraction and are not installed Windows paths. `P` means package distribution, not verified cross-platform support. The local-discovery column is the current header's actual executable and arguments; it requires a found `.exe`/`.cmd`, and is still runtime-unverified.

For npm-derived executable names, linked `package.json` files are exact-version **provider-published `bin` metadata** at `https://unpkg.com/<exact package spec>/package.json`, rather than guesses from package names. These are research citations, not runtime dependencies. The registry pin identifies the metadata source; discovery does not check or enforce the user's installed version. Archive-derived names come from the corresponding pinned manifest.

Binary availability: `W` = Windows x64, `WA` = Windows ARM64; `L` = Linux x64,
`LA` = Linux ARM64; `M` = corresponding musl builds. Linux targets may work in
a matching WSL distro; no WSL runtime was tested. Package rows do not imply
availability on every CPU/OS.

| Registry ID | Version | Pinned distribution launch / required environment (research only) | Distribution availability | Local discovery command / disposition |
|---|---|---|---|---|
| agoragentic-acp | 1.3.0 | `npx agoragentic-mcp@1.3.0 --acp` | P | Omitted: quarantined |
| amp-acp | 0.13.0 | archive `amp-acp` | W,L,LA | `amp-acp`; also requires `amp` |
| antigravity-acp | 1.5.2 | Windows `agy_acp_server.exe`; Linux `agy_acp_server.par --uid=` | W,WA,L,LA | Existing Antigravity built-in unchanged |
| auggie | 0.37.0 | `npx @augmentcode/auggie@0.37.0 --acp`; `AUGMENT_DISABLE_AUTO_UPDATE=1` | P | Manual env |
| autohand | 0.9.12 | archive `autohand --acp` | W,L,LA,M | `autohand --acp` |
| bob | 2.1.0 | `npx @ibm/bob@2.1.0 acp` | P | [`bob`](https://unpkg.com/@ibm/bob@2.1.0/package.json) `acp` |
| claude-acp | 0.110.0 | `npx @agentclientprotocol/claude-agent-acp@0.110.0` | P; L,LA,M binary alternative | Existing Claude built-in unchanged |
| cline | 3.0.75 | `npx cline@3.0.75 --acp` | P | [`cline`](https://unpkg.com/cline@3.0.75/package.json) `--acp` |
| codebuddy-code | 2.185.0 | `npx @tencent-ai/codebuddy-code@2.185.0 --acp` | P | [`codebuddy-code`](https://unpkg.com/@tencent-ai/codebuddy-code@2.185.0/package.json) `--acp` |
| codex-acp | 2.10.0 | `npx @agentclientprotocol/codex-acp@2.10.0` | P | Existing Codex built-in unchanged |
| cortex-code | 1.0.73 | archive versioned `cortex[.exe] acp serve` | W,WA,L,LA | `cortex acp serve` |
| corust-agent | 0.6.0 | archive `corust-agent-acp` | W,L | `corust-agent-acp` |
| crow-cli | 0.1.49 | archive `crow-cli acp` | W,L,LA | `crow-cli acp` |
| cursor | 2026.11.22 | archive `dist-package/cursor-agent[.cmd] acp` | W,WA,L,LA | `cursor-agent acp` |
| deepagents | 0.1.7 | `npx deepagents-acp@0.1.7` | P | Omitted: quarantined |
| devin | 3000.14.30821 | archive `bin/devin[.exe] acp` | W,WA,L,LA | `devin acp` |
| dimcode | 0.5.32 | `npx dimcode@0.5.32 acp` | P | [`dimcode`](https://unpkg.com/dimcode@0.5.32/package.json) `acp` |
| dirac | 0.6.1 | `npx dirac-cli@0.6.1 --acp` | P | [`dirac`](https://unpkg.com/dirac-cli@0.6.1/package.json) `--acp` |
| factory-droid | 1.18.0 | `npx droid@1.18.0 exec --output-format acp-daemon`; `DROID_DISABLE_AUTO_UPDATE=true`, `FACTORY_DROID_AUTO_UPDATE_ENABLED=false` | P | Manual env |
| fast-agent | 0.10.65 | `uvx fast-agent-acp==0.10.65 -x`; `FAST_AGENT_MODEL=codexplan` | P | Manual env |
| gemini | 0.70.0 | `npx @google/gemini-cli@0.70.0 --acp` | P | Existing Gemini built-in unchanged |
| github-copilot-cli | 1.0.114 | `npx @github/copilot@1.0.114 --acp` | P | Existing Copilot built-in unchanged |
| github-copilot | 1.574.0 | `npx @github/copilot-language-server@1.574.0 --acp` | P | [`copilot-language-server`](https://unpkg.com/@github/copilot-language-server@1.574.0/package.json) `--acp`; distinct from Copilot CLI |
| glm-acp-agent | 1.17.1 | `npx glm-acp-agent@1.17.1` | P | [`glm-acp-agent`](https://unpkg.com/glm-acp-agent@1.17.1/package.json) |
| goose | 1.60.2 | archive `goose[.exe] acp` (Windows nested in `goose-package`) | W,L,LA,M | `goose acp` |
| grok-build | 1.0.93 | `npx @xai-official/grok@1.0.93 agent stdio` | P | [`grok`](https://unpkg.com/@xai-official/grok@1.0.93/package.json) `agent stdio` |
| harn | 0.10.202 | archive `harn serve acp` | W,L,LA | `harn serve acp` |
| junie | 4100.11.0 | archive `junie/junie.exe --acp=true`; Linux `junie-app/bin/junie --acp=true` | W,WA,L,LA | `junie --acp=true` |
| kilo | 7.8.32 | `npx @kilocode/cli@7.8.32 acp` | P; W,L,LA,M binary alternative | [`kilo`](https://unpkg.com/@kilocode/cli@7.8.32/package.json) `acp` |
| kimchi | 1.39.1 | archive `bin/kimchi --mode acp` | W,L,LA | `kimchi --mode acp` |
| kimi-code | 2.7.3 | `npx @moonshot-ai/kimi-code@2.7.3 acp` | P | [`kimi`](https://unpkg.com/@moonshot-ai/kimi-code@2.7.3/package.json) `acp`; one shared installed choice |
| kimi | 1.52.0 | archive `kimi[.exe] acp` | W,WA,L,LA | Same `kimi acp` choice as `kimi-code`, not a second entry |
| kiro | 3.0.0 | archive `kirocli/bin/kiro-cli acp --agent-engine=v3` | L,LA,M; no Windows binary | Omitted: no pinned Windows launch |
| minimax-code | 0.7.9 | `npx @minimax-ai/code-acp@0.7.9 acp` | P | [`code-acp`](https://unpkg.com/@minimax-ai/code-acp@0.7.9/package.json) `acp` |
| minion-code | 0.1.44 | `uvx minion-code@0.1.44 acp` | P | Omitted: quarantined |
| mistral-vibe | 3.1.4 | archive `vibe-acp[.exe]` | W,L,LA | `vibe-acp` |
| muse-code | 1.4.24 | archive `muse[.exe] acp`; `MUSE_EXPERIMENTAL_ACP=on` | W,WA,L,LA | Manual env |
| nova | 1.1.68 | `npx @compass-ai/nova@1.1.68 acp` | P | [`nova`](https://unpkg.com/@compass-ai/nova@1.1.68/package.json) `acp` |
| opencode | 2.4.1 | archive `opencode[.exe] acp` | W,WA,L,LA,M | Existing OpenCode built-in unchanged |
| pi-acp | 0.0.35 | `npx pi-acp@0.0.35` | P | [`pi-acp`](https://unpkg.com/pi-acp@0.0.35/package.json); also requires `pi` |
| poolside | 1.0.16 | archive `pool-<platform>-<arch>[.exe] acp` | W,WA,L,LA | installed [`pool acp`](https://github.com/poolsideai/pool/blob/fcabc59ff67810ecec5429ec9d45fa1dda6c1136/README.md#run-as-an-acp-server-pool-acp); fallback `pool-windows-amd64 acp` (x64) / `pool-windows-arm64 acp` (ARM64), same choice |
| qoder | 1.1.96 | `npx @qoder-ai/qodercli@1.1.96 --acp` | P | [`qodercli`](https://unpkg.com/@qoder-ai/qodercli@1.1.96/package.json) `--acp` |
| qwen-code | 0.28.4 | `npx @qwen-code/qwen-code@0.28.4 --acp --experimental-skills` | P | [`qwen`](https://unpkg.com/@qwen-code/qwen-code@0.28.4/package.json) `--acp --experimental-skills` |
| sigit | 1.7.10 | `npx @smbcloud/sigit@1.7.10` (manifest omits `--acp`) | P; W,WA,L,LA binary alternative | [`sigit`](https://unpkg.com/@smbcloud/sigit@1.7.10/package.json) `--acp`; explicit README override below |
| stakpak | 0.3.88 | archive `stakpak[.exe] acp` | W,L,LA,M | `stakpak acp` |
| vtcode | 0.96.14 | archive `vtcode acp`; `VT_ACP_ENABLED=1`, `VT_ACP_ZED_ENABLED=1` | W,L,M | Omitted: quarantined |

**siGit manifest/README drift is intentional and visible:** the pinned [manifest](https://github.com/agentclientprotocol/registry/blob/2a27ff18b5816f47d7ec538e22943ff194e05131/sigit/agent.json) omits `--acp`. The [exact-version 1.7.10 README](https://unpkg.com/@smbcloud/sigit@1.7.10/README.md) says bare `sigit` starts the local terminal UI and every ACP client must pass `--acp`. The bundled local command therefore explicitly overrides the manifest with `sigit --acp`. This is source-backed correction, not a claim of runtime compatibility.

The machine-readable research snapshot is `Generated Files/acp-registry-delivery/research/matrix.json` (private delivery artifact, not a runtime dependency). It retains every distribution object, exact archive URLs/hashes, full paths, args/env and platform keys at the registry revision above. Its older `preset_command`, `setup` and `profile` fields record the superseded preset investigation, **not current product UX or acceptance**; consult the current header and dispositions here for installed discovery. Preview distributions are not selected.

### Research limits and sources

The supplied research covers all 46 stable manifests and registry format/auth guidance. The 16 exact-version package-bin citations above were checked against provider-published metadata on 2026-11-24, as was the siGit exact-version README. They supplement the registry manifests; a package name alone is not executable evidence. These source checks establish executable names and documented arguments, not working installations or runtime compatibility.

Registry authentication guidance describes Agent Auth or Terminal Auth, but membership does not prove our client implements a provider's advertised flow. Terminal Auth setup args/env **replace**, not append to, the normal ACP launch args/env. Provider installation, environment setup, credentials and authentication remain outside this discovery change. No provider downloads or provider execution are needed to enumerate these bundled installed candidates; later use follows the existing custom-ACP launch path and may use the provider's network services.

Validation status: scoped documentation/source-link inspection only. **Builds: PENDING. Product tests: PENDING. Runtime discovery/ACP/auth/resume: UNVERIFIED.** Reconcile affected claims after the implementation settles and the delivery worker pins the final candidate.

Native resume research and explicit unsupported/unknown contracts:
[native session contracts](acp-registry-native-sessions.md).

[registry]: https://github.com/agentclientprotocol/registry/tree/2a27ff18b5816f47d7ec538e22943ff194e05131
[Registry format](https://github.com/agentclientprotocol/registry/blob/2a27ff18b5816f47d7ec538e22943ff194e05131/FORMAT.md)
· [Authentication contract](https://github.com/agentclientprotocol/registry/blob/2a27ff18b5816f47d7ec538e22943ff194e05131/AUTHENTICATION.md)
· [Quarantine](https://github.com/agentclientprotocol/registry/blob/2a27ff18b5816f47d7ec538e22943ff194e05131/quarantine.json)
