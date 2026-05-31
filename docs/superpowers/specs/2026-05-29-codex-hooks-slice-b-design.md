# Codex hooks — slice B design

**Date:** 2026-05-29
**Branch:** `dev/<user>/codex-session`
**Builds on:** slice A (read-only Codex session discovery, merged into PR #98)
**Successor:** slice C (l10n, docs, ut_app fixtures, ACP `loadSession`)

## Goal

Extend `wta hooks {install,status,uninstall}` to register a **wt-agent-hooks** plugin with the Codex CLI so live Codex hook events flow into the Windows Terminal session-management UI (Ctrl+Shift+/), bringing Codex to parity with Copilot / Claude / Gemini for hook plumbing. Add a fourth "Codex CLI" row to the **AI Agents** settings page so users can install / remove hooks from the GUI.

## Non-goals (slice C)

- l10n (.resw) strings for "Codex CLI" display name / status messages
- README.md / CLAUDE.md doc updates mentioning Codex
- `agent_check::install()` adding `winget install OpenAI.Codex`
- ACP `loadSession` support for Shift+Enter in-pane resume
- C++ `ut_app/AgentHooksStatusTests.cpp` fixture rows for Codex
- `STATUS_SCHEMA_VERSION` bump (would require coordinated C++ change)
- Trust-status field in `CliStatus` (Codex requires interactive `/hooks` trust step; surfaced via README in slice C)

## Architecture

### Rust: `tools/wta/src/agent_hooks_installer.rs`

| Existing symbol | Change |
| --- | --- |
| `CliKind` enum | Add `Codex` variant |
| `CliKind::ALL` | Append `CliKind::Codex` |
| `CliKind::name()` | `Codex => "codex"` |
| `CliKind::from_name()` | `"codex" => Codex` |
| `CliKind::dir_name()` | `Codex => "codex"` |
| `install_for_*` dispatch | New `install_for_codex` (mirror of `install_for_claude`) |
| `status_for_*` dispatch | New `status_for_codex` |
| `uninstall_for_*` dispatch | New `uninstall_for_codex` |
| `maybe_stage_bundle_for_claude` | Sibling `maybe_stage_bundle_for_codex` (or parameterize on `CliKind`) |
| `STATUS_SCHEMA_VERSION` | **unchanged (3)** — Codex row uses existing `CliStatus` shape |
| `PLUGIN_NAME` / `MARKETPLACE_NAME` | reused (`wt-agent-hooks` / `wt-local`) |

#### `install_for_codex(home, opts)`

1. Skip cleanly when `~/.codex/` is absent (CLI never used on this machine).
2. Resolve bundle dir via `bundle::resolve_cli_dir(CliKind::Codex)` — same lookup chain (env var → exe-sibling → dev-tree).
3. If the bundle resolves under `WindowsApps`, restage to `LOCALAPPDATA\Microsoft\IntelligentTerminal\hook-bundle-staging\codex` (mirror existing Claude workaround in case `codex plugin add` Rust-side copy has any similar issue; harmless if not).
4. Spawn `codex plugin marketplace add <bundle_path>` — registers the `wt-local` marketplace.
5. Spawn `codex plugin add wt-agent-hooks@wt-local` — installs the plugin into Codex's config.

Both commands run with stdin closed and 30-second timeout. Output captured to tracing logs at `target: "agent_hooks"`.

#### `status_for_codex(home)`

**Primary path** (CLI on PATH):
- `codex plugin marketplace list` → text-parse columns `MARKETPLACE  ROOT`, look for row whose name == `wt-local`. Set `marketplace_registered` and `marketplace_path` from the `ROOT` column. Compute `marketplace_path_valid` by running stat on the path (`directory` exists check, same logic as Claude/Copilot).
- `codex plugin list` → text-parse for a row whose `PLUGIN` column == `wt-agent-hooks`. Set `plugin_installed`. Codex has no enable/disable distinction in `plugin list` output → `plugin_enabled := plugin_installed`.

**Filesystem fallback** (CLI not on PATH or commands fail):
- Read `~/.codex/config.toml`. Parse TOML for plugin/marketplace entries (exact key names verified during plan-task probe). Set `detection_fallback = "fs"` on the returned `CliStatus`.

`binary_on_path` / `binary_path` come from the standard `which`-lookup helper that the other CLIs use.

#### `uninstall_for_codex(home)`

1. `codex plugin remove wt-agent-hooks@wt-local` — remove plugin first.
2. `codex plugin marketplace remove wt-local` — then remove the marketplace registration.
3. Best-effort cleanup of any LOCALAPPDATA staging dir created by step 3 of install.
4. Populate `CliUninstallResult.messages` with command outcomes.

### Bundle: `tools/wta/wt-agent-hooks/codex/`

```
codex/
  .agents/plugins/marketplace.json           ← per developers.openai.com/codex/plugins/build
  plugins/wt-agent-hooks/
    .codex-plugin/plugin.json                ← Codex plugin manifest
    hooks/hooks.json                          ← 4 events
    hooks/send-event.ps1                      ← byte-identical copy of claude/wt-agent-hooks/hooks/send-event.ps1
```

**`marketplace.json`** (required Codex schema):

```json
{
  "$schema": "https://developers.openai.com/codex/marketplace.schema.json",
  "name": "wt-local",
  "displayName": "Windows Terminal (local)",
  "plugins": {
    "wt-agent-hooks": {
      "source": { "source": "local", "path": "./plugins/wt-agent-hooks" },
      "policy": {
        "installation": "AVAILABLE",
        "authentication": "ON_INSTALL"
      },
      "category": "Productivity"
    }
  }
}
```

(Exact key names — including whether `plugins` is an object map or array, and the precise shape of `source` / `policy` — will be cross-checked against the live `~/.codex/.tmp/plugins/openai-curated` reference during the bundle-creation task. The shape above is the working hypothesis from the discovery probe; the task will lock it in and add a unit test that round-trips through `codex plugin marketplace add`.)

**`plugin.json`** (minimal Codex plugin manifest):

```json
{
  "$schema": "https://developers.openai.com/codex/plugin.schema.json",
  "name": "wt-agent-hooks",
  "version": "0.1.0",
  "displayName": "WT Agent Hooks",
  "description": "Forward Codex hook events to Windows Terminal for session-management UI."
}
```

**`hooks.json`** (4 events; Codex env var `${PLUGIN_ROOT}`):

```json
{
  "hooks": {
    "SessionStart": [
      {
        "matcher": "startup|resume",
        "hooks": [
          { "type": "command",
            "command": "powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -File \"${PLUGIN_ROOT}\\hooks\\send-event.ps1\" -CliSource codex -EventName SessionStart" }
        ]
      }
    ],
    "PermissionRequest": [
      { "hooks": [
        { "type": "command",
          "command": "powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -File \"${PLUGIN_ROOT}\\hooks\\send-event.ps1\" -CliSource codex -EventName PermissionRequest" }
      ] }
    ],
    "UserPromptSubmit": [
      { "hooks": [
        { "type": "command",
          "command": "powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -File \"${PLUGIN_ROOT}\\hooks\\send-event.ps1\" -CliSource codex -EventName UserPromptSubmit" }
      ] }
    ],
    "Stop": [
      { "hooks": [
        { "type": "command",
          "command": "powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -File \"${PLUGIN_ROOT}\\hooks\\send-event.ps1\" -CliSource codex -EventName Stop" }
      ] }
    ]
  }
}
```

The stable `powershell ... -File <fixed path>` wrapper means trust-on-hash survives `send-event.ps1` content updates — same trick used for Claude.

**`send-event.ps1`** — byte-for-byte copy of `claude/wt-agent-hooks/hooks/send-event.ps1`. The script accepts `-CliSource codex` from the installer-baked command line, so its env-var fallback chain (which knows about `CLAUDE_SESSION_ID`/`COPILOT_SESSION_ID`/`GEMINI_SESSION_ID` but not `CODEX_SESSION_ID`) never fires in this code path. Slice C may add `CODEX_SESSION_ID` to that chain for defensive correctness.

### C++ / XAML side

`src/cascadia/TerminalSettingsEditor/`:

| File | Change |
| --- | --- |
| `AIAgentsViewModel.idl` | Add `CodexHooksSubtitle`, `ShowCodexHooksSubtitle`, `RemoveCodexHooks` |
| `AIAgentsViewModel.h` | Add `_codexHooksSubtitle` field; mirror three Claude getter methods |
| `AIAgentsViewModel.cpp` | Populate `_codexHooksSubtitle` from the `clis[]` entry whose `name == "codex"`; add `RemoveCodexHooks` body; add `L"CodexHooksSubtitle"` + `L"ShowCodexHooksSubtitle"` to the property-change broadcast list (lines 905–910) |
| `AIAgents.xaml` | New `<Grid>` row for "Codex CLI" — ~30 lines mirroring the Gemini block at lines ~327–346 |

`src/cascadia/inc/AgentHooksStatus.h`:

- Line 42 doc comment: extend `"copilot" \| "claude" \| "gemini"` → `"copilot" \| "claude" \| "gemini" \| "codex"`.

**No changes to:**
- `AgentHooksStatus.h` parser logic — already CLI-name-agnostic.
- `ut_app/AgentHooksStatusTests.cpp` — fixture rows are illustrative only; parser test coverage stays equivalent.
- `CascadiaPackage.wapproj` — content glob `tools\wta\wt-agent-hooks\**` auto-picks up new `codex/` subtree.

## Data flow

```
Codex CLI runs hook  →  powershell -File send-event.ps1 -CliSource codex -EventName ...
                    →  send-event.ps1 POSTs to wta IPC endpoint
                    →  wta receives event, tags with cli=codex, persists to history_loader
                    →  Settings UI / Ctrl+Shift+/ list pick up via StatusReport / session enumeration (slice A)
```

## Error handling

- `~/.codex/` missing → `install_for_codex` returns `Skipped` with reason. Status reports `binary_on_path: false`, all plugin fields `false`.
- `codex` not on PATH → status falls back to filesystem (`detection_fallback = "fs"`); install errors with clear log.
- `codex plugin add` non-zero exit → captured to log + `messages`; partial state surfaced via existing `marketplace_registered: true, plugin_installed: false` ("partially installed") C++ formatter.
- Trust step (user must run `/hooks`) is **outside** wta's control — surfaced via slice-C README; slice B's `install` returns success on registration even though events won't fire until trusted.

## Testing strategy

### Rust unit tests (`agent_hooks_installer.rs` tests module)

1. `CliKind::Codex` round-trips: `from_name("codex")` and `Codex.name() == "codex"`; appears in `CliKind::ALL`.
2. `bundle::resolve_cli_dir(CliKind::Codex)` finds `codex/` via env-var override / exe-sibling / dev-tree fixtures.
3. `install_for_codex` skips cleanly when `~/.codex/` missing.
4. `install_for_codex` invokes the two expected commands in order when `~/.codex/` present (mock executor verifies args).
5. `parse_codex_marketplace_list` extracts the `wt-local` row from a golden text sample.
6. `parse_codex_plugin_list` extracts the `wt-agent-hooks` row from a golden text sample.
7. Filesystem fallback parses a fixture `config.toml`.
8. `uninstall_for_codex` issues `plugin remove` then `marketplace remove` in order.
9. Existing parameterized tests (e.g. `installer_skips_when_home_missing`) get a `CliKind::Codex` arm if they iterate over `CliKind::ALL`.

### Build verification

- `cargo test --manifest-path tools/wta/Cargo.toml` — expect 585 (current) → ~605+ passing.
- Visual Studio build of `CascadiaPackage` solution — verifies XAML / IDL compile and the new ViewModel members link.

### Manual smoke test (documented in PR body, not gated)

1. Install Codex CLI (`winget install OpenAI.Codex` or download).
2. `wta hooks install` → expect successful Codex registration in command output.
3. Open Codex, run `/hooks`, trust the **wt-agent-hooks** plugin.
4. Start a session, submit a prompt → confirm the session appears live in Ctrl+Shift+/ with cli=codex.
5. Open **Settings → AI Agents** → verify the new "Codex CLI" row shows "hooks installed".
6. Click **Remove hooks** → verify `Codex CLI — hooks not installed`.
7. `wta hooks uninstall` → idempotent cleanup.

## Open verification items (resolved during task execution, not blocking design)

- Exact `marketplace.json` / `plugin.json` key spelling against live `~/.codex/.tmp/plugins/openai-curated` (probe in the bundle-creation task).
- `codex plugin add` non-interactive behavior under WindowsApps subtree (verified during install task; staging fallback already accounts for it).
- Empirical `Stop` event reliability (verified during manual smoke; if poor, fallback fix is slice C).
- TOML key names for `[[plugin.marketplaces]]` in `~/.codex/config.toml` (verified during filesystem-fallback task).
