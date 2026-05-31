# Codex hooks slice B — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Register a `wt-agent-hooks` plugin with the Codex CLI through `wta hooks {install,status,uninstall}` and surface a "Codex CLI" row in the AI Agents settings page so live Codex hook events flow into the Windows Terminal session-management UI (parity with Copilot/Claude/Gemini).

**Architecture:** Direct mirror of the Claude implementation pattern in `tools/wta/src/agent_hooks_installer.rs`. New `CliKind::Codex` variant + per-CLI `install_for_codex` / `codex_status` / `uninstall_for_codex` functions + new `tools/wta/wt-agent-hooks/codex/` bundle. C++/XAML side gets a new `CodexHooksSubtitle` ViewModel triplet and a 4th row in `AIAgents.xaml`.

**Tech Stack:** Rust (cargo, serde, serde_json — already wired), C++17 with WinRT (CascadiaPackage / TerminalSettingsEditor), XAML, PowerShell. Codex CLI 0.135.0 (verified). `RUSTUP_TOOLCHAIN=stable` required before every cargo invocation (the repo pins `ms-prod-1.93` which isn't installed locally).

**Reference spec:** `docs/superpowers/specs/2026-05-29-codex-hooks-slice-b-design.md`

**Worktree:** `C:\<user>\GitRepo\intelligent-terminal\.worktree\codex-session` (branch `dev/<user>/codex-session`, PR #98).

---

## File map

**New files:**
- `tools/wta/wt-agent-hooks/codex/.agents/plugins/marketplace.json`
- `tools/wta/wt-agent-hooks/codex/plugins/wt-agent-hooks/.codex-plugin/plugin.json`
- `tools/wta/wt-agent-hooks/codex/plugins/wt-agent-hooks/hooks/hooks.json`
- `tools/wta/wt-agent-hooks/codex/plugins/wt-agent-hooks/hooks/send-event.ps1` (copy of Claude's)

**Modified files:**
- `tools/wta/src/agent_hooks_installer.rs` — main installer/status/uninstall changes
- `src/cascadia/inc/AgentHooksStatus.h` — one-line doc comment update
- `src/cascadia/TerminalSettingsEditor/AIAgentsViewModel.idl` — add 3 members
- `src/cascadia/TerminalSettingsEditor/AIAgentsViewModel.h` — add field + 3 method declarations
- `src/cascadia/TerminalSettingsEditor/AIAgentsViewModel.cpp` — add bodies + status mapping + property-change list
- `src/cascadia/TerminalSettingsEditor/AIAgents.xaml` — add a 4th `<Grid>` row

**Unchanged:**
- `src/cascadia/CascadiaPackage/CascadiaPackage.wapproj` — content glob auto-picks up the new bundle subtree
- `src/cascadia/ut_app/AgentHooksStatusTests.cpp` — parser is CLI-name-agnostic; ut_app additions deferred to slice C

---

## Task ordering rationale

Tasks 1→8 are pure Rust and can be developed/tested in `tools/wta/` without touching the C++ build. Tasks 9–12 wire the new row into the Settings UI (requires Visual Studio build to verify). Task 13 is the final cross-cutting verification.

The bundle files (Task 2) are created early so later Rust tests can stat real paths instead of mocking the filesystem layout.

---

## Test command (use everywhere)

```powershell
$env:RUSTUP_TOOLCHAIN="stable"
cd C:\<user>\GitRepo\intelligent-terminal\.worktree\codex-session
cargo test --manifest-path tools/wta/Cargo.toml
```

Baseline before any slice-B work: **585 tests pass**.

---

### Task 1: Extend `CliKind` enum with `Codex`

**Files:**
- Modify: `tools/wta/src/agent_hooks_installer.rs` (lines 161–198 — `CliKind` enum + impls)

**Existing tests to update:** any test that iterates `CliKind::ALL` and asserts a count — search for `CliKind::ALL.len()` and `CliKind::ALL.iter()`.

- [ ] **Step 1: Write the failing test**

Add at the bottom of the existing `#[cfg(test)] mod tests` block (around line 2238+):

```rust
#[test]
fn cli_kind_codex_roundtrips() {
    assert_eq!(CliKind::from_name("codex"), Some(CliKind::Codex));
    assert_eq!(CliKind::from_name("CODEX"), Some(CliKind::Codex));
    assert_eq!(CliKind::Codex.name(), "codex");
    assert_eq!(CliKind::Codex.dir_name(), "codex");
    assert!(CliKind::ALL.contains(&CliKind::Codex));
}
```

- [ ] **Step 2: Run test to verify it fails**

```powershell
$env:RUSTUP_TOOLCHAIN="stable"
cargo test --manifest-path tools/wta/Cargo.toml cli_kind_codex_roundtrips
```

Expected: **FAIL** — `no variant or associated item named Codex found for enum CliKind`.

- [ ] **Step 3: Add the `Codex` variant**

Edit lines 161–198 to:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliKind {
    Copilot,
    Claude,
    Gemini,
    Codex,
}

impl CliKind {
    pub const ALL: &'static [CliKind] = &[
        CliKind::Copilot,
        CliKind::Claude,
        CliKind::Gemini,
        CliKind::Codex,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Copilot => "copilot",
            Self::Claude => "claude",
            Self::Gemini => "gemini",
            Self::Codex => "codex",
        }
    }

    pub fn from_name(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "copilot" => Some(Self::Copilot),
            "claude" => Some(Self::Claude),
            "gemini" => Some(Self::Gemini),
            "codex" => Some(Self::Codex),
            _ => None,
        }
    }

    fn dir_name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Copilot => "copilot",
            Self::Gemini => "gemini-extension",
            Self::Codex => "codex",
        }
    }
}
```

- [ ] **Step 4: Run the new test plus the full suite**

```powershell
$env:RUSTUP_TOOLCHAIN="stable"
cargo test --manifest-path tools/wta/Cargo.toml cli_kind_codex_roundtrips
cargo test --manifest-path tools/wta/Cargo.toml
```

Expected: new test PASSES. Full suite has compile errors at every `match cli { ... }` site that doesn't yet cover `Codex` — that's expected; we'll fix them as we add per-CLI functions. **Note the exact non-exhaustive match errors** so the next tasks can reference them.

If a counting test fails (e.g. `assert_eq!(CliKind::ALL.len(), 3)`), update its expected value to `4`.

- [ ] **Step 5: Commit**

```powershell
git add tools/wta/src/agent_hooks_installer.rs
git commit -m "feat(wta): add CliKind::Codex variant`n`nCo-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```

---

### Task 2: Create the Codex bundle skeleton

**Files:**
- Create: `tools/wta/wt-agent-hooks/codex/.agents/plugins/marketplace.json`
- Create: `tools/wta/wt-agent-hooks/codex/plugins/wt-agent-hooks/.codex-plugin/plugin.json`
- Create: `tools/wta/wt-agent-hooks/codex/plugins/wt-agent-hooks/hooks/hooks.json`
- Create: `tools/wta/wt-agent-hooks/codex/plugins/wt-agent-hooks/hooks/send-event.ps1` (byte-identical copy of `tools/wta/wt-agent-hooks/claude/wt-agent-hooks/hooks/send-event.ps1`)

**Probe first:** inspect the live reference marketplace at `~/.codex/.tmp/plugins/.agents/plugins/marketplace.json` and any plugin under `~/.codex/.tmp/plugins/plugins/*/.codex-plugin/plugin.json`. The working hypothesis below should be cross-checked against the actual keys.

- [ ] **Step 1: Inspect the reference marketplace**

```powershell
Get-Content $HOME\.codex\.tmp\plugins\.agents\plugins\marketplace.json
Get-ChildItem $HOME\.codex\.tmp\plugins\plugins\*\.codex-plugin\plugin.json | Select-Object -First 1 | ForEach-Object { Get-Content $_.FullName }
```

Expected: JSON with `plugins` field (either object map or array), per-plugin `source`/`policy`/`category` fields. **Note the exact shape** and adjust the marketplace.json below if it differs.

- [ ] **Step 2: Create `marketplace.json`**

Working content (adjust if Step-1 probe shows a different shape):

```json
{
  "name": "wt-local",
  "displayName": "Windows Terminal (local)",
  "description": "Local marketplace populated by wta",
  "owner": { "name": "Agentic Terminal" },
  "plugins": {
    "wt-agent-hooks": {
      "source": { "source": "local", "path": "./plugins/wt-agent-hooks" },
      "policy": {
        "installation": "AVAILABLE",
        "authentication": "ON_INSTALL"
      },
      "category": "Productivity",
      "interface": {
        "displayName": "WT Agent Hooks",
        "description": "Forward Codex hook events to Windows Terminal."
      }
    }
  }
}
```

- [ ] **Step 3: Create `plugin.json`**

```json
{
  "name": "wt-agent-hooks",
  "version": "0.1.0",
  "displayName": "WT Agent Hooks",
  "description": "Forward Codex hook events to Windows Terminal for session-management UI.",
  "author": { "name": "Agentic Terminal" }
}
```

- [ ] **Step 4: Create `hooks/hooks.json`**

```json
{
  "hooks": {
    "SessionStart": [
      {
        "matcher": "startup|resume",
        "hooks": [
          {
            "type": "command",
            "command": "powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -File \"${PLUGIN_ROOT}\\hooks\\send-event.ps1\" -CliSource codex -EventName SessionStart"
          }
        ]
      }
    ],
    "PermissionRequest": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -File \"${PLUGIN_ROOT}\\hooks\\send-event.ps1\" -CliSource codex -EventName PermissionRequest"
          }
        ]
      }
    ],
    "UserPromptSubmit": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -File \"${PLUGIN_ROOT}\\hooks\\send-event.ps1\" -CliSource codex -EventName UserPromptSubmit"
          }
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -File \"${PLUGIN_ROOT}\\hooks\\send-event.ps1\" -CliSource codex -EventName Stop"
          }
        ]
      }
    ]
  }
}
```

- [ ] **Step 5: Copy `send-event.ps1` byte-identically from Claude**

```powershell
Copy-Item `
  tools\wta\wt-agent-hooks\claude\wt-agent-hooks\hooks\send-event.ps1 `
  tools\wta\wt-agent-hooks\codex\plugins\wt-agent-hooks\hooks\send-event.ps1
```

Verify byte-identical:

```powershell
(Get-FileHash tools\wta\wt-agent-hooks\claude\wt-agent-hooks\hooks\send-event.ps1).Hash `
  -eq (Get-FileHash tools\wta\wt-agent-hooks\codex\plugins\wt-agent-hooks\hooks\send-event.ps1).Hash
```

Expected: `True`.

- [ ] **Step 6: Smoke-test the bundle with the live Codex CLI**

```powershell
codex plugin marketplace add (Resolve-Path tools\wta\wt-agent-hooks\codex).Path
codex plugin marketplace list
codex plugin add wt-agent-hooks@wt-local
codex plugin list
```

Expected: marketplace `wt-local` appears, plugin `wt-agent-hooks` shows as installed. If a JSON-schema validation error occurs, fix the offending key in `marketplace.json` / `plugin.json` and retry.

Then clean up:

```powershell
codex plugin remove wt-agent-hooks@wt-local
codex plugin marketplace remove wt-local
```

- [ ] **Step 7: Commit**

```powershell
git add tools/wta/wt-agent-hooks/codex/
git commit -m "feat(wta): add Codex hooks bundle (marketplace + plugin + 4 hook events)`n`nCo-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```

---

### Task 3: Bundle resolver test for `CliKind::Codex`

**Files:**
- Modify: `tools/wta/src/agent_hooks_installer.rs` — tests module

The existing `bundle` module (line ~314) joins `dir_name()` onto each candidate root, so once `dir_name()` returns `"codex"` and the directory exists on disk (Task 2), resolution should work automatically. This task adds an assertion to prove it.

- [ ] **Step 1: Add the test**

Inside the existing tests module, alongside any other `bundle::` tests:

```rust
#[test]
fn bundle_resolves_codex_dir_in_dev_tree() {
    // Dev-tree lookup walks up from CARGO_MANIFEST_DIR to find
    // tools/wta/wt-agent-hooks/<dir_name>/. Task 2 puts a real
    // directory at that path, so this should resolve.
    let resolved = bundle::resolve_cli_dir(CliKind::Codex)
        .expect("codex bundle should resolve in dev tree");
    assert!(
        resolved.join(".agents").join("plugins").join("marketplace.json").is_file(),
        "resolved codex bundle should contain marketplace.json (got {})",
        resolved.display(),
    );
}
```

- [ ] **Step 2: Run test**

```powershell
$env:RUSTUP_TOOLCHAIN="stable"
cargo test --manifest-path tools/wta/Cargo.toml bundle_resolves_codex_dir_in_dev_tree
```

Expected: **PASS** (Task 1 added `dir_name() == "codex"`; Task 2 created the on-disk files).

- [ ] **Step 3: Commit**

```powershell
git add tools/wta/src/agent_hooks_installer.rs
git commit -m "test(wta): bundle resolver finds codex/ in dev tree`n`nCo-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```

---

### Task 4: `install_for_codex` — skip when `~/.codex/` absent + happy path

**Files:**
- Modify: `tools/wta/src/agent_hooks_installer.rs` — add new function after `install_for_claude` (around line 606)

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn install_for_codex_skips_when_home_absent() {
    let tmp = tempfile::tempdir().unwrap();
    // No ~/.codex created. Function should return cleanly without panic
    // and without spawning `codex` (which may or may not be on PATH on CI).
    install_for_codex(tmp.path());
}
```

- [ ] **Step 2: Run test, expect compile error**

```powershell
$env:RUSTUP_TOOLCHAIN="stable"
cargo test --manifest-path tools/wta/Cargo.toml install_for_codex_skips_when_home_absent
```

Expected: **FAIL** — `cannot find function install_for_codex`.

- [ ] **Step 3: Add the function (and its staging helper)**

Insert after `install_for_claude` (after its closing brace around line 606):

```rust
/// Install hooks for Codex CLI by spawning `codex plugin marketplace add`
/// followed by `codex plugin add`. Mirrors `install_for_claude` in shape.
///
/// Subcommand differences vs Claude:
///   * `codex plugin add` (not `install`)
///   * `codex plugin remove` (not `uninstall`) — used by `uninstall_for_codex`
///   * Marketplace metadata lives in `.agents/plugins/marketplace.json`
///     under the bundle root (not `.claude-plugin/marketplace.json`)
///
/// Trust step: after install, the user must run `/hooks` inside Codex
/// to trust the plugin before any events fire. That's documented in
/// the slice-C README; this function returns success on registration.
fn install_for_codex(home: &Path) {
    let codex_dir = home.join(".codex");
    if !codex_dir.is_dir() {
        tracing::debug!(target: "agent_hooks", "no ~/.codex dir; Codex not present");
        return;
    }

    let bundle_dir = match bundle::resolve_cli_dir(CliKind::Codex) {
        Some(p) => p,
        None => {
            tracing::warn!(
                target: "agent_hooks",
                "no wt-agent-hooks/codex bundle found next to wta.exe or in dev tree; \
                 skipping Codex plugin install (set WTA_HOOKS_BUNDLE_DIR to override)",
            );
            return;
        }
    };

    // Stage out of WindowsApps if necessary — Codex is Rust-native so it
    // shouldn't hit the cpSync EPERM that bites Claude, but staging is
    // cheap insurance and keeps the per-CLI install flow uniform.
    let staged_dir = maybe_stage_bundle_for_codex(&bundle_dir);
    let bundle_dir = staged_dir.as_deref().unwrap_or(&bundle_dir);

    let bundle_path = bundle_dir.to_string_lossy().into_owned();
    if let Err(e) = run_plugin_cli(
        "codex",
        &["plugin", "marketplace", "add", &bundle_path],
        "agent_hooks",
        &["already registered"],
    ) {
        tracing::warn!(
            target: "agent_hooks",
            err = %e,
            "codex plugin marketplace add failed; aborting plugin install",
        );
        return;
    }

    let plugin_ref = format!("{}@{}", PLUGIN_NAME, MARKETPLACE_NAME);
    if let Err(e) = run_plugin_cli(
        "codex",
        &["plugin", "add", &plugin_ref],
        "agent_hooks",
        &[],
    ) {
        tracing::warn!(
            target: "agent_hooks",
            err = %e,
            plugin = %plugin_ref,
            "codex plugin add failed",
        );
    }
}

/// WindowsApps -> LOCALAPPDATA staging for Codex bundles. Mirrors
/// `maybe_stage_bundle_for_claude`; see that function's comment for
/// rationale.
fn maybe_stage_bundle_for_codex(source: &Path) -> Option<PathBuf> {
    if !is_under_windows_apps(source) {
        return None;
    }
    let root = crate::runtime_paths::intelligent_terminal_root()?;
    let staged = root.join(STAGING_SUBDIR).join(CliKind::Codex.dir_name());
    match restage_bundle_dir(source, &staged) {
        Ok(()) => {
            tracing::info!(
                target: "agent_hooks",
                source = %source.display(),
                staged = %staged.display(),
                "restaged codex bundle out of WindowsApps",
            );
            Some(staged)
        }
        Err(e) => {
            tracing::warn!(
                target: "agent_hooks",
                err = %e,
                source = %source.display(),
                staged = %staged.display(),
                "failed to restage codex bundle out of WindowsApps; using original path",
            );
            None
        }
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

```powershell
$env:RUSTUP_TOOLCHAIN="stable"
cargo test --manifest-path tools/wta/Cargo.toml install_for_codex_skips_when_home_absent
```

Expected: **PASS**.

- [ ] **Step 5: Commit**

```powershell
git add tools/wta/src/agent_hooks_installer.rs
git commit -m "feat(wta): add install_for_codex + WindowsApps staging helper`n`nCo-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```

---

### Task 5: Wire `Codex` into the top-level `install` dispatch

**Files:**
- Modify: `tools/wta/src/agent_hooks_installer.rs` — the public `install` function (search for `install_for_claude(`)

This task makes `wta hooks install` actually call `install_for_codex` for the new `CliKind`.

- [ ] **Step 1: Locate the dispatch**

```powershell
Select-String -Path tools/wta/src/agent_hooks_installer.rs -Pattern "install_for_claude\(" -SimpleMatch
```

The dispatch site lives in a public `install` (or similarly named) function that already calls `install_for_copilot`, `install_for_claude`, and `install_for_gemini`. Note the line number.

- [ ] **Step 2: Write the failing test**

```rust
#[test]
fn install_dispatches_codex() {
    // Smoke: install on an empty HOME shouldn't panic when CliKind::Codex
    // is in CliKind::ALL but ~/.codex doesn't exist. Failures here usually
    // mean a `match cli { ... }` site forgot the Codex arm.
    let tmp = tempfile::tempdir().unwrap();
    install_with_home(tmp.path(), CliScope::One(CliKind::Codex));
}
```

(If the existing test harness uses a different entry point name, use that; search: `Select-String -Path tools/wta/src/agent_hooks_installer.rs -Pattern "fn install_with_home"`.)

- [ ] **Step 3: Add the `Codex` arm**

In the dispatch function, add `CliKind::Codex => install_for_codex(home)` alongside the existing three CLIs.

If the dispatch is a `match` on a single `CliKind`, the arm is one line. If it's an iterator over `CliKind::ALL`, no change is needed beyond Task 1 (the arm comes for free) — verify by re-running the suite.

- [ ] **Step 4: Run the test and full suite**

```powershell
$env:RUSTUP_TOOLCHAIN="stable"
cargo test --manifest-path tools/wta/Cargo.toml
```

Expected: the new test passes; full suite has fewer non-exhaustive-match errors than after Task 1.

- [ ] **Step 5: Commit**

```powershell
git add tools/wta/src/agent_hooks_installer.rs
git commit -m "feat(wta): dispatch CliKind::Codex through hooks install`n`nCo-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```

---

### Task 6: Codex text parsers (`parse_codex_marketplace_list`, `parse_codex_plugin_list`)

**Files:**
- Modify: `tools/wta/src/agent_hooks_installer.rs` — add functions near the existing `parse_copilot_*` (line 1271) and `parse_claude_*` (line 1316) parsers

Sample text from local `codex 0.135.0`:

```
> codex plugin marketplace list
MARKETPLACE      ROOT
openai-curated   https://github.com/openai/codex-marketplace
wt-local         C:\some\path\to\codex

> codex plugin list
PLUGIN            STATUS         VERSION   PATH
github            not installed  -         -
wt-agent-hooks    installed      0.1.0     C:\...\wt-agent-hooks
```

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn parse_codex_marketplace_list_finds_wt_local() {
    let sample = "MARKETPLACE      ROOT\n\
                  openai-curated   https://github.com/openai/codex-marketplace\n\
                  wt-local         C:\\some\\path\\to\\codex\n";
    let (registered, path) = parse_codex_marketplace_list(sample);
    assert!(registered);
    assert_eq!(path.as_deref(), Some("C:\\some\\path\\to\\codex"));
}

#[test]
fn parse_codex_marketplace_list_absent() {
    let sample = "MARKETPLACE      ROOT\n\
                  openai-curated   https://github.com/openai/codex-marketplace\n";
    let (registered, path) = parse_codex_marketplace_list(sample);
    assert!(!registered);
    assert!(path.is_none());
}

#[test]
fn parse_codex_plugin_list_finds_wt_agent_hooks() {
    let sample = "PLUGIN            STATUS         VERSION   PATH\n\
                  github            not installed  -         -\n\
                  wt-agent-hooks    installed      0.1.0     C:\\some\\path\n";
    assert!(parse_codex_plugin_list(sample));
}

#[test]
fn parse_codex_plugin_list_not_installed() {
    let sample = "PLUGIN            STATUS         VERSION   PATH\n\
                  wt-agent-hooks    not installed  -         -\n";
    assert!(!parse_codex_plugin_list(sample));
}

#[test]
fn parse_codex_plugin_list_absent_row() {
    let sample = "PLUGIN            STATUS         VERSION   PATH\n\
                  github            not installed  -         -\n";
    assert!(!parse_codex_plugin_list(sample));
}
```

- [ ] **Step 2: Run tests to verify they fail**

```powershell
$env:RUSTUP_TOOLCHAIN="stable"
cargo test --manifest-path tools/wta/Cargo.toml parse_codex
```

Expected: **FAIL** — `cannot find function parse_codex_marketplace_list`.

- [ ] **Step 3: Implement the parsers**

Add after the `parse_claude_*` block (around line 1370):

```rust
/// Parse `codex plugin marketplace list` plain-text output.
/// Returns `(registered, root_path)` where `registered` is true when a
/// row whose first whitespace-delimited column equals `wt-local`
/// exists, and `root_path` is the remainder of that row trimmed.
fn parse_codex_marketplace_list(stdout: &str) -> (bool, Option<String>) {
    for line in stdout.lines() {
        let line = line.trim_end();
        // Skip header and blank lines.
        if line.is_empty() || line.starts_with("MARKETPLACE") {
            continue;
        }
        let mut split = line.splitn(2, char::is_whitespace);
        let name = match split.next() {
            Some(s) => s.trim(),
            None => continue,
        };
        if name == MARKETPLACE_NAME {
            let rest = split.next().unwrap_or("").trim();
            let path = if rest.is_empty() { None } else { Some(rest.to_string()) };
            return (true, path);
        }
    }
    (false, None)
}

/// Parse `codex plugin list` plain-text output. Returns true when a row
/// for `wt-agent-hooks` exists AND its STATUS column starts with
/// "installed" (not "not installed", "available", etc.).
fn parse_codex_plugin_list(stdout: &str) -> bool {
    for line in stdout.lines() {
        let line = line.trim_end();
        if line.is_empty() || line.starts_with("PLUGIN") {
            continue;
        }
        let mut cols = line.split_whitespace();
        let name = match cols.next() {
            Some(s) => s,
            None => continue,
        };
        if name != PLUGIN_NAME {
            continue;
        }
        let rest: Vec<&str> = cols.collect();
        if rest.is_empty() {
            return false;
        }
        // Status starts at rest[0]. "not installed" → not installed.
        return rest[0] != "not";
    }
    false
}
```

- [ ] **Step 4: Run tests to verify they pass**

```powershell
$env:RUSTUP_TOOLCHAIN="stable"
cargo test --manifest-path tools/wta/Cargo.toml parse_codex
```

Expected: all 5 PASS.

- [ ] **Step 5: Commit**

```powershell
git add tools/wta/src/agent_hooks_installer.rs
git commit -m "feat(wta): parse codex plugin/marketplace list text output`n`nCo-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```

---

### Task 7: `codex_status` + filesystem fallback + dispatch

**Files:**
- Modify: `tools/wta/src/agent_hooks_installer.rs` — add `codex_status` and `codex_fs_fallback` near `claude_status` (line 992), wire into `status` dispatch, update `populate_marketplace_path` if it has per-CLI match arms

**Probe first:** check what `~/.codex/config.toml` looks like after a real install to identify the TOML keys for plugin/marketplace state.

- [ ] **Step 1: Probe `~/.codex/config.toml`**

```powershell
codex plugin marketplace add (Resolve-Path tools\wta\wt-agent-hooks\codex).Path
codex plugin add wt-agent-hooks@wt-local
Get-Content $HOME\.codex\config.toml
```

Note the section names that contain `wt-local` / `wt-agent-hooks`. Most likely candidates:
- `[plugins.marketplaces.wt-local]` table with `path = "..."` and `source = "local"`
- `[plugins.installed.wt-agent-hooks]` or `[[plugins.installed]]` with `marketplace = "wt-local"`

Then clean up:

```powershell
codex plugin remove wt-agent-hooks@wt-local
codex plugin marketplace remove wt-local
```

- [ ] **Step 2: Write the failing tests**

```rust
#[test]
fn codex_status_falls_back_when_binary_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let s = codex_status(false, None, Some(tmp.path()));
    assert_eq!(s.name, "codex");
    assert!(!s.binary_on_path);
    assert_eq!(s.detection_fallback, Some("fs"));
}

#[test]
fn codex_fs_fallback_reads_config_toml() {
    let tmp = tempfile::tempdir().unwrap();
    let codex_dir = tmp.path().join(".codex");
    std::fs::create_dir_all(&codex_dir).unwrap();
    // Adjust to whatever Step-1 probe revealed.
    let toml = r#"
[plugins.marketplaces.wt-local]
source = "local"
path = "C:\\some\\codex\\bundle"

[plugins.installed.wt-agent-hooks]
marketplace = "wt-local"
version = "0.1.0"
"#;
    std::fs::write(codex_dir.join("config.toml"), toml).unwrap();

    let mut s = CliStatus {
        name: "codex",
        binary_on_path: false,
        binary_path: None,
        marketplace_registered: false,
        marketplace_path: None,
        marketplace_path_valid: false,
        plugin_installed: false,
        plugin_enabled: false,
        detection_fallback: None,
    };
    codex_fs_fallback(&mut s, Some(tmp.path()));
    assert!(s.marketplace_registered);
    assert!(s.plugin_installed);
    assert!(s.plugin_enabled); // mirrors plugin_installed for Codex
    assert_eq!(s.detection_fallback, Some("fs"));
}
```

- [ ] **Step 3: Run tests to verify they fail**

```powershell
$env:RUSTUP_TOOLCHAIN="stable"
cargo test --manifest-path tools/wta/Cargo.toml codex_status codex_fs_fallback
```

Expected: **FAIL** — `cannot find function codex_status` / `codex_fs_fallback`.

- [ ] **Step 4: Implement `codex_status` + `codex_fs_fallback`**

Add after `claude_fs_fallback` (around line 1060):

```rust
fn codex_status(on_path: bool, bin_path: Option<String>, home: Option<&Path>) -> CliStatus {
    let mut out = CliStatus {
        name: CliKind::Codex.name(),
        binary_on_path: on_path,
        binary_path: bin_path,
        marketplace_registered: false,
        marketplace_path: None,
        marketplace_path_valid: false,
        plugin_installed: false,
        plugin_enabled: false,
        detection_fallback: None,
    };
    if !on_path {
        codex_fs_fallback(&mut out, home);
        populate_marketplace_path(&mut out, CliKind::Codex, home);
        return out;
    }

    let mkt = match run_plugin_cli_capture("codex", &["plugin", "marketplace", "list"]) {
        Ok(o) if o.success => Some(parse_codex_marketplace_list(&o.stdout)),
        Ok(_) | Err(_) => None,
    };
    let plugin = match run_plugin_cli_capture("codex", &["plugin", "list"]) {
        Ok(o) if o.success => Some(parse_codex_plugin_list(&o.stdout)),
        Ok(_) | Err(_) => None,
    };

    if let (Some((registered, path)), Some(installed)) = (mkt, plugin) {
        out.marketplace_registered = registered;
        if path.is_some() {
            out.marketplace_path = path;
        }
        out.plugin_installed = installed;
        out.plugin_enabled = installed; // Codex has no per-plugin enable flag
    } else {
        codex_fs_fallback(&mut out, home);
    }
    populate_marketplace_path(&mut out, CliKind::Codex, home);
    out
}

fn codex_fs_fallback(out: &mut CliStatus, home: Option<&Path>) {
    out.detection_fallback = Some("fs");
    let Some(home) = home else { return };
    let config_path = home.join(".codex").join("config.toml");
    let text = match fs::read_to_string(&config_path) {
        Ok(t) => t,
        Err(_) => return,
    };

    // Cheap substring match — we don't need full TOML parsing to detect
    // presence, and bringing in a different TOML parser would inflate
    // the dep graph. Adjust the literal patterns to match Step-1 probe.
    let mkt = text.contains("plugins.marketplaces.wt-local")
        || text.contains("[plugins.marketplaces.\"wt-local\"]");
    let plugin = text.contains("plugins.installed.wt-agent-hooks")
        || text.contains("[plugins.installed.\"wt-agent-hooks\"]");
    out.marketplace_registered = mkt;
    out.plugin_installed = plugin;
    out.plugin_enabled = plugin;
}
```

If `populate_marketplace_path` doesn't yet know about `CliKind::Codex`, add a Codex arm to its match (mirror Claude's branch — read marketplace path from `~/.codex/config.toml`).

- [ ] **Step 5: Wire `Codex` into the `status` dispatch**

Find the top-level `status` function (search for `claude_status(`). Add a `CliKind::Codex => codex_status(on_path, bin_path, home)` arm.

- [ ] **Step 6: Run tests + full suite**

```powershell
$env:RUSTUP_TOOLCHAIN="stable"
cargo test --manifest-path tools/wta/Cargo.toml codex_status codex_fs_fallback
cargo test --manifest-path tools/wta/Cargo.toml
```

Expected: new tests PASS. Remaining non-exhaustive match errors decrease.

- [ ] **Step 7: Commit**

```powershell
git add tools/wta/src/agent_hooks_installer.rs
git commit -m "feat(wta): codex_status with CLI + filesystem fallback`n`nCo-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```

---

### Task 8: `uninstall_for_codex` + dispatch

**Files:**
- Modify: `tools/wta/src/agent_hooks_installer.rs` — add after the existing `uninstall_for_claude` and wire into the `uninstall` dispatch

- [ ] **Step 1: Find and read `uninstall_for_claude`**

```powershell
Select-String -Path tools/wta/src/agent_hooks_installer.rs -Pattern "^fn uninstall_for_claude" -Context 0,60
```

Use the function as a template — note how it builds a `CliUninstallResult`, sets `attempted`, sweeps legacy dirs, and returns messages.

- [ ] **Step 2: Write the failing test**

```rust
#[test]
fn uninstall_for_codex_skips_when_home_absent() {
    let tmp = tempfile::tempdir().unwrap();
    let result = uninstall_for_codex(tmp.path());
    assert_eq!(result.name, "codex");
    assert!(!result.attempted);
    assert!(result.plugin_uninstalled.is_none());
    assert!(result.marketplace_removed.is_none());
}
```

- [ ] **Step 3: Run to verify it fails**

```powershell
$env:RUSTUP_TOOLCHAIN="stable"
cargo test --manifest-path tools/wta/Cargo.toml uninstall_for_codex
```

Expected: **FAIL** — `cannot find function uninstall_for_codex`.

- [ ] **Step 4: Implement `uninstall_for_codex`**

Add following the shape of `uninstall_for_claude`:

```rust
fn uninstall_for_codex(home: &Path) -> CliUninstallResult {
    let mut result = CliUninstallResult {
        name: CliKind::Codex.name(),
        attempted: false,
        plugin_uninstalled: None,
        marketplace_removed: None,
        staging_dir_removed: true,
        messages: Vec::new(),
    };

    let codex_dir = home.join(".codex");
    if !codex_dir.is_dir() {
        result.messages.push("skipped: no ~/.codex directory".to_string());
        return result;
    }
    result.attempted = true;

    let plugin_ref = format!("{}@{}", PLUGIN_NAME, MARKETPLACE_NAME);
    let plugin_outcome = run_plugin_cli(
        "codex",
        &["plugin", "remove", &plugin_ref],
        "agent_hooks",
        &["not installed"],
    );
    match plugin_outcome {
        Ok(()) => {
            result.plugin_uninstalled = Some(true);
            result.messages.push("codex plugin remove succeeded".to_string());
        }
        Err(e) => {
            result.plugin_uninstalled = Some(false);
            result.messages.push(format!("codex plugin remove failed: {e}"));
        }
    }

    let mkt_outcome = run_plugin_cli(
        "codex",
        &["plugin", "marketplace", "remove", MARKETPLACE_NAME],
        "agent_hooks",
        &["not registered", "not found"],
    );
    match mkt_outcome {
        Ok(()) => {
            result.marketplace_removed = Some(true);
            result.messages.push("codex plugin marketplace remove succeeded".to_string());
        }
        Err(e) => {
            result.marketplace_removed = Some(false);
            result.messages.push(format!("codex plugin marketplace remove failed: {e}"));
        }
    }

    // Sweep staging dir (mirrors uninstall_for_claude).
    if let Some(root) = crate::runtime_paths::intelligent_terminal_root() {
        let staged = root.join(STAGING_SUBDIR).join(CliKind::Codex.dir_name());
        if staged.is_dir() {
            match fs::remove_dir_all(&staged) {
                Ok(()) => result.messages.push(format!("removed staging dir {}", staged.display())),
                Err(e) => {
                    result.staging_dir_removed = false;
                    result.messages.push(format!("failed to remove staging dir: {e}"));
                }
            }
        }
    }

    result
}
```

- [ ] **Step 5: Wire `Codex` into the `uninstall` dispatch**

Find the top-level `uninstall` function and add a `CliKind::Codex => uninstall_for_codex(home)` arm.

- [ ] **Step 6: Run tests + full suite**

```powershell
$env:RUSTUP_TOOLCHAIN="stable"
cargo test --manifest-path tools/wta/Cargo.toml uninstall_for_codex
cargo test --manifest-path tools/wta/Cargo.toml
```

Expected: full suite passes (no more non-exhaustive match errors). Test count: ~595+.

- [ ] **Step 7: Commit**

```powershell
git add tools/wta/src/agent_hooks_installer.rs
git commit -m "feat(wta): uninstall_for_codex + uninstall dispatch arm`n`nCo-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```

---

### Task 9: Update stale doc comment in `AgentHooksStatus.h`

**Files:**
- Modify: `src/cascadia/inc/AgentHooksStatus.h` line 42

- [ ] **Step 1: Make the edit**

Change line 42 from:
```cpp
        std::string name; // "copilot" | "claude" | "gemini"
```
to:
```cpp
        std::string name; // "copilot" | "claude" | "gemini" | "codex"
```

- [ ] **Step 2: Commit**

```powershell
git add src/cascadia/inc/AgentHooksStatus.h
git commit -m "docs(cascadia): mention codex in CliStatus.name comment`n`nCo-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```

---

### Task 10: Add Codex members to `AIAgentsViewModel` (IDL + header)

**Files:**
- Modify: `src/cascadia/TerminalSettingsEditor/AIAgentsViewModel.idl` (lines 120–137)
- Modify: `src/cascadia/TerminalSettingsEditor/AIAgentsViewModel.h` (lines 142–160)

- [ ] **Step 1: IDL additions**

In `AIAgentsViewModel.idl`, alongside the Copilot/Claude/Gemini triples:

```idl
        String CodexHooksSubtitle { get; };
        Boolean ShowCodexHooksSubtitle { get; };
        void RemoveCodexHooks();
```

Insert each near its counterparts (Codex grouped with Gemini, in the existing visual order in the file).

- [ ] **Step 2: Header additions**

In `AIAgentsViewModel.h`:

```cpp
        winrt::hstring CodexHooksSubtitle() const { return _codexHooksSubtitle; }
        bool ShowCodexHooksSubtitle() const noexcept { return !_codexHooksSubtitle.empty(); }
        void RemoveCodexHooks();
```

And add the private member field (mirroring `_claudeHooksSubtitle`):

```cpp
        winrt::hstring _codexHooksSubtitle;
```

- [ ] **Step 3: Commit**

```powershell
git add src/cascadia/TerminalSettingsEditor/AIAgentsViewModel.idl src/cascadia/TerminalSettingsEditor/AIAgentsViewModel.h
git commit -m "feat(settings): expose CodexHooksSubtitle in AIAgentsViewModel IDL/header`n`nCo-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```

---

### Task 11: Wire Codex into `AIAgentsViewModel.cpp` (status mapping + RemoveCodexHooks + change broadcast)

**Files:**
- Modify: `src/cascadia/TerminalSettingsEditor/AIAgentsViewModel.cpp` (around lines 905–910 for property list; around line 949–970 for Remove methods; plus wherever `_claudeHooksSubtitle` is assigned from the StatusReport JSON)

- [ ] **Step 1: Find the status-mapping site**

```powershell
Select-String -Path src/cascadia/TerminalSettingsEditor/AIAgentsViewModel.cpp -Pattern "_claudeHooksSubtitle\s*=" -Context 0,5
```

The site reads a `CliStatus` row whose `name == "claude"` and formats a subtitle. Mirror it for `"codex"`.

- [ ] **Step 2: Add Codex status mapping**

In the same block that builds `_copilotHooksSubtitle` / `_claudeHooksSubtitle` / `_geminiHooksSubtitle`, append (adapting helper/namespace names to whatever the existing block uses):

```cpp
if (const auto* codex = AgentHooks::FindCli(report, "codex"))
{
    _codexHooksSubtitle = winrt::hstring{ AgentHooks::FormatCliStatusLine(*codex, L"Codex CLI") };
}
else
{
    _codexHooksSubtitle = winrt::hstring{};
}
```

- [ ] **Step 3: Add `RemoveCodexHooks` body**

After `RemoveGeminiHooks` (around line 967), mirror its body:

```cpp
    void AIAgentsViewModel::RemoveCodexHooks()
    {
        if (_installingAgentHooks) return;
        _RemoveAgentHooksForCli(L"codex");
    }
```

(Adjust the helper name to whatever the existing `RemoveClaudeHooks` / `RemoveGeminiHooks` call.)

- [ ] **Step 4: Append to the property-change broadcast list**

At lines 905–910, the broadcast list currently reads:

```cpp
                       L"CopilotHooksSubtitle",
                       L"ClaudeHooksSubtitle",
                       L"GeminiHooksSubtitle",
                       L"ShowCopilotHooksSubtitle",
                       L"ShowClaudeHooksSubtitle",
                       L"ShowGeminiHooksSubtitle");
```

Add Codex entries:

```cpp
                       L"CopilotHooksSubtitle",
                       L"ClaudeHooksSubtitle",
                       L"GeminiHooksSubtitle",
                       L"CodexHooksSubtitle",
                       L"ShowCopilotHooksSubtitle",
                       L"ShowClaudeHooksSubtitle",
                       L"ShowGeminiHooksSubtitle",
                       L"ShowCodexHooksSubtitle");
```

- [ ] **Step 5: Commit**

```powershell
git add src/cascadia/TerminalSettingsEditor/AIAgentsViewModel.cpp
git commit -m "feat(settings): populate Codex hooks subtitle + RemoveCodexHooks`n`nCo-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```

---

### Task 12: Add Codex row to `AIAgents.xaml`

**Files:**
- Modify: `src/cascadia/TerminalSettingsEditor/AIAgents.xaml` (after the Gemini block, around line 346)

- [ ] **Step 1: Find the Gemini block as template**

```powershell
Select-String -Path src/cascadia/TerminalSettingsEditor/AIAgents.xaml -Pattern "GeminiHooksSubtitle" -Context 8,12
```

Copy the full `<Grid>` block that hosts the "Gemini CLI" row (header TextBlock + subtitle TextBlock + Remove button + Install button if present).

- [ ] **Step 2: Paste a Codex copy directly below**

Substitute substrings:
- `Gemini CLI` → `Codex CLI`
- `GeminiHooksSubtitle` → `CodexHooksSubtitle`
- `ShowGeminiHooksSubtitle` → `ShowCodexHooksSubtitle`
- `RemoveGeminiHooks` → `RemoveCodexHooks`

(Plus any `_geminiHooks` install-button bindings that the existing block references — mirror those too if they exist.)

Example block (verify field names match existing pattern):

```xml
                        <Grid Padding="0,8,0,8">
                            <Grid.ColumnDefinitions>
                                <ColumnDefinition Width="*" />
                                <ColumnDefinition Width="Auto" />
                            </Grid.ColumnDefinitions>
                            <StackPanel Grid.Column="0" VerticalAlignment="Center" Spacing="2">
                                <TextBlock Text="Codex CLI" TextWrapping="Wrap" />
                                <TextBlock Text="{x:Bind ViewModel.CodexHooksSubtitle, Mode=OneWay}"
                                           Visibility="{x:Bind ViewModel.ShowCodexHooksSubtitle, Mode=OneWay}"
                                           Style="{StaticResource CaptionTextBlockStyle}"
                                           Opacity="0.6"
                                           TextWrapping="Wrap" />
                            </StackPanel>
                            <Button Grid.Column="1"
                                    Content="Remove hooks"
                                    Click="{x:Bind ViewModel.RemoveCodexHooks}"
                                    IsEnabled="{x:Bind ViewModel.CanInstallAgentHooks, Mode=OneWay}"
                                    MinWidth="120" />
                        </Grid>
```

- [ ] **Step 3: Build CascadiaPackage in Visual Studio**

Open `OpenConsole.sln` in Visual Studio, build the `CascadiaPackage` configuration (`Debug | x64`).

Expected: build succeeds. If IDL/header changes haven't propagated, run `Rebuild` on `Microsoft.Terminal.Settings.Model` first.

- [ ] **Step 4: Commit**

```powershell
git add src/cascadia/TerminalSettingsEditor/AIAgents.xaml
git commit -m "feat(settings): add Codex CLI row to AI Agents page`n`nCo-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
```

---

### Task 13: Final cross-cutting verification

**Files:** none modified (verification only)

- [ ] **Step 1: Full Rust test suite**

```powershell
$env:RUSTUP_TOOLCHAIN="stable"
cd C:\<user>\GitRepo\intelligent-terminal\.worktree\codex-session
cargo test --manifest-path tools/wta/Cargo.toml
```

Expected: **all tests pass**, count is roughly 585 (baseline) + 10 (added in Tasks 1, 3, 4, 6×5, 7×2, 8) = ~595–605.

- [ ] **Step 2: Lint pass**

```powershell
$env:RUSTUP_TOOLCHAIN="stable"
cargo clippy --manifest-path tools/wta/Cargo.toml -- -D warnings
```

Expected: no clippy warnings on new code (existing warnings, if any, are out of scope).

- [ ] **Step 3: CascadiaPackage build**

In Visual Studio: `Build → Build CascadiaPackage (Debug | x64)`.

Expected: build succeeds. Check the AI Agents settings page renders 4 rows.

- [ ] **Step 4: Manual smoke test on a deployed build (if Codex CLI is installed)**

```powershell
wta hooks install
codex   # then run /hooks and trust wt-agent-hooks
# In Codex: submit a prompt
# In WT: press Ctrl+Shift+/ — expect a Codex session row to appear live
wta hooks status
wta hooks uninstall
wta hooks status
```

- [ ] **Step 5: Push and update PR**

```powershell
git push
gh pr view 98 --json title,url
# Update PR body to mention slice B if needed.
```

---

## Self-review

| Spec section | Covered by task |
| --- | --- |
| `CliKind::Codex` enum | Task 1 |
| Bundle directory layout | Task 2 |
| `install_for_codex` | Tasks 4, 5 |
| `maybe_stage_bundle_for_codex` | Task 4 |
| `parse_codex_marketplace_list` / `parse_codex_plugin_list` | Task 6 |
| `codex_status` + filesystem fallback | Task 7 |
| `uninstall_for_codex` | Task 8 |
| Dispatch arms (`install` / `status` / `uninstall`) | Tasks 5, 7, 8 |
| `STATUS_SCHEMA_VERSION` unchanged | (no task — verified by no-bump in any task) |
| `AgentHooksStatus.h` doc-comment fix | Task 9 |
| `AIAgentsViewModel.idl/.h` Codex members | Task 10 |
| `AIAgentsViewModel.cpp` status mapping + RemoveCodexHooks + broadcast | Task 11 |
| `AIAgents.xaml` Codex row | Task 12 |
| Rust unit tests | Tasks 1, 3, 4, 6, 7, 8 |
| Build verification | Tasks 12, 13 |
| Smoke test | Task 13 |
| `CascadiaPackage.wapproj` unchanged | (no task — wapproj's `wt-agent-hooks\**` glob auto-includes codex/) |

**Placeholder scan:** every step has actual code or exact commands. The few items marked "adjust if probe shows different shape" (Task 2 marketplace.json key names, Task 7 TOML key names) are intentional — the probe step in the same task locks them in.

**Type consistency:** `CliKind::Codex`, function names `install_for_codex` / `codex_status` / `codex_fs_fallback` / `uninstall_for_codex` / `parse_codex_marketplace_list` / `parse_codex_plugin_list` / `maybe_stage_bundle_for_codex` used consistently across tasks. `MARKETPLACE_NAME` and `PLUGIN_NAME` constants reused (matching `"wt-local"` / `"wt-agent-hooks"`). C++ identifiers `CodexHooksSubtitle` / `ShowCodexHooksSubtitle` / `RemoveCodexHooks` / `_codexHooksSubtitle` consistent across IDL/H/CPP/XAML.

---

## Execution handoff

Plan complete. Two execution options:

1. **Subagent-Driven (recommended)** — dispatch a fresh subagent per task, review between tasks, fast iteration. Pairs well with the natural commit-per-task cadence.
2. **Inline Execution** — execute tasks in this session using executing-plans, batch execution with checkpoints for review.
