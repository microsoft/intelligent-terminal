# Registry agents: native launch and resume evidence

Observed 2026-11-24. Registry snapshot:
`2a27ff18b5816f47d7ec538e22943ff194e05131`.
**Source-verified is not runtime-verified.** No provider was installed, launched,
authenticated or resumed. Documentation at a different source revision is not
proof that the registry's exact distributed version implements those flags.
The installed-discovery feature creates **no new native profiles**, hooks,
history scanners, ACP-to-native ID mappings or resume templates. This research
is retained to explain that boundary, not to instruct users to configure a
registry profile. Product builds/tests for the corrected candidate are
**PENDING**, with no inherited acceptance from the earlier preset design.

The companion [launch matrix](acp-registry-agents.md#complete-pinned-launch-inventory)
covers all 46 pinned ACP manifest entries, not 46 installed dropdown choices.
The current local catalog has 30 executable-discovery candidates; the existing
six built-ins are unchanged. Here, `ID` means a provider-native saved
session identifier, **not an ACP SessionId**, unless a mapping is separately
proven. No new mapping has been proven for use by this feature.

## Findings that rule out a universal resume template

* **Auggie changes argument meaning across major versions.** The registry pins
  0.37.0: the official [0.x reference][auggie0] documents
  `auggie --resume <sessionId>` (ID/prefix). The default [1.x reference][auggie1]
  instead documents `--resume` as a picker and `--session <path>` as a session
  file. Do not copy the default documentation's flags into the pinned 0.x entry.
* **Amp uses different IDs.** The adapter's pinned
  [README][amp] documents durable `S-…` ACP IDs mapped to `T-…` native Amp thread
  IDs. Old sessions can lack the mapping. `amp threads continue` continues the
  latest thread; it does not establish a safe direct native resume command
  accepting an arbitrary ACP ID.
* **Pickers are not direct resume.** `devin --resume`, `pool -r`, and
  `dirac history` are not evidence that appending an ACP ID is valid.
* **Native and adapter executables differ.** `vibe-acp`, `claude-agent-acp`,
  `codex-acp`, `corust-agent-acp`, and Copilot Language Server must not be used as
  interactive native commands just because their ACP launch works.
* **Do not invent compatibility machinery.** Per-provider ID translation,
  flags inferred from names, runtime registry refresh and implicit WSL command
  rewriting would be fragile. The bounded feature is bundled metadata and
  executable-based installed discovery in the existing dropdown/custom-ACP
  path, with negotiated ACP load support and honest unknown/unsupported native
  resume. It adds no native-profile configuration workflow.
* **A shared executable is not shared native history.** `kimi-code` and `kimi`
  coalesce into one actual `kimi acp` installed choice. That avoids duplicate
  ACP choices; it establishes no equivalence between their versions, storage
  formats or saved-session identifiers.

## Complete native-contract disposition

`Unknown` below means the consulted official README/provider entry page did not
establish an exact native startup/resume contract. It does **not** prove the CLI
lacks that capability. All rows remain runtime-unverified. A slash command is
reported as a slash command, not silently converted to a startup parameter.

| Registry entry | Native interactive launch / resume established by source | Identifier semantics / disposition |
|---|---|---|
| agoragentic-acp | Unknown; quarantined | README checked; do not infer a TUI from its MCP/ACP package. |
| amp-acp | Underlying Amp; `amp threads continue` documented as latest-thread continuation | `S-…` ACP ≠ `T-…` native; adapter mapping required. Direct native-ID resume syntax not established here. [source][amp] |
| antigravity-acp | Unknown from provider IDE-extension page | Existing built-in behavior is unchanged; do not equate server UID with a native conversation ID. |
| auggie | `auggie`; `auggie --resume ID`; `--continue` for latest | 0.x ID/prefix; not 1.x session-file semantics. [source][auggie0] |
| autohand | `autohand`; `--resume [id]`; `--continue` latest in folder | Saved native session ID; no ACP equality proof. [source][autohand] |
| bob | Unknown from IBM provider landing page | No native flags guessed from `bob acp`. |
| claude-acp | Separate `claude`; `claude -r ID-or-name` | Native saved conversation ID/name; adapter mapping not established by this research. [source][claude] |
| cline | Unknown from root README | CLI-specific resume parser still requires verification; VS Code task history is not proof of CLI grammar. |
| codebuddy-code | Unknown from provider CLI landing page | No ID or resume mapping verified. |
| codex-acp | Separate `codex`; `codex resume`; explicit session selection by ID documented | Native interactive session; do not assume adapter ACP ID is that ID. [source][codex] |
| cortex-code | Unknown from Snowflake product page | `acp serve` is not a native interactive invocation. |
| corust-agent | Unknown from release README | Registry executable is `corust-agent-acp`; no native launch/resume inferred. |
| crow-cli | README documents interactive `crow-agent --session-id ID` | Durable native DB session; `crow-cli run -s ID "prompt"` is a different one-shot interface. [source][crow] |
| cursor | Provider docs: native launcher `agent`, `--resume [chatId]`, `--continue` (`--resume=-1`) | Native chatId. Registry archive names `cursor-agent`; that name alone does not establish the installed native alias/path. [source][cursor] |
| deepagents | Unknown; quarantined | README checked; no native ID mapping inferred. |
| devin | `devin`; `--continue` latest; bare `--resume` picker | Specific-ID argument not established by README. [source][devin] |
| dimcode | Unknown from provider landing page | `acp` subcommand is not evidence of native resume. |
| dirac | `dirac`; `dirac history` documented | History entry point only; exact direct-ID startup flag unknown. [source][dirac] |
| factory-droid | `droid`; `droid --resume [sessionId]` (alias `-r`) | Native saved session ID, default last modified. `droid exec -s ID` is noninteractive, not the native profile command. [source][droid] |
| fast-agent | Unknown from root README | `uvx fast-agent-acp -x` is an ACP process, not proof of native TUI or resume grammar. |
| gemini | `gemini`; `gemini --resume UUID` or project-local index; bare `--resume` latest | Project-scoped saved sessions; prefer full UUID over changing index. [source][gemini] |
| github-copilot-cli | `copilot`; `--resume=VALUE`; bare `--resume` picker; `--continue` latest | VALUE can be ID/prefix/name, not necessarily exact. Native session/task distinctions matter. [source][copilot] |
| github-copilot | Language Server; no native TUI/resume established | Separate from `github-copilot-cli`; sharing artwork does not make the commands or IDs interchangeable. |
| glm-acp-agent | ACP lifecycle documented; no native interactive CLI established | README documents `session/load`, `session/resume`, persisted ACP sessions; not a native resume flag. [source][glm] |
| goose | `goose session --resume --session-id ID` | Native ID example `YYYYMMDD_COUNT`; session name is a separate selector. [source][goose] |
| grok-build | Unknown; provider page returned HTTP 403 to the research fetch | `agent stdio` does not prove native profile/resume syntax. |
| harn | Unknown native CLI flags | Official session guide shows language/runtime `session_id` APIs, not a native CLI resume flag. |
| junie | Unknown from ACP release README | `--acp=true` is only the ACP server contract. |
| kilo | Unknown from root README | ACP command and package name do not establish native session flag/ID. |
| kimchi | Unknown startup flag from root README | `/ferment resume` is a workflow lifecycle command, not proof of conversation startup resume. |
| kimi-code | Current official docs: `kimi`; `kimi --session ID`; `--continue` latest cwd | Saved session ID in `~/.kimi-code`; docs also mention `--resume` for forks. Do not merge this JS package's IDs with the legacy Python CLI. [source][kimicode] |
| kimi | `kimi`; `--session ID` or `--resume ID`, `-r ID`; bare selector is a picker | Legacy Python CLI, cwd-scoped saved native session; distinct registry/version/storage contract. [source][kimi] |
| kiro | Unknown from root README/provider ACP page | Registry engine-v3 launch is Linux-only; cloud handoff/resume is not proof of local native CLI syntax. |
| minimax-code | Unknown from provider landing page | ACP adapter/package may differ from native CLI. |
| minion-code | Unknown; quarantined | README checked; do not infer flags. |
| mistral-vibe | Separate `vibe`; `--resume [SESSION_ID]`; `--continue` folder/terminal latest | Parser distinguishes exact ById, picker and continue; no ACP ID equivalence assumed. [source][vibe] |
| muse-code | Unknown from provider documentation landing page | Experimental ACP environment flag does not establish native resume. |
| nova | Unknown from root README | No native launch/resume inferred from npm bin/package name. |
| opencode | `opencode` interactive launch documented; current fetched intro omitted resume grammar | Existing built-in `--session` behavior retained; this research does not certify current adapter-to-native ID equivalence. |
| pi-acp | Separate `pi`; README documents in-TUI `/resume` and shared session files | ACP session history support is documented; native startup parameter and ID-vs-file-path contract require upstream pi CLI verification. [source][pi] |
| poolside | `pool`; README documents `pool -r` and in-TUI `/resume` | Exact ID-taking argument not established here. [source][pool] |
| qoder | Unknown from provider landing page | No direct native-ID resume flag verified. |
| qwen-code | Parser: `qwen --resume ID-or-title`; `--continue`; bare `--resume` picker | `--session-id UUID` starts a specified ID **without resumption**; not interchangeable with `--resume`. [source][qwen] |
| sigit | Exact-version README: bare `sigit` starts the local terminal UI; `sigit --acp` starts editor-agent mode | Native resume flag/ID unknown. The pinned registry omits `--acp`; the local ACP command explicitly overrides it following the README. Do not treat bare `sigit` as ACP. [source][sigit] |
| stakpak | Parser: `stakpak --session ID` (alias `-s`) | Resumes latest checkpoint in that native session; conflicts with checkpoint ID selector. [source][stakpak] |
| vtcode | README: `vtcode continue` latest; `vtcode exec resume --last "prompt"` headless | Quarantined; no direct-ID native interactive startup parameter established. [source][vtcode] |

### Source identity and coverage

The 33 GitHub-linked providers' root READMEs were fetched at exact HEAD SHAs,
not executed. Focused native session docs/parsers were then read for Amp,
Autohand, crow, Devin, Dirac, GLM, Goose, legacy Kimi, Mistral Vibe, pi, Poolside,
Qwen, Stakpak and VT Code. Official proprietary-provider landing pages were also
read; lack of a flag there is reported as unknown, not unsupported. This is a
bounded source audit, not an exhaustive proof of every CLI option's absence.
The generated launch matrix retains provider repository/docs locators for every
row. The named source links below pin actual claims wherever source is public.
Live documentation links are timestamped observations, not immutable version
certification; recheck them against the user's installed version before use.
The siGit row additionally uses the published exact-version 1.7.10 README,
directly inspected for this correction; it supersedes the earlier assumption
that bare `sigit` defaults to ACP. No native-resume syntax is inferred from that
correction.

The registry revision pins research input, not a final product candidate or
the version found on the user's PATH. Implementation is still changing before
final pin. None of these source observations is a passing build, product test,
runtime session-load result or native-resume result.

[amp]: https://github.com/tao12345666333/amp-acp/blob/6d5b506a9653987abee4313267eb1edee22221b6/README.md#session-persistence
[auggie0]: https://docs.augmentcode.com/cli/v0/reference.md#sessions
[auggie1]: https://docs.augmentcode.com/cli/reference#sessions
[autohand]: https://github.com/autohandai/code-cli/blob/28c58e5ee196c04c7162da70128c65e47f233237/README.md
[claude]: https://code.claude.com/docs/en/cli-reference
[codex]: https://developers.openai.com/codex/cli/reference
[crow]: https://github.com/crow-cli/crow-cli/blob/b23bfa2e61d3e08d79b94ef2c7254ce3797b3b81/README.md
[cursor]: https://cursor.com/docs/cli/reference/parameters
[devin]: https://github.com/CognitionAI/devin-cli/blob/eb11ea01cfa276b37cf1ffb00cca7dd097fdb5f9/README.md
[dirac]: https://github.com/dirac-run/dirac/blob/192823449ad958f4f02bcf60db309a39d46c9c28/README.md
[droid]: https://docs.factory.com/droid-cli/cli-reference
[gemini]: https://geminicli.com/docs/cli/session-management/
[copilot]: https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-command-reference
[glm]: https://github.com/stefandevo/glm-acp-agent/blob/9c2e6325042c79e6d4a2054c91fcdd01ef38485e/README.md
[goose]: https://github.com/block/goose/blob/c9572369a8e0a60ea505c046496377d5f8afbc87/documentation/docs/guides/sessions/session-management.md
[kimicode]: https://moonshotai.github.io/kimi-code/en/guides/sessions
[kimi]: https://github.com/MoonshotAI/kimi-cli/blob/9ab1286b8fe4e6bcd116949a27ce5e0ac3389c82/docs/en/guides/sessions.md
[vibe]: https://github.com/mistralai/mistral-vibe/blob/e76235b203b8965af584e88e69284bc704c31a1f/vibe/cli-rust/src/cli.rs#L46-L52
[pi]: https://github.com/svkozak/pi-acp/blob/41fb17a54f4599a0ca4723887cce0f40aaea325a/README.md
[pool]: https://github.com/poolsideai/pool/blob/fcabc59ff67810ecec5429ec9d45fa1dda6c1136/README.md
[qwen]: https://github.com/QwenLM/qwen-code/blob/f31eb5cd0d0c802f7077d7fa193164a91c52743f/packages/cli/src/config/config.ts#L222-L242
[sigit]: https://unpkg.com/@smbcloud/sigit@1.7.10/README.md
[stakpak]: https://github.com/stakpak/agent/blob/760cd2b5984d29c2d513bb15ca33e995fae45f17/cli/src/main.rs#L132-L134
[vtcode]: https://github.com/vinhnx/VTCode/blob/d14ca52878b32eec4e92177bf8142b8b9bfaa2da/README.md
