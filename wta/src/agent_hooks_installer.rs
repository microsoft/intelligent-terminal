// wta/src/agent_hooks_installer.rs
//
// Auto-install / status / uninstall the wt-agent-hooks bridge for Claude
// Code, Copilot CLI, and Gemini CLI.
//
// Why this exists
// ===============
//
// The wta agent-pane registry transitions a session out of `IDLE` only when
// it receives `agent_event` broadcasts from the COM server. Those events
// originate from a small PowerShell bridge (`send-event.ps1`) that the
// CLI invokes through its hook system. If the user hasn't run a manual
// plugin-install step, the CLI never invokes the bridge, the registry
// stays empty, and the F2 list looks frozen.
//
// Bundle = single source of truth (issue #20)
// -------------------------------------------
//
// The installable plugin contents live entirely under `wta/wt-agent-hooks/`
// in the repo, in three CLI-specific subtrees:
//
//   wta/wt-agent-hooks/
//     claude/                              <- passed to `claude plugin marketplace add`
//       .claude-plugin/marketplace.json
//       wt-agent-hooks/                    <- the plugin folder Claude copies
//         .claude-plugin/plugin.json
//         hooks/{hooks.json,send-event.ps1}
//     copilot/                             <- passed to `copilot plugin marketplace add`
//       (same shape; only hooks.json differs from claude/ — `-CliSource copilot`)
//     gemini-extension/                    <- passed to `gemini extensions install`
//       gemini-extension.json
//       hooks/{hooks.json,send-event.ps1}
//
// The MSIX package ships this directory next to `wta.exe` (see
// `CascadiaPackage.wapproj`'s `wt-agent-hooks` Content glob), so at runtime
// the installer just hands the per-CLI subdirectory to each CLI's marketplace
// command. No JSON is generated at runtime; no files are materialized into
// `%LOCALAPPDATA%\IntelligentTerminal\<cli>-plugin-src\`; no copies of any
// bundle file are embedded into `wta.exe` via `include_str!`. The bundle on
// disk is the **only** source of truth.
//
// Bundle resolution
// -----------------
//
// At startup, [`bundle::resolve_cli_dir`] walks a short candidate chain:
//
//   1. `WTA_HOOKS_BUNDLE_DIR` env var — explicit override (highest priority,
//      e.g. for distributors patching the bundle without rebuilding wta).
//   2. `<dir-of-current-exe>/wt-agent-hooks/` — where MSIX deposits the
//      bundle next to `wta.exe`.
//   3. Walk parents of `current_exe()` looking for `wta/wt-agent-hooks/` —
//      dev-tree fallback for `cargo build` runs against a checked-out repo.
//
// If none resolve, the installer logs a warning and skips that CLI's install
// step. There is no embedded fallback: a missing bundle next to `wta.exe`
// in a packaged build is a build/deploy bug we want to surface loudly, not
// paper over with a stale baked-in copy.
//
// CLI registration
// ----------------
//
// Each CLI is registered via its own `marketplace add` / `extensions install`
// command — never by editing the CLI's settings/config files directly. Direct
// edits would have to re-serialize JSONC files and would silently strip
// header comments and any unknown user-managed fields.
//
// Per-CLI install flow:
//
//   * Claude:  `claude plugin marketplace add <bundle>/claude`
//              `claude plugin install wt-agent-hooks@wt-local`
//   * Copilot: `copilot plugin marketplace add <bundle>/copilot`
//              `copilot plugin install wt-agent-hooks@wt-local`
//   * Gemini:  `gemini extensions install <bundle>/gemini-extension`
//
// All spawns are best-effort: failures (e.g. `<cli>.exe` not on PATH, or
// "marketplace already added") are logged at warn/info and never crash
// startup.
//
// For Claude specifically: prior wta builds wrote a wta-tagged `hooks` block
// directly into `~/.claude/settings.json`. We strip that legacy block on
// every startup before invoking `claude plugin install` so duplicate hook
// entries don't fire.
//
// Public surface for `wta hooks <action>` (Track 2 / #18)
// -------------------------------------------------------
//
// In addition to the install entry point [`ensure_installed`], this module
// exposes two read-only / best-effort APIs the Settings UI and
// `Verify-AgentHooks.ps1` consume:
//
//   * [`status`] — describe per-CLI install state without writing.
//   * [`uninstall`] — best-effort uninstall for one CLI or all.
//
// Both return JSON-serializable reports with a `schema_version` field so
// downstream consumers can refuse to parse incompatible shapes. See
// [`StatusReport`] / [`UninstallReport`] for the full schema.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// String used to tag every hook entry we manage so we can re-detect them
/// across runs and avoid duplicating entries on each wta launch.
const WTA_TAG: &str = "wt-agent-hooks";

/// Plugin name used in the Claude/Copilot plugin manifest and the
/// `enabledPlugins` map key. Must match `plugin.json`'s `name` field.
const PLUGIN_NAME: &str = "wt-agent-hooks";

/// Marketplace identifier under which our plugin lives. Claude/Copilot CLI
/// require marketplace names to be kebab-case (letters, numbers, hyphens —
/// no underscores). Used as:
///   * Folder name under `installed-plugins/<marketplace>/`.
///   * Key in `extraKnownMarketplaces` in settings.json.
///   * Suffix on `enabledPlugins` map keys (`<plugin>@<marketplace>`).
///
/// Older wta builds used `_direct` here, which Copilot CLI silently rejected
/// as a marketplace name (failing the kebab-case validator), causing the
/// plugin to never load even when the folder existed on disk.
const MARKETPLACE_NAME: &str = "wt-local";

/// Folder name installed under `~/.gemini/extensions/` for Gemini CLI.
const GEMINI_EXTENSION_DIR_NAME: &str = "wt-agent-hooks";

/// Schema version of the JSON returned by [`status`]. Bumped when the shape
/// or the set of possible string-enum values changes.
///
/// v3 (this version): added `marketplace_path` and `marketplace_path_valid`
/// per-CLI fields (#25). `marketplace_registered: true` no longer implies the
/// registered `source.path` actually exists on disk; consumers should consult
/// `marketplace_path_valid` for that.
///
/// v2: `bundle_source.kind` no longer includes `"embedded"` (the embedded
/// `include_str!` fallback was removed in #20). Possible kinds are
/// `env` / `exe-sibling` / `dev-tree` / `none`.
const STATUS_SCHEMA_VERSION: u32 = 3;

/// Schema version of the JSON returned by [`uninstall`].
///
/// v2 (this version): `staging_dir_removed` now describes the sweep of
/// **legacy** staging directories (no longer maintained by current wta) —
/// `%LOCALAPPDATA%\IntelligentTerminal\<cli>-plugin-src\<marketplace>\`
/// from #17, and `%LOCALAPPDATA%\IntelligentTerminal\hook-bundle-fallback\`
/// from the short-lived embedded-fallback materialization in #20. New wta
/// installs never write to either path; uninstall sweeps them so users
/// upgrading from older wta builds don't end up with orphan files.
const UNINSTALL_SCHEMA_VERSION: u32 = 2;

// ---------------------------------------------------------------------------
// Public CLI enum (consumed by `wta hooks --cli=<name>`)
// ---------------------------------------------------------------------------

/// One of the supported agent CLIs. Used as both a routing key (which
/// per-CLI helper to invoke) and as the `name` field in the JSON output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliKind {
    Copilot,
    Claude,
    Gemini,
}

impl CliKind {
    /// Iteration order also dictates the order rows appear in
    /// `wta hooks status` output.
    pub const ALL: &'static [CliKind] = &[CliKind::Copilot, CliKind::Claude, CliKind::Gemini];

    pub fn name(self) -> &'static str {
        match self {
            Self::Copilot => "copilot",
            Self::Claude => "claude",
            Self::Gemini => "gemini",
        }
    }

    pub fn from_name(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "copilot" => Some(Self::Copilot),
            "claude" => Some(Self::Claude),
            "gemini" => Some(Self::Gemini),
            _ => None,
        }
    }

    /// Folder name under `wta/wt-agent-hooks/` that holds this CLI's
    /// installable subtree.
    fn dir_name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Copilot => "copilot",
            Self::Gemini => "gemini-extension",
        }
    }
}

/// Filter for `wta hooks uninstall --cli=...`.
#[derive(Debug, Clone, Copy)]
pub enum CliScope {
    All,
    One(CliKind),
}

impl CliScope {
    fn includes(self, k: CliKind) -> bool {
        match self {
            Self::All => true,
            Self::One(x) => x == k,
        }
    }
}

// ---------------------------------------------------------------------------
// Public JSON-serializable types
// ---------------------------------------------------------------------------

/// Per-CLI install state surfaced by [`status`].
///
/// `binary_on_path`/`binary_path` say whether the CLI itself is
/// installed. The remaining flags describe whether *our* plugin is
/// registered with that CLI. `detection_fallback` is set to `Some("fs")`
/// when the CLI command failed to spawn or returned unparseable output
/// and we used filesystem heuristics instead.
///
/// `marketplace_registered` only attests that the CLI knows about the
/// `wt-local` marketplace by name; it says nothing about whether the
/// registered source path still exists on disk. `marketplace_path` and
/// `marketplace_path_valid` (added in schema v3 / #25) cover that:
///
///   * `marketplace_path` — the `source.path` recorded with the CLI for
///     `directory`-shaped sources. `None` when no entry was found, when the
///     source is `github`-shaped (no local path is meaningful), or when the
///     CLI's source-of-truth file couldn't be read.
///   * `marketplace_path_valid` — `true` when the marketplace entry exists
///     AND its registered location is usable: `directory` sources require
///     `path` to point at an existing directory; `github` sources are always
///     valid (validity isn't local-filesystem-shaped). `false` when no entry
///     was found or the directory has been pruned out from under us
///     (the #21 staleness symptom this field exists to catch).
#[derive(Debug, Clone, Serialize)]
pub struct CliStatus {
    pub name: &'static str,
    pub binary_on_path: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binary_path: Option<String>,
    pub marketplace_registered: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub marketplace_path: Option<String>,
    pub marketplace_path_valid: bool,
    pub plugin_installed: bool,
    pub plugin_enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detection_fallback: Option<&'static str>,
}

/// Top-level shape of `wta hooks status --json`. `bundle_source`
/// reports which entry in the bundle lookup chain supplied the hook
/// files for the running `wta` process — useful when debugging "why is
/// this machine running an old `send-event.ps1`?" support tickets.
#[derive(Debug, Clone, Serialize)]
pub struct StatusReport {
    pub schema_version: u32,
    pub clis: Vec<CliStatus>,
    pub bundle_source: BundleSourceInfo,
}

/// Resolved location of the `wt-agent-hooks/` bundle the running `wta`
/// is using. `kind` is one of `"env" | "exe-sibling" | "dev-tree" | "none"`.
/// `"none"` means no on-disk bundle was resolvable through any of the
/// candidate roots — the installer logs a warning and skips registration
/// in that state.
#[derive(Debug, Clone, Serialize)]
pub struct BundleSourceInfo {
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// Per-CLI outcome of [`uninstall`]. Each of the optional booleans is
/// `Some(true)` when the matching CLI command succeeded, `Some(false)`
/// when it ran but failed, and `None` when we skipped it (e.g. CLI not
/// on PATH so we can't invoke `<cli> plugin uninstall`).
#[derive(Debug, Clone, Serialize)]
pub struct CliUninstallResult {
    pub name: &'static str,
    pub attempted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugin_uninstalled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub marketplace_removed: Option<bool>,
    /// True when every legacy staging directory (#17 LOCALAPPDATA staging
    /// and #20 hook-bundle-fallback materialization) is either absent or
    /// removed successfully. New wta installs don't write to either
    /// location, so this is `true` on a clean machine.
    pub staging_dir_removed: bool,
    pub messages: Vec<String>,
}

/// Top-level shape of `wta hooks uninstall --json`.
#[derive(Debug, Clone, Serialize)]
pub struct UninstallReport {
    pub schema_version: u32,
    pub clis: Vec<CliUninstallResult>,
}

// ---------------------------------------------------------------------------
// Bundle resolver
// ---------------------------------------------------------------------------

mod bundle {
    //! Resolution of the per-CLI bundle directory for hand-off to
    //! `<cli> plugin marketplace add` / `gemini extensions install`.
    //!
    //! Lookup chain (first hit wins):
    //!
    //!   1. `WTA_HOOKS_BUNDLE_DIR` env var — absolute path to a
    //!      `wt-agent-hooks/`-shaped directory (highest priority).
    //!   2. `<dir-of-current-exe>/wt-agent-hooks/` — where the MSIX
    //!      package deposits the loose bundle next to `wta.exe`.
    //!   3. Walk parents of `current_exe()` looking for
    //!      `wta/wt-agent-hooks/` — dev-tree fallback that mirrors
    //!      the walk in `_ResolveWtaExePath` (TerminalSettingsEditor).
    //!
    //! Returns `None` if no on-disk copy is resolvable. The caller is
    //! expected to log a warning and skip that CLI's install step. There
    //! is deliberately no embedded fallback — see the module-level
    //! comment in `agent_hooks_installer.rs` for rationale.

    use super::{BundleSourceInfo, CliKind};
    use std::path::PathBuf;

    /// Resolve the on-disk per-CLI bundle directory. Returns `None` when
    /// no loose copy is found anywhere in the candidate chain; callers
    /// should log + skip in that case.
    pub(super) fn resolve_cli_dir(cli: CliKind) -> Option<PathBuf> {
        let resolved = find_loose_dir(cli, &candidate_roots());
        if let Some(ref path) = resolved {
            tracing::debug!(
                target: "agent_hooks",
                cli = ?cli,
                path = %path.display(),
                "resolved bundle from loose copy",
            );
        }
        resolved
    }

    /// Identify which root in the lookup chain supplied the bundle. Used
    /// by `wta hooks status` to surface the resolved source for support
    /// diagnosis. `kind` is one of `"env" | "exe-sibling" | "dev-tree" |
    /// "none"`.
    pub(super) fn resolve_source() -> BundleSourceInfo {
        // Each candidate is "real" if at least one CLI subtree exists
        // under it — guards against an empty `WTA_HOOKS_BUNDLE_DIR` or a
        // half-populated layout.
        let any_subtree = |root: &std::path::Path| -> bool {
            CliKind::ALL.iter().any(|c| root.join(c.dir_name()).is_dir())
        };

        let env = std::env::var_os("WTA_HOOKS_BUNDLE_DIR")
            .map(PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty());
        if let Some(p) = &env {
            if any_subtree(p) {
                return BundleSourceInfo {
                    kind: "env",
                    path: Some(p.display().to_string()),
                };
            }
        }

        let exe = std::env::current_exe().ok();
        if let Some(exe_dir) = exe.as_ref().and_then(|p| p.parent()) {
            let sib = exe_dir.join("wt-agent-hooks");
            if any_subtree(&sib) {
                return BundleSourceInfo {
                    kind: "exe-sibling",
                    path: Some(sib.display().to_string()),
                };
            }
        }

        if let Some(exe) = exe.as_ref() {
            let mut cursor = exe.parent().map(|p| p.to_path_buf());
            while let Some(dir) = cursor {
                let candidate = dir.join("wta").join("wt-agent-hooks");
                if any_subtree(&candidate) {
                    return BundleSourceInfo {
                        kind: "dev-tree",
                        path: Some(candidate.display().to_string()),
                    };
                }
                let parent = dir.parent().map(|p| p.to_path_buf());
                if parent.as_ref().map(|p| p == &dir).unwrap_or(true) {
                    break;
                }
                cursor = parent;
            }
        }

        BundleSourceInfo {
            kind: "none",
            path: None,
        }
    }

    /// Test seam: separate loose-copy lookup from candidate-root computation
    /// so unit tests can inject a deterministic chain without mutating
    /// process-wide env state.
    pub(super) fn find_loose_dir(cli: CliKind, roots: &[PathBuf]) -> Option<PathBuf> {
        for root in roots {
            let candidate = root.join(cli.dir_name());
            if candidate.is_dir() {
                return Some(candidate);
            }
        }
        None
    }

    /// Resolve candidate roots fresh on every call. The installer only
    /// resolves ~3 directories per run, so the cost (a few `parent()`
    /// hops + `is_dir` stat) is negligible. Computing per-call also keeps
    /// tests honest: a `OnceLock` cache caused races where one test
    /// populated the chain before another test could set
    /// `WTA_HOOKS_BUNDLE_DIR`.
    pub(super) fn candidate_roots() -> Vec<PathBuf> {
        let mut out = Vec::with_capacity(3);

        if let Some(env) = std::env::var_os("WTA_HOOKS_BUNDLE_DIR") {
            let p = PathBuf::from(env);
            if !p.as_os_str().is_empty() {
                out.push(p);
            }
        }

        let exe = std::env::current_exe().ok();
        if let Some(exe_dir) = exe.as_ref().and_then(|p| p.parent()) {
            out.push(exe_dir.join("wt-agent-hooks"));
        }

        if let Some(exe) = exe.as_ref() {
            let mut cursor = exe.parent().map(|p| p.to_path_buf());
            while let Some(dir) = cursor {
                let candidate = dir.join("wta").join("wt-agent-hooks");
                if candidate.is_dir() {
                    out.push(candidate);
                    break;
                }
                let parent = dir.parent().map(|p| p.to_path_buf());
                if parent.as_ref().map(|p| p == &dir).unwrap_or(true) {
                    break;
                }
                cursor = parent;
            }
        }

        out
    }
}

// ---------------------------------------------------------------------------
// Public install entry points
// ---------------------------------------------------------------------------

/// Top-level entry point. Run once at wta startup. Idempotent and silent on
/// failure: if a CLI isn't installed, we skip it; if its settings.json is
/// malformed, we leave it alone.
pub fn ensure_installed() {
    let Some(home) = home_dir() else {
        tracing::debug!(target: "agent_hooks", "no HOME/USERPROFILE; skipping");
        return;
    };
    ensure_installed_in(&home);
}

/// Run the installer against a specific home directory. Split out from
/// [`ensure_installed`] so tests can drive it with an isolated tempdir
/// without mutating `USERPROFILE`/`HOME` for the whole process.
fn ensure_installed_in(home: &Path) {
    install_for_claude(home);
    install_for_copilot(home);
    install_for_gemini(home);
}

// ---------------------------------------------------------------------------
// Per-CLI install flows
// ---------------------------------------------------------------------------

/// Install hooks for Claude Code by spawning `claude plugin install`.
///
/// Always uses Claude Code's own plugin manager — never edits
/// `~/.claude/settings.json` directly. Letting Claude manage its own
/// settings preserves any unknown / user-managed fields the user may
/// have added.
///
/// Steps:
///   1. Strip any wta-tagged top-level `hooks` block left behind by
///      pre-plugin-install wta builds (so duplicate entries don't fire).
///   2. Resolve the static `claude/` bundle directory.
///   3. Spawn `claude plugin marketplace add <bundle>/claude`.
///   4. Spawn `claude plugin install wt-agent-hooks@wt-local`.
fn install_for_claude(home: &Path) {
    let claude_dir = home.join(".claude");
    if !claude_dir.is_dir() {
        tracing::debug!(target: "agent_hooks", "no ~/.claude dir; Claude not present");
        return;
    }

    // Cleanup: prior wta builds merged a tagged `hooks` block directly
    // into ~/.claude/settings.json. Now that we register the plugin via
    // `claude plugin install`, leaving that block in place would fire
    // each event twice — once from settings.json and once from the
    // plugin. Strip our entries on every startup.
    let settings_path = claude_dir.join("settings.json");
    if let Err(e) = cleanup_legacy_claude_hooks(&settings_path) {
        tracing::warn!(
            target: "agent_hooks",
            err = %e,
            path = %settings_path.display(),
            "failed to strip legacy wta hooks from settings.json; non-fatal",
        );
    }

    let bundle_dir = match bundle::resolve_cli_dir(CliKind::Claude) {
        Some(p) => p,
        None => {
            tracing::warn!(
                target: "agent_hooks",
                "no wt-agent-hooks/ bundle found next to wta.exe or in dev tree; \
                 skipping Claude plugin install (set WTA_HOOKS_BUNDLE_DIR to override)",
            );
            return;
        }
    };

    let bundle_path = bundle_dir.to_string_lossy().into_owned();
    if let Err(e) = run_plugin_cli(
        "claude",
        &["plugin", "marketplace", "add", &bundle_path],
        "agent_hooks",
        &[],
    ) {
        tracing::warn!(
            target: "agent_hooks",
            err = %e,
            "claude plugin marketplace add failed; aborting plugin install",
        );
        return;
    }

    let plugin_ref = format!("{}@{}", PLUGIN_NAME, MARKETPLACE_NAME);
    if let Err(e) = run_plugin_cli(
        "claude",
        &["plugin", "install", &plugin_ref],
        "agent_hooks",
        &[],
    ) {
        tracing::warn!(
            target: "agent_hooks",
            err = %e,
            plugin = %plugin_ref,
            "claude plugin install failed",
        );
    }
}

/// Install hooks for Copilot CLI by spawning `copilot plugin install`.
fn install_for_copilot(home: &Path) {
    let copilot_dir = home.join(".copilot");
    if !copilot_dir.is_dir() {
        tracing::debug!(target: "copilot_hooks", "no ~/.copilot dir; Copilot CLI not present");
        return;
    }

    let bundle_dir = match bundle::resolve_cli_dir(CliKind::Copilot) {
        Some(p) => p,
        None => {
            tracing::warn!(
                target: "copilot_hooks",
                "no wt-agent-hooks/ bundle found next to wta.exe or in dev tree; \
                 skipping Copilot plugin install (set WTA_HOOKS_BUNDLE_DIR to override)",
            );
            return;
        }
    };

    // Cleanup (issue #21): pre-staging-refactor wta builds, moved/deleted
    // worktrees, renamed dev clones, or stale `WTA_HOOKS_BUNDLE_DIR` values
    // can all leave an `extraKnownMarketplaces["wt-local"]` entry whose
    // `source.path` no longer matches the bundle we resolved this run.
    // `copilot plugin marketplace add` is silently a no-op when the entry
    // already exists, so without this cleanup the stale path persists
    // forever and the new bundle never registers. Rewrite the path field
    // in place; Copilot's loader uses whatever string lives there.
    let settings_path = copilot_dir.join("settings.json");
    if let Err(e) = cleanup_stale_copilot_marketplace(&settings_path, &bundle_dir) {
        tracing::warn!(
            target: "copilot_hooks",
            err = %e,
            path = %settings_path.display(),
            "failed to clean up stale wt-local marketplace entry; non-fatal",
        );
    }

    let bundle_path = bundle_dir.to_string_lossy().into_owned();
    // copilot plugin marketplace add exits 1 with stderr "Marketplace
    // \"wt-local\" already registered" when re-run — match that
    // substring (per #17's idempotency probe) to keep startup install
    // idempotent. copilot plugin install is already exit-0 idempotent.
    if let Err(e) = run_plugin_cli(
        "copilot",
        &["plugin", "marketplace", "add", &bundle_path],
        "copilot_hooks",
        &["already registered"],
    ) {
        tracing::warn!(
            target: "copilot_hooks",
            err = %e,
            "copilot plugin marketplace add failed; aborting plugin install",
        );
        return;
    }

    let plugin_ref = format!("{}@{}", PLUGIN_NAME, MARKETPLACE_NAME);
    if let Err(e) = run_plugin_cli(
        "copilot",
        &["plugin", "install", &plugin_ref],
        "copilot_hooks",
        &[],
    ) {
        tracing::warn!(
            target: "copilot_hooks",
            err = %e,
            plugin = %plugin_ref,
            "copilot plugin install failed",
        );
        return;
    }

    // Round-7 cleanup: a previous wta wrote files to `_direct/` (which
    // Copilot rejected as an invalid marketplace name). Remove the stale
    // folder so users don't see two copies of the plugin on disk.
    let stale = copilot_dir.join("installed-plugins").join("_direct");
    if stale.is_dir() {
        if let Err(e) = fs::remove_dir_all(&stale) {
            tracing::warn!(
                target: "copilot_hooks",
                err = %e,
                path = %stale.display(),
                "failed to remove stale _direct folder; non-fatal",
            );
        } else {
            tracing::info!(
                target: "copilot_hooks",
                path = %stale.display(),
                "removed stale _direct plugin folder",
            );
        }
    }
}

/// Install hooks for Gemini CLI by spawning `gemini extensions install`.
fn install_for_gemini(home: &Path) {
    let gemini_dir = home.join(".gemini");
    if !gemini_dir.is_dir() {
        tracing::debug!(target: "gemini_hooks", "no ~/.gemini dir; Gemini CLI not present");
        return;
    }

    let bundle_dir = match bundle::resolve_cli_dir(CliKind::Gemini) {
        Some(p) => p,
        None => {
            tracing::warn!(
                target: "gemini_hooks",
                "no wt-agent-hooks/ bundle found next to wta.exe or in dev tree; \
                 skipping Gemini extension install (set WTA_HOOKS_BUNDLE_DIR to override)",
            );
            return;
        }
    };

    let bundle_path = bundle_dir.to_string_lossy().into_owned();
    // `--consent --skip-settings`: defuse Gemini 0.41.2's interactive
    // security-consent and config-on-install prompts. Without them,
    // `gemini extensions install` blocks on stdin and a background
    // install (e.g. from the Settings UI's "Install hooks" button)
    // hangs the timeout. Verified by manual probe in #17.
    //
    // Idempotency: `gemini extensions install` exits 1 with stderr
    // "Extension \"wt-agent-hooks\" is already installed. Please
    // uninstall it first." when the extension is already present.
    // Match on `already installed` to convert that to success.
    if let Err(e) = run_plugin_cli(
        "gemini",
        &[
            "extensions",
            "install",
            &bundle_path,
            "--consent",
            "--skip-settings",
        ],
        "gemini_hooks",
        &["already installed"],
    ) {
        tracing::warn!(
            target: "gemini_hooks",
            err = %e,
            "gemini extensions install failed",
        );
    }
}

// ---------------------------------------------------------------------------
// Public read-only status entry point (Track 2 / #18)
// ---------------------------------------------------------------------------

/// Build a [`StatusReport`] describing the current install state for
/// every supported CLI under the user's home directory. Side-effect
/// free: spawns CLIs in read-only mode and stats files; never writes.
pub fn status() -> StatusReport {
    let home = home_dir();
    StatusReport {
        schema_version: STATUS_SCHEMA_VERSION,
        clis: CliKind::ALL
            .iter()
            .map(|k| status_for(*k, home.as_deref()))
            .collect(),
        bundle_source: bundle::resolve_source(),
    }
}

fn status_for(cli: CliKind, home: Option<&Path>) -> CliStatus {
    let (on_path, bin_path) = locate_binary(cli);
    match cli {
        CliKind::Copilot => copilot_status(on_path, bin_path, home),
        CliKind::Claude => claude_status(on_path, bin_path, home),
        CliKind::Gemini => gemini_status(on_path, bin_path, home),
    }
}

fn locate_binary(cli: CliKind) -> (bool, Option<String>) {
    match which::which(cli.name()) {
        Ok(p) => (true, Some(p.display().to_string())),
        Err(_) => (false, None),
    }
}

fn copilot_status(on_path: bool, bin_path: Option<String>, home: Option<&Path>) -> CliStatus {
    let mut out = CliStatus {
        name: CliKind::Copilot.name(),
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
        // CLI not present — fall back to fs check so we still report
        // install state from a prior run.
        copilot_fs_fallback(&mut out, home);
        populate_marketplace_path(&mut out, CliKind::Copilot, home);
        return out;
    }

    // 1. plugin list (text — Copilot 1.0.44-2 has no --json).
    let plugin_ok = match run_plugin_cli_capture("copilot", &["plugin", "list"]) {
        Ok(o) if o.success => Some(parse_copilot_plugin_list(&o.stdout)),
        Ok(_) | Err(_) => None,
    };
    // 2. marketplace list (text).
    let mkt_ok = match run_plugin_cli_capture("copilot", &["plugin", "marketplace", "list"]) {
        Ok(o) if o.success => Some(parse_copilot_marketplace_list(&o.stdout)),
        Ok(_) | Err(_) => None,
    };

    if let (Some(p), Some(m)) = (plugin_ok, mkt_ok) {
        out.plugin_installed = p;
        // Copilot's `plugin list` doesn't expose enabled/disabled, so
        // "listed" implies enabled. Disabling a plugin removes it.
        out.plugin_enabled = p;
        out.marketplace_registered = m;
    } else {
        copilot_fs_fallback(&mut out, home);
    }

    populate_marketplace_path(&mut out, CliKind::Copilot, home);
    out
}

fn copilot_fs_fallback(out: &mut CliStatus, home: Option<&Path>) {
    out.detection_fallback = Some("fs");
    let Some(home) = home else { return };

    // Source of truth is `~/.copilot/config.json`. The
    // `installed-plugins/<marketplace>/<plugin>/` directory may exist
    // empty (Copilot lazy-populates the cache_path), so a pure
    // file-existence check there gives false negatives. Parse the
    // JSON (Copilot writes JSONC with leading `//` banner comments —
    // strip those before handing to serde_json) and look for our
    // entry.
    let config_path = home.join(".copilot").join("config.json");
    if let Ok(text) = fs::read_to_string(&config_path) {
        let stripped = strip_jsonc_line_comments(&text);
        if let Ok(v) = serde_json::from_str::<Value>(&stripped) {
            if let Some(present) = copilot_config_lookup(&v) {
                out.plugin_installed = present.installed;
                out.plugin_enabled = present.enabled;
                out.marketplace_registered = present.marketplace_registered;
                return;
            }
        }
    }

    // Last-resort heuristic for very old layouts: just check the
    // marketplace folder exists. Not as accurate as the JSON path,
    // but better than reporting a clean "not installed" when the
    // config file is unreadable.
    let marketplace_dir = home
        .join(".copilot")
        .join("installed-plugins")
        .join(MARKETPLACE_NAME);
    let any = marketplace_dir.is_dir();
    out.plugin_installed = any;
    out.plugin_enabled = any;
    out.marketplace_registered = any;
}

/// Inspect `~/.copilot/config.json` for our plugin / marketplace.
///
/// Real shape (Copilot CLI 1.0.44-2):
/// ```jsonc
/// {
///   "installedPlugins": [
///     { "name": "wt-agent-hooks", "marketplace": "wt-local",
///       "version": "0.1.0", "enabled": true,
///       "cache_path": "..." }
///   ],
///   "extraKnownMarketplaces": { "wt-local": { ... } }
/// }
/// ```
///
/// `extraKnownMarketplaces` may be an object keyed by marketplace name
/// or an array — accept either shape so we don't fall over on a future
/// schema change.
fn copilot_config_lookup(v: &Value) -> Option<CopilotConfigState> {
    let plugin = v
        .get("installedPlugins")
        .and_then(|x| x.as_array())
        .into_iter()
        .flatten()
        .find(|e| {
            e.get("name").and_then(|n| n.as_str()) == Some(PLUGIN_NAME)
                && e.get("marketplace").and_then(|n| n.as_str()) == Some(MARKETPLACE_NAME)
        });

    let marketplace_registered = match v.get("extraKnownMarketplaces") {
        Some(Value::Object(map)) => map.contains_key(MARKETPLACE_NAME),
        Some(Value::Array(arr)) => arr
            .iter()
            .any(|e| e.get("name").and_then(|n| n.as_str()) == Some(MARKETPLACE_NAME)),
        _ => false,
    };

    Some(CopilotConfigState {
        installed: plugin.is_some() || marketplace_registered,
        enabled: plugin
            .and_then(|p| p.get("enabled"))
            .and_then(|x| x.as_bool())
            .unwrap_or(plugin.is_some()),
        marketplace_registered: marketplace_registered || plugin.is_some(),
    })
}

#[derive(Debug, Clone, Copy)]
struct CopilotConfigState {
    installed: bool,
    enabled: bool,
    marketplace_registered: bool,
}

/// Strip `//` line comments outside of strings. Copilot CLI's
/// `config.json` is JSONC — it carries a "// User settings belong in
/// settings.json." banner that strict serde_json refuses. This is the
/// minimum normalization needed; we don't try to handle `/* ... */`
/// block comments because Copilot doesn't emit them.
///
/// Tracks an in-string flag so a `//` literal inside a JSON string
/// (e.g. a `"https://..."` URL) isn't accidentally treated as the
/// start of a comment.
fn strip_jsonc_line_comments(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_string = false;
    let mut escaped = false;
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if c == '"' {
            in_string = true;
            out.push(c);
            i += 1;
            continue;
        }
        if c == '/' && i + 1 < chars.len() && chars[i + 1] == '/' {
            // Skip until newline.
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

fn claude_status(on_path: bool, bin_path: Option<String>, home: Option<&Path>) -> CliStatus {
    let mut out = CliStatus {
        name: CliKind::Claude.name(),
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
        claude_fs_fallback(&mut out, home);
        populate_marketplace_path(&mut out, CliKind::Claude, home);
        return out;
    }

    let plugin_json = match run_plugin_cli_capture("claude", &["plugin", "list", "--json"]) {
        Ok(o) if o.success => parse_claude_plugin_list_json(&o.stdout),
        Ok(_) | Err(_) => None,
    };
    let mkt_json =
        match run_plugin_cli_capture("claude", &["plugin", "marketplace", "list", "--json"]) {
            Ok(o) if o.success => parse_claude_marketplace_list_json(&o.stdout),
            Ok(_) | Err(_) => None,
        };

    if let (Some(p), Some(m)) = (plugin_json, mkt_json) {
        out.plugin_installed = p.installed;
        out.plugin_enabled = p.enabled;
        out.marketplace_registered = m;
    } else {
        claude_fs_fallback(&mut out, home);
    }
    populate_marketplace_path(&mut out, CliKind::Claude, home);
    out
}

fn claude_fs_fallback(out: &mut CliStatus, home: Option<&Path>) {
    out.detection_fallback = Some("fs");
    let Some(home) = home else { return };
    // Mirrors AIAgentsViewModel.cpp _IsClaudeHookInstalled: marketplace
    // entry recorded by Claude AND a plugin install dir on disk.
    let known_path = home
        .join(".claude")
        .join("plugins")
        .join("known_marketplaces.json");
    let marketplace_known = fs::read_to_string(&known_path)
        .map(|t| t.contains("\"wt-local\""))
        .unwrap_or(false);
    // Claude copies the plugin into ~/.claude/plugins/cache/<marketplace>/
    // <plugin>/<version>/ at install time; presence of any version dir is
    // a good fs-only "is installed" signal.
    let plugin_cache_root = home
        .join(".claude")
        .join("plugins")
        .join("cache")
        .join(MARKETPLACE_NAME)
        .join(PLUGIN_NAME);
    let plugin_dir_exists = plugin_cache_root
        .read_dir()
        .map(|mut iter| iter.next().is_some())
        .unwrap_or(false);
    let installed = marketplace_known && plugin_dir_exists;
    out.plugin_installed = installed;
    out.plugin_enabled = installed;
    out.marketplace_registered = marketplace_known;
}

fn gemini_status(on_path: bool, bin_path: Option<String>, home: Option<&Path>) -> CliStatus {
    let mut out = CliStatus {
        name: CliKind::Gemini.name(),
        binary_on_path: on_path,
        binary_path: bin_path,
        // Gemini has no marketplace concept — extensions install from
        // path/git directly. Report `true` whenever the extension is
        // installed so the Settings UI can render a uniform row.
        marketplace_registered: false,
        marketplace_path: None,
        marketplace_path_valid: false,
        plugin_installed: false,
        plugin_enabled: false,
        detection_fallback: None,
    };
    if !on_path {
        gemini_fs_fallback(&mut out, home);
        populate_marketplace_path(&mut out, CliKind::Gemini, home);
        return out;
    }

    match run_plugin_cli_capture("gemini", &["extensions", "list", "-o", "json"]) {
        Ok(o) if o.success => {
            // Gemini CLI 0.41.2 emits the JSON payload to **stderr**
            // (with stdout empty). Try stdout first, then stderr — be
            // defensive in case future versions move it back.
            let payload = if !o.stdout.trim().is_empty() {
                &o.stdout
            } else {
                &o.stderr
            };
            if let Some(p) = parse_gemini_extensions_list_json(payload) {
                out.plugin_installed = p.installed;
                out.plugin_enabled = p.enabled;
                out.marketplace_registered = p.installed;
                populate_marketplace_path(&mut out, CliKind::Gemini, home);
                return out;
            }
            gemini_fs_fallback(&mut out, home);
        }
        Ok(_) | Err(_) => gemini_fs_fallback(&mut out, home),
    }
    populate_marketplace_path(&mut out, CliKind::Gemini, home);
    out
}

fn gemini_fs_fallback(out: &mut CliStatus, home: Option<&Path>) {
    out.detection_fallback = Some("fs");
    let Some(home) = home else { return };
    let ext_dir = gemini_extension_dir(home);
    let installed = ext_dir.is_dir() && ext_dir.join("gemini-extension.json").is_file();
    out.plugin_installed = installed;
    out.plugin_enabled = installed;
    out.marketplace_registered = installed;
}

// ---- marketplace-path probe (#25) ------------------------------------------
//
// `marketplace_registered` only attests that the CLI knows about our
// `wt-local` marketplace by name. Issue #25's symptom: when a user removes
// the worktree the Copilot/Claude marketplace was registered against,
// `marketplace_registered` stays `true` (because the entry in
// `extraKnownMarketplaces` / `known_marketplaces.json` is still there)
// while every subsequent `<cli> plugin install` silently fails with
// "source path does not exist".
//
// To let downstream consumers (Settings UI, `Verify-AgentHooks.ps1`)
// detect that drift, we surface:
//
//   * `marketplace_path`        — the registered `source.path`
//   * `marketplace_path_valid`  — `true` when that path is still usable
//
// For `directory`-shaped sources, validity is `Path::is_dir`. For
// `github`-shaped sources (e.g. `superpowers-marketplace`), validity is
// not a local-filesystem property — we report `valid: true, path: None`.
// For Gemini, which has no marketplace concept, the equivalent location
// is the per-extension install directory under `~/.gemini/extensions/`.

/// Resolved marketplace registration info for our `wt-local` marketplace
/// on a given CLI. `path` is the registered local source path (only set
/// for `directory`-shaped sources); `valid` is the path-validity bit
/// described in [`CliStatus::marketplace_path_valid`].
#[derive(Debug, Clone, Default)]
struct MarketplaceInfo {
    path: Option<String>,
    valid: bool,
}

/// Populate `marketplace_path` / `marketplace_path_valid` on `out` from
/// the CLI's on-disk source-of-truth file. Side-effect free; missing /
/// unreadable files leave the defaults (`None` / `false`) in place.
fn populate_marketplace_path(out: &mut CliStatus, cli: CliKind, home: Option<&Path>) {
    let Some(home) = home else { return };
    let info = match cli {
        CliKind::Copilot => copilot_marketplace_info(home),
        CliKind::Claude => claude_marketplace_info(home),
        CliKind::Gemini => gemini_marketplace_info(home),
    };
    out.marketplace_path = info.path;
    out.marketplace_path_valid = info.valid;
}

/// Read `~/.copilot/settings.json` and locate the `wt-local` entry under
/// `extraKnownMarketplaces`. Settings.json is JSONC-tolerant in older
/// Copilot builds, so strip `//` line comments before parsing.
fn copilot_marketplace_info(home: &Path) -> MarketplaceInfo {
    let settings_path = home.join(".copilot").join("settings.json");
    let Ok(text) = fs::read_to_string(&settings_path) else {
        return MarketplaceInfo::default();
    };
    let stripped = strip_jsonc_line_comments(&text);
    let Ok(v) = serde_json::from_str::<Value>(&stripped) else {
        return MarketplaceInfo::default();
    };
    let entry = v
        .get("extraKnownMarketplaces")
        .and_then(|x| x.as_object())
        .and_then(|m| m.get(MARKETPLACE_NAME));
    match entry {
        Some(e) => classify_marketplace_source(e.get("source")),
        None => MarketplaceInfo::default(),
    }
}

/// Read `~/.claude/plugins/known_marketplaces.json` and locate the
/// `wt-local` entry. The file is strict JSON in Claude Code 2.1.x, so
/// no JSONC normalization is needed.
fn claude_marketplace_info(home: &Path) -> MarketplaceInfo {
    let known_path = home
        .join(".claude")
        .join("plugins")
        .join("known_marketplaces.json");
    let Ok(text) = fs::read_to_string(&known_path) else {
        return MarketplaceInfo::default();
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return MarketplaceInfo::default();
    };
    let entry = v.as_object().and_then(|m| m.get(MARKETPLACE_NAME));
    match entry {
        Some(e) => classify_marketplace_source(e.get("source")),
        None => MarketplaceInfo::default(),
    }
}

/// Gemini has no marketplace registry — the `~/.gemini/extensions/wt-agent-hooks/`
/// directory is the install location, the source path, and the validity
/// signal all rolled into one. Report it as the marketplace path so the
/// Settings UI / verify script can render a uniform row across all three
/// CLIs.
fn gemini_marketplace_info(home: &Path) -> MarketplaceInfo {
    let ext_dir = gemini_extension_dir(home);
    if ext_dir.is_dir() {
        MarketplaceInfo {
            path: Some(ext_dir.display().to_string()),
            valid: true,
        }
    } else {
        MarketplaceInfo::default()
    }
}

/// Classify a `source` JSON value (the inner object stored under each
/// marketplace entry's `"source"` key) into a [`MarketplaceInfo`]:
///
///   * `{ "source": "directory", "path": "..." }` — read `path`, validity
///     is `Path::is_dir`.
///   * `{ "source": "github", ... }` — no local path applies; report
///     `valid: true` so consumers don't false-positive a "broken" status.
///   * Unknown / missing `source` kind — registered-but-unknown shape;
///     report `valid: true` so we don't punish forward-compatible source
///     kinds we haven't taught about yet.
///   * `None` — no entry at all; defaults (`None` / `false`).
fn classify_marketplace_source(source: Option<&Value>) -> MarketplaceInfo {
    let Some(source) = source else {
        return MarketplaceInfo::default();
    };
    let kind = source.get("source").and_then(|x| x.as_str()).unwrap_or("");
    match kind {
        "directory" => {
            let path = source
                .get("path")
                .and_then(|x| x.as_str())
                .map(String::from);
            let valid = path
                .as_deref()
                .map(|p| Path::new(p).is_dir())
                .unwrap_or(false);
            MarketplaceInfo { path, valid }
        }
        "github" => MarketplaceInfo {
            path: None,
            valid: true,
        },
        _ => MarketplaceInfo {
            path: None,
            valid: true,
        },
    }
}

// ---- output parsers --------------------------------------------------------

/// Search Copilot's `plugin list` output for our entry. Looks for the
/// substring `wt-agent-hooks@wt-local` anywhere in the output —
/// deliberately ignores the leading bullet character because Node-based
/// CLIs on Windows often emit UTF-8 bytes that get reinterpreted as
/// cp850/cp1252 when stdout is not connected to a TTY (so the real `•`
/// can show up as garbage).
fn parse_copilot_plugin_list(stdout: &str) -> bool {
    let needle = format!("{}@{}", PLUGIN_NAME, MARKETPLACE_NAME);
    stdout.contains(&needle)
}

/// Search for our marketplace name in the `Registered marketplaces:`
/// section. We only consider lines after the section header so the
/// "Included with GitHub Copilot:" preamble (built-in marketplaces we
/// don't own) doesn't produce false positives.
///
/// Encoding-agnostic: matches on `<MARKETPLACE> ` (with a trailing
/// space) so we don't depend on the rendered bullet character.
fn parse_copilot_marketplace_list(stdout: &str) -> bool {
    let mut in_registered = false;
    for l in stdout.lines() {
        let trimmed = l.trim_end();
        if trimmed.contains("Registered marketplaces") {
            in_registered = true;
            continue;
        }
        if !in_registered {
            continue;
        }
        // Look for `<marketplace> (` or `<marketplace>` at end-of-line,
        // anywhere on the line. Avoids depending on the leading bullet.
        let needle_paren = format!("{} (", MARKETPLACE_NAME);
        if trimmed.contains(&needle_paren) || trimmed.ends_with(MARKETPLACE_NAME) {
            return true;
        }
    }
    false
}

#[derive(Debug, Clone, Copy)]
struct PluginPresence {
    installed: bool,
    enabled: bool,
}

/// Parse `claude plugin list --json` output. Returns `None` if the JSON
/// doesn't conform — caller falls back to fs heuristics.
///
/// Sample (Claude 2.1.133):
/// `[{"id":"wt-agent-hooks@wt-local","version":"0.1.0","scope":"user",
///    "enabled":true,"installPath":"...","installedAt":"...",...}]`
fn parse_claude_plugin_list_json(stdout: &str) -> Option<PluginPresence> {
    let v: Value = serde_json::from_str(stdout.trim()).ok()?;
    let arr = v.as_array()?;
    let id_target = format!("{}@{}", PLUGIN_NAME, MARKETPLACE_NAME);
    for entry in arr {
        let id = entry.get("id").and_then(|x| x.as_str()).unwrap_or("");
        if id == id_target {
            let enabled = entry
                .get("enabled")
                .and_then(|x| x.as_bool())
                .unwrap_or(true);
            return Some(PluginPresence {
                installed: true,
                enabled,
            });
        }
    }
    Some(PluginPresence {
        installed: false,
        enabled: false,
    })
}

/// Parse `claude plugin marketplace list --json`. Looks for any entry
/// with `name == "wt-local"`.
fn parse_claude_marketplace_list_json(stdout: &str) -> Option<bool> {
    let v: Value = serde_json::from_str(stdout.trim()).ok()?;
    let arr = v.as_array()?;
    Some(arr.iter().any(|e| {
        e.get("name").and_then(|x| x.as_str()) == Some(MARKETPLACE_NAME)
    }))
}

/// Parse `gemini extensions list -o json`. Looks for our extension by
/// `name`. `enabled` derives from `isActive` (the field gemini surfaces
/// for "is this extension active in the current scope?").
fn parse_gemini_extensions_list_json(stdout: &str) -> Option<PluginPresence> {
    let v: Value = serde_json::from_str(stdout.trim()).ok()?;
    let arr = v.as_array()?;
    for entry in arr {
        let name = entry.get("name").and_then(|x| x.as_str()).unwrap_or("");
        if name == GEMINI_EXTENSION_DIR_NAME {
            let enabled = entry
                .get("isActive")
                .and_then(|x| x.as_bool())
                .unwrap_or(true);
            return Some(PluginPresence {
                installed: true,
                enabled,
            });
        }
    }
    Some(PluginPresence {
        installed: false,
        enabled: false,
    })
}

// ---------------------------------------------------------------------------
// Public uninstall entry point (Track 2 / #18)
// ---------------------------------------------------------------------------

/// Run uninstall against `scope`. Best-effort: every step is logged but
/// failures never abort the run. CLIs not on PATH are recorded with
/// `attempted: false` and a message; legacy staging directories are
/// still swept in the background so we don't leave behind orphan files
/// from older wta builds.
pub fn uninstall(scope: CliScope) -> UninstallReport {
    let home = home_dir();
    UninstallReport {
        schema_version: UNINSTALL_SCHEMA_VERSION,
        clis: CliKind::ALL
            .iter()
            .copied()
            .filter(|k| scope.includes(*k))
            .map(|k| uninstall_for(k, home.as_deref()))
            .collect(),
    }
}

fn uninstall_for(cli: CliKind, home: Option<&Path>) -> CliUninstallResult {
    match cli {
        CliKind::Copilot => copilot_uninstall(home),
        CliKind::Claude => claude_uninstall(home),
        CliKind::Gemini => gemini_uninstall(home),
    }
}

fn copilot_uninstall(_home: Option<&Path>) -> CliUninstallResult {
    let mut out = CliUninstallResult {
        name: CliKind::Copilot.name(),
        attempted: false,
        plugin_uninstalled: None,
        marketplace_removed: None,
        staging_dir_removed: false,
        messages: Vec::new(),
    };
    let plugin_ref = format!("{}@{}", PLUGIN_NAME, MARKETPLACE_NAME);

    if which::which("copilot").is_ok() {
        out.attempted = true;
        out.plugin_uninstalled = Some(spawn_step(
            &mut out.messages,
            "copilot",
            &["plugin", "uninstall", &plugin_ref],
        ));
        // `--force`: marketplace removal would otherwise refuse if
        // anything is still installed under it (e.g. previous step
        // failed). Belt-and-braces.
        out.marketplace_removed = Some(spawn_step(
            &mut out.messages,
            "copilot",
            &["plugin", "marketplace", "remove", MARKETPLACE_NAME, "--force"],
        ));
    } else {
        out.messages
            .push("copilot CLI not on PATH; skipped CLI steps".into());
    }

    out.staging_dir_removed = sweep_legacy_staging_dirs(&mut out.messages, CliKind::Copilot);
    out
}

fn claude_uninstall(home: Option<&Path>) -> CliUninstallResult {
    let mut out = CliUninstallResult {
        name: CliKind::Claude.name(),
        attempted: false,
        plugin_uninstalled: None,
        marketplace_removed: None,
        staging_dir_removed: false,
        messages: Vec::new(),
    };
    let plugin_ref = format!("{}@{}", PLUGIN_NAME, MARKETPLACE_NAME);

    if which::which("claude").is_ok() {
        out.attempted = true;
        out.plugin_uninstalled = Some(spawn_step(
            &mut out.messages,
            "claude",
            &["plugin", "uninstall", &plugin_ref],
        ));
        out.marketplace_removed = Some(spawn_step(
            &mut out.messages,
            "claude",
            &["plugin", "marketplace", "remove", MARKETPLACE_NAME],
        ));
    } else {
        out.messages
            .push("claude CLI not on PATH; skipped CLI steps".into());
    }

    out.staging_dir_removed = sweep_legacy_staging_dirs(&mut out.messages, CliKind::Claude);

    // Belt-and-braces: clean up the legacy hooks block we may have
    // written in pre-plugin-install builds. install_for_claude already
    // does this on every startup, but uninstall should leave nothing
    // behind either way.
    if let Some(home) = home {
        let settings_path = home.join(".claude").join("settings.json");
        if let Err(e) = cleanup_legacy_claude_hooks(&settings_path) {
            out.messages.push(format!(
                "legacy hooks cleanup failed at {}: {}",
                settings_path.display(),
                e,
            ));
        }
    }

    out
}

fn gemini_uninstall(home: Option<&Path>) -> CliUninstallResult {
    let mut out = CliUninstallResult {
        name: CliKind::Gemini.name(),
        attempted: false,
        // Gemini has no marketplace surface.
        plugin_uninstalled: None,
        marketplace_removed: None,
        staging_dir_removed: false,
        messages: Vec::new(),
    };

    let cli_ok = which::which("gemini").is_ok();
    if cli_ok {
        out.attempted = true;
        out.plugin_uninstalled = Some(spawn_step(
            &mut out.messages,
            "gemini",
            &["extensions", "uninstall", GEMINI_EXTENSION_DIR_NAME],
        ));
    } else {
        out.messages
            .push("gemini CLI not on PATH; will remove extension dir directly".into());
    }

    // Whether or not the CLI step succeeded, remove the on-disk dir so
    // we leave no orphan files. Gemini's own uninstall normally does
    // this, so the second sweep is a no-op when the CLI succeeded.
    let mut all_removed = true;
    if let Some(home) = home {
        let ext_dir = gemini_extension_dir(home);
        if ext_dir.exists() {
            match fs::remove_dir_all(&ext_dir) {
                Ok(_) => {
                    out.messages.push(format!("removed {}", ext_dir.display()));
                }
                Err(e) => {
                    all_removed = false;
                    out.messages.push(format!(
                        "failed to remove {}: {}",
                        ext_dir.display(),
                        e,
                    ));
                }
            }
        }
    }

    // Also sweep #17 / #20-style legacy LOCALAPPDATA staging — Gemini
    // never staged there in the current code path, but older wta builds
    // may have if a user upgraded across architectures.
    let legacy_ok = sweep_legacy_staging_dirs(&mut out.messages, CliKind::Gemini);

    out.staging_dir_removed = all_removed && legacy_ok;
    out
}

/// Spawn `<exe>` with `args` and append a one-line summary to
/// `messages`. Returns true on success. Never propagates errors —
/// uninstall is best-effort by design.
fn spawn_step(messages: &mut Vec<String>, exe: &str, args: &[&str]) -> bool {
    match run_plugin_cli_capture(exe, args) {
        Ok(o) if o.success => {
            messages.push(format!("ok: {} {}", exe, args.join(" ")));
            true
        }
        Ok(o) => {
            let combined = if o.stderr.trim().is_empty() {
                o.stdout.trim().to_string()
            } else {
                o.stderr.trim().to_string()
            };
            messages.push(format!(
                "fail ({}): {} {} :: {}",
                o.status_code
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "?".into()),
                exe,
                args.join(" "),
                combined,
            ));
            false
        }
        Err(e) => {
            messages.push(format!("error: {} {} :: {}", exe, args.join(" "), e));
            false
        }
    }
}

/// Path to the Gemini extension directory we install / inspect / remove.
fn gemini_extension_dir(home: &Path) -> PathBuf {
    home.join(".gemini")
        .join("extensions")
        .join(GEMINI_EXTENSION_DIR_NAME)
}

/// Per-CLI legacy staging directories swept by `wta hooks uninstall`.
/// New wta installs never write to any of these; the sweep exists
/// purely so users upgrading from older wta builds end up with a clean
/// disk after `wta hooks uninstall`.
///
///   * `<localappdata>\IntelligentTerminal\<cli>-plugin-src\<marketplace>\`
///     was the staging dir #17 wrote into before invoking
///     `<cli> plugin marketplace add` (Copilot, Claude).
///   * `<localappdata>\IntelligentTerminal\gemini-plugin-src\wt-agent-hooks\`
///     was the equivalent Gemini staging dir added by #17's Gemini
///     plugin-CLI flow.
///   * `<localappdata>\IntelligentTerminal\hook-bundle-fallback\<dir>\`
///     was the embedded-fallback materialization location used in the
///     short-lived first commit of #20 before the embedded fallback
///     was removed entirely.
fn legacy_staging_dirs(cli: CliKind) -> Vec<PathBuf> {
    let Some(root) = crate::runtime_paths::intelligent_terminal_root() else {
        return Vec::new();
    };
    let mut dirs = Vec::new();
    // #17-style per-CLI staging.
    match cli {
        CliKind::Copilot => dirs.push(root.join("copilot-plugin-src").join(MARKETPLACE_NAME)),
        CliKind::Claude => dirs.push(root.join("claude-plugin-src").join(MARKETPLACE_NAME)),
        CliKind::Gemini => dirs.push(
            root.join("gemini-plugin-src")
                .join(GEMINI_EXTENSION_DIR_NAME),
        ),
    }
    // #20-first-commit-style embedded-fallback materialization.
    dirs.push(root.join("hook-bundle-fallback").join(cli.dir_name()));
    dirs
}

/// Sweep every legacy staging directory for `cli`. Returns true when
/// every path is either absent or removed successfully.
fn sweep_legacy_staging_dirs(messages: &mut Vec<String>, cli: CliKind) -> bool {
    let dirs = legacy_staging_dirs(cli);
    if dirs.is_empty() {
        messages.push("could not resolve LOCALAPPDATA; legacy staging dirs untouched".into());
        return false;
    }
    let mut all_clean = true;
    for dir in &dirs {
        if !dir.exists() {
            continue;
        }
        match fs::remove_dir_all(dir) {
            Ok(_) => {
                messages.push(format!("removed legacy staging dir {}", dir.display()));
            }
            Err(e) => {
                all_clean = false;
                messages.push(format!(
                    "failed to remove legacy staging dir {}: {}",
                    dir.display(),
                    e,
                ));
            }
        }
    }
    all_clean
}

// ---------------------------------------------------------------------------
// CLI process spawn helpers
// ---------------------------------------------------------------------------

/// Outcome of spawning a CLI, with stdout/stderr captured for callers
/// that need to parse the output (`wta hooks status`).
#[derive(Debug, Clone)]
struct CliRunOutcome {
    success: bool,
    status_code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// Spawn `<exe>` with the given args, capture stdout/stderr, and trace
/// the result. Never returns Err on non-zero exit — callers inspect
/// `outcome.success` themselves so they can keep parsing partial
/// output (e.g. a `plugin list` that prints rows then warns at the
/// end). Only returns Err when the process couldn't be spawned at all
/// (e.g. CLI not on PATH).
///
/// On Windows, `Command::new("foo")` does **not** consult `PATHEXT`,
/// so `.cmd` / `.bat` shims (which is how every Node-based CLI ships
/// here — `copilot.cmd`, `gemini.cmd`) won't be found by name. We
/// resolve through `which::which` first to get the full path
/// (including the extension) and spawn that.
fn run_plugin_cli_capture(exe: &str, args: &[&str]) -> std::io::Result<CliRunOutcome> {
    use std::process::Stdio;
    let resolved = which::which(exe).ok();
    let mut cmd = match &resolved {
        Some(p) => std::process::Command::new(p),
        None => std::process::Command::new(exe),
    };
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let output = cmd.output()?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    if !output.status.success() {
        tracing::warn!(
            target: "agent_hooks",
            exe = exe,
            args = ?args,
            stdout = %stdout.trim(),
            stderr = %stderr.trim(),
            status = ?output.status.code(),
            "plugin CLI returned non-zero exit",
        );
    } else {
        tracing::info!(
            target: "agent_hooks",
            exe = exe,
            args = ?args,
            stdout = %stdout.trim(),
            "plugin CLI succeeded",
        );
    }
    Ok(CliRunOutcome {
        success: output.status.success(),
        status_code: output.status.code(),
        stdout,
        stderr,
    })
}

/// Thin Err-on-non-zero wrapper around [`run_plugin_cli_capture`] used
/// by the install path, where any failure normally aborts the remaining
/// steps.
///
/// `idempotency_substrings`: lower-cased stdout+stderr snippets that
/// indicate "already in the desired state" — when the spawned CLI exits
/// non-zero AND its captured output contains any of these substrings,
/// we convert the failure to `Ok(())` and log at info!. Wired per-call-
/// site (verified by manual probe in #17, the "Track 1" PR):
///   * `copilot plugin marketplace add`  -> `["already registered"]`
///   * `gemini extensions install`       -> `["already installed"]`
///   * everything else (claude marketplace add / install + copilot
///     plugin install) is already exit-0 idempotent on the CLI side.
fn run_plugin_cli(
    exe: &str,
    args: &[&str],
    _log_target: &str,
    idempotency_substrings: &[&str],
) -> std::io::Result<()> {
    let outcome = run_plugin_cli_capture(exe, args)?;
    if !outcome.success {
        if matches_idempotency_substring(
            &outcome.stdout,
            &outcome.stderr,
            idempotency_substrings,
        ) {
            tracing::info!(
                target: "agent_hooks",
                exe = exe,
                args = ?args,
                "plugin CLI exited non-zero but matched idempotency substring; treating as success",
            );
            return Ok(());
        }
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!(
                "{} {} exited {}",
                exe,
                args.join(" "),
                outcome
                    .status_code
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "?".into()),
            ),
        ));
    }
    Ok(())
}

/// Lower-cased substring search across the captured stdout+stderr for
/// any of `needles`. Returns true on the first hit. Lower-casing both
/// sides keeps the match case-insensitive without per-CLI normalization
/// rules.
fn matches_idempotency_substring(stdout: &str, stderr: &str, needles: &[&str]) -> bool {
    if needles.is_empty() {
        return false;
    }
    let combined = format!("{}\n{}", stdout, stderr).to_ascii_lowercase();
    needles
        .iter()
        .any(|n| combined.contains(&n.to_ascii_lowercase()))
}

/// Return the discovered home directory. Mirrors `history_loader::home_dir`
/// so behavior is consistent between the two modules.
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

// ---------------------------------------------------------------------------
// Legacy settings.json cleanup
// ---------------------------------------------------------------------------

/// Strip wta-tagged entries from the top-level `hooks` block of
/// `~/.claude/settings.json`. Pre-plugin-install wta builds wrote our
/// hook entries directly into settings.json; once the plugin is
/// installed via `claude plugin install`, leaving those entries in
/// place would fire each event twice. Idempotent: no-op if there's
/// nothing to clean.
fn cleanup_legacy_claude_hooks(settings_path: &Path) -> std::io::Result<()> {
    let text = match fs::read_to_string(settings_path) {
        Ok(t) if !t.trim().is_empty() => t,
        Ok(_) => return Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };

    let mut settings: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                target: "agent_hooks",
                err = %e,
                path = %settings_path.display(),
                "settings.json malformed; leaving untouched",
            );
            return Ok(());
        }
    };

    let Some(root) = settings.as_object_mut() else {
        return Ok(());
    };
    let Some(hooks) = root.get_mut("hooks") else {
        return Ok(());
    };
    let Some(hooks_obj) = hooks.as_object_mut() else {
        return Ok(());
    };

    let mut changed = false;
    let event_names: Vec<String> = hooks_obj.keys().cloned().collect();
    for event_name in event_names {
        let Some(arr) = hooks_obj.get_mut(&event_name).and_then(|v| v.as_array_mut()) else {
            continue;
        };
        let before = arr.len();
        arr.retain(|entry| !entry_is_wta_tagged(entry));
        if arr.len() != before {
            changed = true;
        }
        if arr.is_empty() {
            hooks_obj.remove(&event_name);
        }
    }

    // If the hooks object is now empty, remove it entirely so we don't
    // leave behind a `"hooks": {}` artifact in the user's settings.
    if hooks_obj.is_empty() {
        root.remove("hooks");
        changed = true;
    }

    if !changed {
        return Ok(());
    }

    let serialized = serde_json::to_string_pretty(&settings)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    fs::write(settings_path, serialized)?;
    tracing::info!(
        target: "agent_hooks",
        path = %settings_path.display(),
        "stripped legacy wta hooks block",
    );
    Ok(())
}

/// True iff the entry was inserted by us (any nested `command` string
/// references our bridge script or carries the WTA_TAG marker). Used by
/// `cleanup_legacy_claude_hooks` to identify our own entries during
/// migration off the direct-settings.json path.
fn entry_is_wta_tagged(entry: &Value) -> bool {
    let Some(hooks) = entry.get("hooks").and_then(|h| h.as_array()) else {
        return false;
    };
    for h in hooks {
        let Some(cmd) = h.get("command").and_then(|c| c.as_str()) else { continue; };
        if cmd.contains(WTA_TAG) || cmd.contains("send-event.ps1") {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Copilot stale-marketplace cleanup (issue #21)
// ---------------------------------------------------------------------------

/// Rewrite a stale `extraKnownMarketplaces["wt-local"].source.path` in
/// `~/.copilot/settings.json` so it points at the bundle we resolved on
/// this run. Idempotent and best-effort: no-op when the file is missing,
/// the entry is absent, the path already matches, or the JSON is
/// malformed.
///
/// Why this exists
/// ===============
///
/// `copilot plugin marketplace add wt-local <path>` is silently a no-op
/// when an entry named `wt-local` already exists; it does **not**
/// overwrite the `path` field. So if any earlier wta build (or a
/// since-deleted worktree, or a stale `WTA_HOOKS_BUNDLE_DIR`) registered
/// `wt-local` with a now-wrong path, every subsequent `wta install-hooks`
/// will leave the stale path in place and the new bundle never takes
/// effect. This function detects the mismatch and rewrites the path in
/// place. Copilot's loader uses whatever string lives in `source.path`
/// (per the issue #21 verification), so an in-place rewrite is enough —
/// no need to spawn `copilot plugin uninstall` + `marketplace remove`.
///
/// Scope (issue #21 broadened scope from the verification comment)
/// ----------------------------------------------------------------
///
/// We touch only entries whose `source.source == "directory"` (the local
/// bundle case). GitHub-source entries and non-`wt-local` user-managed
/// marketplaces are left alone unconditionally.
///
/// Concrete `~/.copilot/settings.json` shape we care about:
/// ```jsonc
/// {
///   "extraKnownMarketplaces": {
///     "wt-local": {
///       "source": {
///         "source": "directory",
///         "path": "C:\\old\\stale\\path\\copilot"
///       }
///     }
///   }
/// }
/// ```
fn cleanup_stale_copilot_marketplace(
    settings_path: &Path,
    expected_source: &Path,
) -> std::io::Result<()> {
    let text = match fs::read_to_string(settings_path) {
        Ok(t) if !t.trim().is_empty() => t,
        Ok(_) => return Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };

    let mut settings: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                target: "copilot_hooks",
                err = %e,
                path = %settings_path.display(),
                "settings.json malformed; leaving untouched",
            );
            return Ok(());
        }
    };

    let expected_str = expected_source.to_string_lossy().into_owned();
    let old_path: String;

    {
        let Some(root) = settings.as_object_mut() else {
            return Ok(());
        };
        let Some(extra) = root
            .get_mut("extraKnownMarketplaces")
            .and_then(|v| v.as_object_mut())
        else {
            return Ok(());
        };
        let Some(entry) = extra
            .get_mut(MARKETPLACE_NAME)
            .and_then(|v| v.as_object_mut())
        else {
            return Ok(());
        };
        let Some(source) = entry.get_mut("source").and_then(|v| v.as_object_mut()) else {
            return Ok(());
        };

        // Only rewrite local-directory entries; never touch a user-managed
        // GitHub-sourced wt-local override.
        if source.get("source").and_then(|v| v.as_str()) != Some("directory") {
            return Ok(());
        }

        let current = source
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if paths_equivalent(Path::new(&current), expected_source) {
            return Ok(());
        }

        source.insert("path".to_string(), Value::String(expected_str.clone()));
        old_path = current;
    }

    let serialized = serde_json::to_string_pretty(&settings)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    fs::write(settings_path, serialized)?;
    tracing::info!(
        target: "copilot_hooks",
        path = %settings_path.display(),
        old = %old_path,
        new = %expected_str,
        "rewrote stale wt-local marketplace path",
    );
    Ok(())
}

/// Compare two filesystem paths for equivalence. Trailing path
/// separators are normalized away and on Windows the comparison is
/// case-insensitive (ASCII-fold; matches typical NTFS semantics for the
/// kinds of paths we deal with — drive letters, ASCII directory names).
/// We avoid `canonicalize` because the stale path may no longer exist
/// on disk, which is precisely the case we want to detect and rewrite.
fn paths_equivalent(a: &Path, b: &Path) -> bool {
    fn normalize(p: &Path) -> Vec<String> {
        p.components()
            .map(|c| {
                let s = c.as_os_str().to_string_lossy().into_owned();
                if cfg!(windows) {
                    s.to_ascii_lowercase()
                } else {
                    s
                }
            })
            .collect()
    }
    normalize(a) == normalize(b)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn unique_dir(label: &str) -> PathBuf {
        let n = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let p = std::env::temp_dir().join(format!("wta-hooks-{}-{}-{}", label, pid, n));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    // ---- bundle resolver -------------------------------------------------

    /// `bundle::find_loose_dir` returns the per-CLI subdirectory when it
    /// exists under one of the candidate roots. Test exercises the inner
    /// helper directly so we don't have to mutate process-wide env state.
    #[test]
    fn bundle_find_loose_dir_picks_first_match() {
        let root_a = unique_dir("loose-a");
        let root_b = unique_dir("loose-b");
        // Only root_b has the claude/ subtree.
        fs::create_dir_all(root_b.join("claude")).unwrap();

        let roots = vec![root_a.clone(), root_b.clone()];
        let resolved = bundle::find_loose_dir(CliKind::Claude, &roots).expect("found in root_b");
        assert_eq!(resolved, root_b.join("claude"));

        // Nothing for Copilot anywhere → None.
        assert!(bundle::find_loose_dir(CliKind::Copilot, &roots).is_none());
    }

    // ---- bundle content invariants --------------------------------------
    //
    // These tests load the bundle files via `include_str!` at *test* compile
    // time only. The blobs are NOT linked into the production `wta.exe`
    // binary (they live inside a `#[cfg(test)]` module). The runtime install
    // path always reads from the on-disk bundle resolved by
    // `bundle::resolve_cli_dir`.

    const CLAUDE_HOOKS_JSON: &str =
        include_str!("../wt-agent-hooks/claude/wt-agent-hooks/hooks/hooks.json");
    const COPILOT_HOOKS_JSON: &str =
        include_str!("../wt-agent-hooks/copilot/wt-agent-hooks/hooks/hooks.json");
    const GEMINI_HOOKS_JSON: &str =
        include_str!("../wt-agent-hooks/gemini-extension/hooks/hooks.json");

    const CLAUDE_PLUGIN_JSON: &str =
        include_str!("../wt-agent-hooks/claude/wt-agent-hooks/.claude-plugin/plugin.json");
    const COPILOT_PLUGIN_JSON: &str =
        include_str!("../wt-agent-hooks/copilot/wt-agent-hooks/.claude-plugin/plugin.json");

    const CLAUDE_MARKETPLACE_JSON: &str =
        include_str!("../wt-agent-hooks/claude/.claude-plugin/marketplace.json");
    const COPILOT_MARKETPLACE_JSON: &str =
        include_str!("../wt-agent-hooks/copilot/.claude-plugin/marketplace.json");

    const CLAUDE_SEND_EVENT_PS1: &str =
        include_str!("../wt-agent-hooks/claude/wt-agent-hooks/hooks/send-event.ps1");
    const COPILOT_SEND_EVENT_PS1: &str =
        include_str!("../wt-agent-hooks/copilot/wt-agent-hooks/hooks/send-event.ps1");
    const GEMINI_SEND_EVENT_PS1: &str =
        include_str!("../wt-agent-hooks/gemini-extension/hooks/send-event.ps1");

    /// `hooks.json` files must reference `${CLAUDE_PLUGIN_ROOT}` (Claude/
    /// Copilot) or `${extensionPath}` (Gemini), and `send-event.ps1` must
    /// be non-empty in every per-CLI subtree.
    #[test]
    fn bundle_files_are_well_formed() {
        assert!(CLAUDE_HOOKS_JSON.contains("${CLAUDE_PLUGIN_ROOT}"));
        assert!(COPILOT_HOOKS_JSON.contains("${CLAUDE_PLUGIN_ROOT}"));
        assert!(GEMINI_HOOKS_JSON.contains("${extensionPath}"));

        assert!(!CLAUDE_SEND_EVENT_PS1.is_empty());
        assert!(!COPILOT_SEND_EVENT_PS1.is_empty());
        assert!(!GEMINI_SEND_EVENT_PS1.is_empty());
    }

    /// Per-CLI hooks.json files must each contain the expected `-CliSource`
    /// argument so the bridge script tags emitted events with the right CLI.
    #[test]
    fn bundle_hooks_thread_cli_source() {
        assert!(CLAUDE_HOOKS_JSON.contains("-CliSource claude"));
        assert!(!CLAUDE_HOOKS_JSON.contains("-CliSource copilot"));

        assert!(COPILOT_HOOKS_JSON.contains("-CliSource copilot"));
        assert!(!COPILOT_HOOKS_JSON.contains("-CliSource claude"));

        assert!(GEMINI_HOOKS_JSON.contains("-CliSource gemini"));
    }

    /// Claude and Copilot must ship the canonical 10-event Claude-documented
    /// catalog, including `StopFailure` (the Claude-documented event for an
    /// API/network failure) and `PostToolUseFailure`. `ErrorOccurred` must
    /// NOT appear (it was an undocumented name from earlier wta builds; the
    /// documented equivalent is `StopFailure`).
    #[test]
    fn claude_and_copilot_carry_full_event_catalog() {
        const REQUIRED_EVENTS: &[&str] = &[
            "SessionStart",
            "SessionEnd",
            "Notification",
            "UserPromptSubmit",
            "PreToolUse",
            "PostToolUse",
            "PostToolUseFailure",
            "StopFailure",
            "Stop",
            "SubagentStop",
        ];
        for (label, hooks) in [("claude", CLAUDE_HOOKS_JSON), ("copilot", COPILOT_HOOKS_JSON)] {
            for event in REQUIRED_EVENTS {
                assert!(
                    hooks.contains(&format!("\"{event}\":")),
                    "{label} hooks.json missing event {event}"
                );
            }
            assert!(
                !hooks.contains("\"ErrorOccurred\":"),
                "{label} hooks.json still references undocumented ErrorOccurred"
            );
        }
    }

    /// Claude and Copilot share the same hook-event schema; their
    /// `hooks.json` files must be byte-identical except for the
    /// `-CliSource <name>` token. Prevents future drift between the two
    /// per-CLI bundles.
    #[test]
    fn claude_and_copilot_hooks_json_are_parity_identical() {
        let normalized_claude = CLAUDE_HOOKS_JSON.replace("-CliSource claude", "-CliSource <CLI>");
        let normalized_copilot =
            COPILOT_HOOKS_JSON.replace("-CliSource copilot", "-CliSource <CLI>");
        assert_eq!(
            normalized_claude, normalized_copilot,
            "claude/ and copilot/ hooks.json must match modulo -CliSource value"
        );
    }

    /// Claude and Copilot share the same `plugin.json`, `marketplace.json`,
    /// and `send-event.ps1` content; assert byte-equality so future edits
    /// stay in sync.
    #[test]
    fn claude_and_copilot_share_static_manifests() {
        assert_eq!(
            CLAUDE_PLUGIN_JSON, COPILOT_PLUGIN_JSON,
            "claude/ and copilot/ plugin.json must match byte-for-byte"
        );
        assert_eq!(
            CLAUDE_MARKETPLACE_JSON, COPILOT_MARKETPLACE_JSON,
            "claude/ and copilot/ marketplace.json must match byte-for-byte"
        );
        assert_eq!(
            CLAUDE_SEND_EVENT_PS1, COPILOT_SEND_EVENT_PS1,
            "claude/ and copilot/ send-event.ps1 must match byte-for-byte"
        );
    }

    /// `send-event.ps1` is single-source-of-truth across all three CLIs.
    /// (Claude/Copilot byte-equality is covered above; this also pins
    /// Gemini to the same content.)
    #[test]
    fn all_three_cli_send_event_scripts_are_identical() {
        assert_eq!(CLAUDE_SEND_EVENT_PS1, GEMINI_SEND_EVENT_PS1);
    }

    /// `marketplace.json` must declare the `wt-local` marketplace name and
    /// the `wt-agent-hooks` plugin pointing at `./wt-agent-hooks`.
    #[test]
    fn marketplace_json_shape() {
        let v: Value = serde_json::from_str(CLAUDE_MARKETPLACE_JSON).unwrap();
        assert_eq!(v.get("name").and_then(|x| x.as_str()), Some(MARKETPLACE_NAME));
        let plugins = v.get("plugins").and_then(|x| x.as_array()).unwrap();
        assert_eq!(plugins.len(), 1);
        assert_eq!(
            plugins[0].get("name").and_then(|x| x.as_str()),
            Some(PLUGIN_NAME)
        );
        assert_eq!(
            plugins[0].get("source").and_then(|x| x.as_str()),
            Some("./wt-agent-hooks")
        );
    }

    // ---- cleanup_legacy_claude_hooks ------------------------------------

    #[test]
    fn cleanup_legacy_claude_hooks_noop_when_file_missing() {
        let dir = unique_dir("cleanup-missing");
        let path = dir.join("settings.json");
        cleanup_legacy_claude_hooks(&path).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn cleanup_legacy_claude_hooks_removes_wta_entries() {
        let dir = unique_dir("cleanup-removes");
        let path = dir.join("settings.json");
        let before = serde_json::json!({
            "hooks": {
                "SessionStart": [
                    {
                        "matcher": ".*",
                        "hooks": [{
                            "type": "command",
                            "command": "powershell -ExecutionPolicy Bypass -File \"C:\\\\foo\\\\send-event.ps1\" -CliSource claude agent.session.start"
                        }]
                    },
                    {
                        "matcher": ".*",
                        "hooks": [{
                            "type": "command",
                            "command": "echo user-defined hook"
                        }]
                    }
                ]
            },
            "model": "sonnet"
        });
        fs::write(&path, serde_json::to_string_pretty(&before).unwrap()).unwrap();

        cleanup_legacy_claude_hooks(&path).unwrap();

        let after: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        // Unrelated key preserved.
        assert_eq!(after.get("model").and_then(|v| v.as_str()), Some("sonnet"));
        // User-defined hook preserved.
        let arr = after
            .get("hooks")
            .and_then(|h| h.get("SessionStart"))
            .and_then(|v| v.as_array())
            .unwrap();
        assert_eq!(arr.len(), 1);
        let cmd = arr[0]
            .get("hooks")
            .and_then(|h| h.as_array())
            .unwrap()[0]
            .get("command")
            .and_then(|c| c.as_str())
            .unwrap();
        assert_eq!(cmd, "echo user-defined hook");
    }

    #[test]
    fn cleanup_legacy_claude_hooks_strips_empty_hooks_object() {
        let dir = unique_dir("cleanup-empty");
        let path = dir.join("settings.json");
        let before = serde_json::json!({
            "hooks": {
                "SessionStart": [
                    {
                        "matcher": ".*",
                        "hooks": [{
                            "type": "command",
                            "command": "powershell -ExecutionPolicy Bypass -File \"C:\\\\foo\\\\send-event.ps1\" -CliSource claude agent.session.start"
                        }]
                    }
                ]
            }
        });
        fs::write(&path, serde_json::to_string_pretty(&before).unwrap()).unwrap();

        cleanup_legacy_claude_hooks(&path).unwrap();

        let after: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(
            after.get("hooks").is_none(),
            "expected empty hooks object to be removed: {}",
            after
        );
    }

    #[test]
    fn cleanup_legacy_claude_hooks_idempotent_on_clean_file() {
        let dir = unique_dir("cleanup-clean");
        let path = dir.join("settings.json");
        let before = serde_json::json!({ "model": "sonnet" });
        let serialized = serde_json::to_string_pretty(&before).unwrap();
        fs::write(&path, &serialized).unwrap();

        cleanup_legacy_claude_hooks(&path).unwrap();

        // File should not have been rewritten (content identical).
        let after = fs::read_to_string(&path).unwrap();
        assert_eq!(after, serialized);
    }

    #[test]
    fn cleanup_legacy_claude_hooks_skips_malformed_json() {
        let dir = unique_dir("cleanup-malformed");
        let path = dir.join("settings.json");
        fs::write(&path, "{ this is not valid json").unwrap();

        // Must not panic; must not rewrite the file.
        cleanup_legacy_claude_hooks(&path).unwrap();
        let after = fs::read_to_string(&path).unwrap();
        assert_eq!(after, "{ this is not valid json");
    }

    // ---- cleanup_stale_copilot_marketplace (#21) ------------------------
    //
    // Real settings.json shape we rewrite (only `extraKnownMarketplaces`
    // shown for brevity):
    //
    //   "extraKnownMarketplaces": {
    //     "wt-local": {
    //       "source": {
    //         "source": "directory",
    //         "path": "C:\\some\\path\\copilot"
    //       }
    //     }
    //   }

    fn copilot_settings_with(market: Value) -> Value {
        serde_json::json!({
            "askedSetupTerminals": ["windows-terminal"],
            "extraKnownMarketplaces": market,
            "model": "sonnet"
        })
    }

    #[test]
    fn cleanup_stale_copilot_marketplace_noop_when_file_missing() {
        let dir = unique_dir("copilot-cleanup-missing");
        let path = dir.join("settings.json");
        let expected = PathBuf::from("C:\\new\\bundle\\copilot");
        cleanup_stale_copilot_marketplace(&path, &expected).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn cleanup_stale_copilot_marketplace_noop_when_no_entry() {
        let dir = unique_dir("copilot-cleanup-no-entry");
        let path = dir.join("settings.json");
        let before = serde_json::json!({
            "extraKnownMarketplaces": {
                "superpowers-marketplace": {
                    "source": { "source": "github", "repo": "obra/superpowers-marketplace" }
                }
            }
        });
        let serialized = serde_json::to_string_pretty(&before).unwrap();
        fs::write(&path, &serialized).unwrap();

        let expected = PathBuf::from("C:\\new\\bundle\\copilot");
        cleanup_stale_copilot_marketplace(&path, &expected).unwrap();

        // File should not have been rewritten (content identical).
        let after = fs::read_to_string(&path).unwrap();
        assert_eq!(after, serialized);
    }

    /// Round-7 legacy case: stale path is the install destination itself
    /// (`~/.copilot/installed-plugins/wt-local/`). Rewrite must point at
    /// the new bundle source.
    #[test]
    fn cleanup_stale_copilot_marketplace_rewrites_install_destination() {
        let dir = unique_dir("copilot-cleanup-install-dest");
        let path = dir.join("settings.json");
        let before = copilot_settings_with(serde_json::json!({
            "wt-local": {
                "source": {
                    "source": "directory",
                    "path": "C:\\Users\\u\\.copilot\\installed-plugins\\wt-local"
                }
            }
        }));
        fs::write(&path, serde_json::to_string_pretty(&before).unwrap()).unwrap();

        let expected = PathBuf::from("C:\\repo\\wta\\wt-agent-hooks\\copilot");
        cleanup_stale_copilot_marketplace(&path, &expected).unwrap();

        let after: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let new_path = after
            .pointer("/extraKnownMarketplaces/wt-local/source/path")
            .and_then(|v| v.as_str())
            .unwrap();
        assert_eq!(new_path, "C:\\repo\\wta\\wt-agent-hooks\\copilot");
        // Untouched siblings preserved.
        assert_eq!(after.get("model").and_then(|v| v.as_str()), Some("sonnet"));
    }

    /// Verifier's reproduction scenario: stale path is a sibling worktree
    /// directory that was deleted between runs.
    #[test]
    fn cleanup_stale_copilot_marketplace_rewrites_sibling_worktree_path() {
        let dir = unique_dir("copilot-cleanup-sibling");
        let path = dir.join("settings.json");
        let before = copilot_settings_with(serde_json::json!({
            "wt-local": {
                "source": {
                    "source": "directory",
                    "path": "C:\\repo\\.worktree\\track-static-bundle\\wta\\wt-agent-hooks\\copilot"
                }
            }
        }));
        fs::write(&path, serde_json::to_string_pretty(&before).unwrap()).unwrap();

        let expected =
            PathBuf::from("C:\\repo\\.worktree\\track-copilot-cleanup\\wta\\wt-agent-hooks\\copilot");
        cleanup_stale_copilot_marketplace(&path, &expected).unwrap();

        let after: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let new_path = after
            .pointer("/extraKnownMarketplaces/wt-local/source/path")
            .and_then(|v| v.as_str())
            .unwrap();
        assert_eq!(
            new_path,
            "C:\\repo\\.worktree\\track-copilot-cleanup\\wta\\wt-agent-hooks\\copilot"
        );
    }

    /// User-managed entries (other marketplaces, github-source `wt-local`)
    /// must be left exactly as-is.
    #[test]
    fn cleanup_stale_copilot_marketplace_leaves_user_entries_alone() {
        let dir = unique_dir("copilot-cleanup-user");
        let path = dir.join("settings.json");

        // (a) wt-local is a github-source override — must NOT touch.
        let before_a = copilot_settings_with(serde_json::json!({
            "wt-local": {
                "source": { "source": "github", "repo": "someone/wt-local-fork" }
            },
            "superpowers-marketplace": {
                "source": { "source": "github", "repo": "obra/superpowers-marketplace" }
            }
        }));
        let serialized = serde_json::to_string_pretty(&before_a).unwrap();
        fs::write(&path, &serialized).unwrap();

        let expected = PathBuf::from("C:\\repo\\wta\\wt-agent-hooks\\copilot");
        cleanup_stale_copilot_marketplace(&path, &expected).unwrap();

        let after = fs::read_to_string(&path).unwrap();
        assert_eq!(
            after, serialized,
            "github-source wt-local entry must be preserved verbatim"
        );

        // (b) Only some other marketplace exists (no wt-local at all).
        let before_b = copilot_settings_with(serde_json::json!({
            "user-marketplace": {
                "source": { "source": "directory", "path": "C:\\users-stuff" }
            }
        }));
        let serialized_b = serde_json::to_string_pretty(&before_b).unwrap();
        fs::write(&path, &serialized_b).unwrap();

        cleanup_stale_copilot_marketplace(&path, &expected).unwrap();
        let after_b = fs::read_to_string(&path).unwrap();
        assert_eq!(
            after_b, serialized_b,
            "non-wt-local directory entries must be preserved verbatim"
        );
    }

    #[test]
    fn cleanup_stale_copilot_marketplace_idempotent_when_path_matches() {
        let dir = unique_dir("copilot-cleanup-match");
        let path = dir.join("settings.json");

        let expected = PathBuf::from("C:\\repo\\wta\\wt-agent-hooks\\copilot");
        let before = copilot_settings_with(serde_json::json!({
            "wt-local": {
                "source": {
                    "source": "directory",
                    "path": expected.to_string_lossy()
                }
            }
        }));
        let serialized = serde_json::to_string_pretty(&before).unwrap();
        fs::write(&path, &serialized).unwrap();

        cleanup_stale_copilot_marketplace(&path, &expected).unwrap();

        // File must not have been rewritten (content identical).
        let after = fs::read_to_string(&path).unwrap();
        assert_eq!(after, serialized);

        // And on Windows, the comparison is case-insensitive: rewriting
        // the same path with different case should still be a no-op.
        if cfg!(windows) {
            let upper = PathBuf::from("C:\\REPO\\WTA\\WT-AGENT-HOOKS\\COPILOT");
            cleanup_stale_copilot_marketplace(&path, &upper).unwrap();
            let after2 = fs::read_to_string(&path).unwrap();
            assert_eq!(after2, serialized);
        }
    }

    #[test]
    fn cleanup_stale_copilot_marketplace_skips_malformed_json() {
        let dir = unique_dir("copilot-cleanup-malformed");
        let path = dir.join("settings.json");
        fs::write(&path, "{ not valid").unwrap();

        let expected = PathBuf::from("C:\\repo\\wta\\wt-agent-hooks\\copilot");
        // Must not panic; must not rewrite the file.
        cleanup_stale_copilot_marketplace(&path, &expected).unwrap();
        let after = fs::read_to_string(&path).unwrap();
        assert_eq!(after, "{ not valid");
    }

    // ---- status / uninstall parsers (Track 2) ---------------------------

    /// Real `copilot plugin list` output captured 2026-05-08 (Copilot
    /// CLI 1.0.44-2). Asserts our parser finds the wt-agent-hooks
    /// entry by `<plugin>@<marketplace>` prefix.
    #[test]
    fn copilot_plugin_list_parser_finds_our_entry() {
        let stdout = "\
Installed plugins:
  • superpowers@superpowers-marketplace (v5.1.0)
  • wt-agent-hooks@wt-local (v0.1.0)
";
        assert!(parse_copilot_plugin_list(stdout));
    }

    #[test]
    fn copilot_plugin_list_parser_returns_false_when_missing() {
        let stdout = "\
Installed plugins:
  • superpowers@superpowers-marketplace (v5.1.0)
";
        assert!(!parse_copilot_plugin_list(stdout));
    }

    #[test]
    fn copilot_plugin_list_parser_returns_false_when_empty() {
        assert!(!parse_copilot_plugin_list(""));
    }

    /// Real `copilot plugin marketplace list` output. Built-in
    /// marketplaces appear before the "Registered marketplaces:"
    /// header; only entries below that header should count.
    #[test]
    fn copilot_marketplace_list_parser_only_counts_registered() {
        let stdout = "\
✨ Included with GitHub Copilot:
  ◆ copilot-plugins (GitHub: github/copilot-plugins)
  ◆ awesome-copilot (GitHub: github/awesome-copilot)

Registered marketplaces:
  • superpowers-marketplace (GitHub: obra/superpowers-marketplace)
  • wt-local (Local: C:\\Users\\u\\.copilot\\installed-plugins\\wt-local)
";
        assert!(parse_copilot_marketplace_list(stdout));
    }

    #[test]
    fn copilot_marketplace_list_parser_ignores_builtin_only() {
        let stdout = "\
✨ Included with GitHub Copilot:
  ◆ wt-local (GitHub: bogus/wt-local)

Registered marketplaces:
  • superpowers-marketplace (GitHub: obra/superpowers-marketplace)
";
        // wt-local appears in the included list, NOT registered.
        // Parser should refuse to count it.
        assert!(!parse_copilot_marketplace_list(stdout));
    }

    /// Real `claude plugin list --json` output captured 2026-05-08
    /// (Claude Code 2.1.133).
    #[test]
    fn claude_plugin_list_json_parser_extracts_enabled_flag() {
        let stdout = r#"[{"id":"wt-agent-hooks@wt-local","version":"0.1.0","scope":"user","enabled":true,"installPath":"C:\\Users\\u\\.claude\\plugins\\cache\\wt-local\\wt-agent-hooks\\0.1.0","installedAt":"2026-05-08T11:29:58.295Z","lastUpdated":"2026-05-08T11:29:58.295Z"}]"#;
        let p = parse_claude_plugin_list_json(stdout).expect("parses");
        assert!(p.installed);
        assert!(p.enabled);
    }

    #[test]
    fn claude_plugin_list_json_parser_reports_disabled() {
        let stdout = r#"[{"id":"wt-agent-hooks@wt-local","version":"0.1.0","scope":"user","enabled":false}]"#;
        let p = parse_claude_plugin_list_json(stdout).expect("parses");
        assert!(p.installed);
        assert!(!p.enabled);
    }

    #[test]
    fn claude_plugin_list_json_parser_handles_empty_array() {
        let p = parse_claude_plugin_list_json("[]").expect("parses");
        assert!(!p.installed);
        assert!(!p.enabled);
    }

    #[test]
    fn claude_plugin_list_json_parser_returns_none_on_garbage() {
        assert!(parse_claude_plugin_list_json("not json").is_none());
    }

    #[test]
    fn claude_marketplace_list_json_parser_finds_our_marketplace() {
        let stdout = r#"[{"name":"wt-local","source":"...","plugins":[]}]"#;
        assert_eq!(parse_claude_marketplace_list_json(stdout), Some(true));
    }

    #[test]
    fn claude_marketplace_list_json_parser_misses_when_only_others() {
        let stdout = r#"[{"name":"superpowers-marketplace","source":"..."}]"#;
        assert_eq!(parse_claude_marketplace_list_json(stdout), Some(false));
    }

    /// Real `gemini extensions list -o json` output (Gemini 0.41.2).
    #[test]
    fn gemini_extensions_list_json_parser_extracts_active_flag() {
        let stdout = r#"[{"name":"wt-agent-hooks","version":"0.1.0","isActive":true,"path":"..."}]"#;
        let p = parse_gemini_extensions_list_json(stdout).expect("parses");
        assert!(p.installed);
        assert!(p.enabled);
    }

    #[test]
    fn gemini_extensions_list_json_parser_reports_disabled() {
        let stdout = r#"[{"name":"wt-agent-hooks","version":"0.1.0","isActive":false}]"#;
        let p = parse_gemini_extensions_list_json(stdout).expect("parses");
        assert!(p.installed);
        assert!(!p.enabled);
    }

    #[test]
    fn gemini_extensions_list_json_parser_handles_empty_array() {
        let p = parse_gemini_extensions_list_json("[]").expect("parses");
        assert!(!p.installed);
        assert!(!p.enabled);
    }

    // ---- strip_jsonc_line_comments --------------------------------------

    #[test]
    fn strip_jsonc_line_comments_drops_banner() {
        let input = "// header\n// second line\n{\"a\":1}\n";
        let out = strip_jsonc_line_comments(input);
        let v: Value = serde_json::from_str(&out).expect("parses");
        assert_eq!(v.get("a").and_then(|x| x.as_i64()), Some(1));
    }

    #[test]
    fn strip_jsonc_line_comments_preserves_url_in_string() {
        // // inside a JSON string literal must not be interpreted as a comment.
        let input = "{\"url\":\"https://example.com/a/b\"}\n";
        let out = strip_jsonc_line_comments(input);
        assert_eq!(out, input);
    }

    // ---- copilot_config_lookup ------------------------------------------

    #[test]
    fn copilot_config_lookup_finds_installed_plugin() {
        let v: Value = serde_json::from_str(
            r#"{
                "installedPlugins": [
                    {"name":"wt-agent-hooks","marketplace":"wt-local","enabled":true}
                ],
                "extraKnownMarketplaces": {"wt-local": {}}
            }"#,
        )
        .unwrap();
        let s = copilot_config_lookup(&v).unwrap();
        assert!(s.installed);
        assert!(s.enabled);
        assert!(s.marketplace_registered);
    }

    #[test]
    fn copilot_config_lookup_handles_disabled_plugin() {
        let v: Value = serde_json::from_str(
            r#"{
                "installedPlugins": [
                    {"name":"wt-agent-hooks","marketplace":"wt-local","enabled":false}
                ],
                "extraKnownMarketplaces": {"wt-local": {}}
            }"#,
        )
        .unwrap();
        let s = copilot_config_lookup(&v).unwrap();
        assert!(s.installed);
        assert!(!s.enabled);
    }

    // ---- bundle::resolve_source -----------------------------------------

    /// `bundle::resolve_source` returns `kind: "none"` when nothing is on
    /// disk and the env override is unset.
    #[test]
    fn bundle_resolve_source_returns_none_when_nothing_resolves() {
        // Save & clear WTA_HOOKS_BUNDLE_DIR so the test doesn't pick up
        // the dev tree's bundle via a leftover env var.
        let saved = std::env::var_os("WTA_HOOKS_BUNDLE_DIR");
        // SAFETY: tests run with --test-threads=1 in CI, but even without
        // serialization, every other test that touches this env var
        // restores it; collisions would manifest as flakes here, not data
        // corruption. We accept the small risk.
        unsafe {
            std::env::set_var(
                "WTA_HOOKS_BUNDLE_DIR",
                "C:/this/path/definitely/does/not/exist",
            );
        }

        // The exe-sibling and dev-tree probes will still fire. In a
        // cargo-test environment exe-dir is `target/debug/deps/`, so
        // `<exe-dir>/wt-agent-hooks/` won't exist; the parent walk will
        // find `<repo>/wta/wt-agent-hooks/` though, so this asserts the
        // dev-tree path wins (we deliberately don't assert "none" here
        // because the dev tree IS resolvable — we just check that the
        // env path didn't trip the false-positive).
        let info = bundle::resolve_source();
        assert_ne!(info.kind, "env", "non-existent env path must not match");

        // Restore.
        unsafe {
            match saved {
                Some(v) => std::env::set_var("WTA_HOOKS_BUNDLE_DIR", v),
                None => std::env::remove_var("WTA_HOOKS_BUNDLE_DIR"),
            }
        }
    }

    /// Schema versions are stable contracts with the Settings UI and
    /// `Verify-AgentHooks.ps1`. Bumping them requires a coordinated
    /// downstream update — pin them here so a careless change shows up
    /// as a test failure.
    #[test]
    fn schema_versions_are_pinned() {
        assert_eq!(STATUS_SCHEMA_VERSION, 3);
        assert_eq!(UNINSTALL_SCHEMA_VERSION, 2);
    }

    // ---- run_plugin_cli idempotency (#17) -------------------------------

    #[test]
    fn idempotency_substring_matches_in_stderr() {
        assert!(matches_idempotency_substring(
            "",
            "Marketplace \"wt-local\" already registered",
            &["already registered"],
        ));
    }

    #[test]
    fn idempotency_substring_matches_in_stdout() {
        assert!(matches_idempotency_substring(
            "Extension \"wt-agent-hooks\" is already installed.",
            "",
            &["already installed"],
        ));
    }

    #[test]
    fn idempotency_substring_is_case_insensitive() {
        assert!(matches_idempotency_substring(
            "ALREADY INSTALLED",
            "",
            &["already installed"],
        ));
    }

    #[test]
    fn idempotency_substring_returns_false_with_empty_needles() {
        assert!(!matches_idempotency_substring(
            "already registered",
            "",
            &[],
        ));
    }

    #[test]
    fn idempotency_substring_returns_false_when_no_match() {
        assert!(!matches_idempotency_substring(
            "some unrelated error",
            "more unrelated noise",
            &["already registered", "already installed"],
        ));
    }

    #[test]
    fn idempotency_substring_matches_any_needle() {
        assert!(matches_idempotency_substring(
            "Extension \"wt-agent-hooks\" is already installed.",
            "",
            &["already registered", "already installed"],
        ));
    }

    // ---- marketplace path validity (#25) --------------------------------

    /// `directory`-shaped source with an existing path → reports the path
    /// and `valid: true`.
    #[test]
    fn classify_marketplace_source_directory_existing_path() {
        let dir = unique_dir("classify-dir-ok");
        let v = serde_json::json!({
            "source": "directory",
            "path": dir.display().to_string(),
        });
        let info = classify_marketplace_source(Some(&v));
        assert_eq!(info.path.as_deref(), Some(dir.display().to_string().as_str()));
        assert!(info.valid);
    }

    /// `directory`-shaped source with a now-missing path → reports the
    /// path (so consumers can show what went stale) but `valid: false`.
    /// This is the exact #25 symptom.
    #[test]
    fn classify_marketplace_source_directory_missing_path() {
        let dir = unique_dir("classify-dir-stale");
        let stale = dir.join("does-not-exist");
        let v = serde_json::json!({
            "source": "directory",
            "path": stale.display().to_string(),
        });
        let info = classify_marketplace_source(Some(&v));
        assert_eq!(
            info.path.as_deref(),
            Some(stale.display().to_string().as_str())
        );
        assert!(!info.valid, "missing dir must report invalid");
    }

    /// `directory`-shaped source with no `path` key → can't validate;
    /// report `valid: false` with `path: None`.
    #[test]
    fn classify_marketplace_source_directory_without_path_field() {
        let v = serde_json::json!({ "source": "directory" });
        let info = classify_marketplace_source(Some(&v));
        assert!(info.path.is_none());
        assert!(!info.valid);
    }

    /// `github`-shaped source → no local path applies; valid by definition.
    #[test]
    fn classify_marketplace_source_github_is_always_valid() {
        let v = serde_json::json!({
            "source": "github",
            "repo": "owner/repo",
        });
        let info = classify_marketplace_source(Some(&v));
        assert!(info.path.is_none());
        assert!(info.valid);
    }

    /// Unknown / forward-compatible `source` kind → don't false-positive
    /// a "broken" status; report valid.
    #[test]
    fn classify_marketplace_source_unknown_kind_is_valid() {
        let v = serde_json::json!({ "source": "ipfs", "cid": "..." });
        let info = classify_marketplace_source(Some(&v));
        assert!(info.path.is_none());
        assert!(info.valid);
    }

    /// `None` source value → no entry; report defaults.
    #[test]
    fn classify_marketplace_source_none_returns_defaults() {
        let info = classify_marketplace_source(None);
        assert!(info.path.is_none());
        assert!(!info.valid);
    }

    /// `copilot_marketplace_info` reads `~/.copilot/settings.json`,
    /// strips the JSONC banner, and surfaces the registered directory
    /// path + validity. Mirrors the real on-disk shape from a working
    /// install (see `~/.copilot/settings.json` schema).
    #[test]
    fn copilot_marketplace_info_directory_path_is_validated() {
        let home = unique_dir("copilot-mkt-ok");
        let copilot_dir = home.join(".copilot");
        fs::create_dir_all(&copilot_dir).unwrap();
        let bundle = unique_dir("copilot-mkt-bundle");
        let settings = serde_json::json!({
            "extraKnownMarketplaces": {
                MARKETPLACE_NAME: {
                    "source": {
                        "source": "directory",
                        "path": bundle.display().to_string(),
                    }
                }
            }
        });
        let body = format!(
            "// User settings belong in settings.json.\n{}\n",
            serde_json::to_string_pretty(&settings).unwrap()
        );
        fs::write(copilot_dir.join("settings.json"), body).unwrap();

        let info = copilot_marketplace_info(&home);
        assert_eq!(
            info.path.as_deref(),
            Some(bundle.display().to_string().as_str())
        );
        assert!(info.valid);
    }

    /// #25 reproduction: settings.json points at a now-pruned worktree —
    /// `marketplace_path` still surfaces the stale path so consumers can
    /// display it, `valid` is `false`.
    #[test]
    fn copilot_marketplace_info_reports_stale_directory() {
        let home = unique_dir("copilot-mkt-stale");
        let copilot_dir = home.join(".copilot");
        fs::create_dir_all(&copilot_dir).unwrap();
        let stale = home.join("pruned-worktree-dir");
        let settings = serde_json::json!({
            "extraKnownMarketplaces": {
                MARKETPLACE_NAME: {
                    "source": {
                        "source": "directory",
                        "path": stale.display().to_string(),
                    }
                }
            }
        });
        fs::write(
            copilot_dir.join("settings.json"),
            serde_json::to_string_pretty(&settings).unwrap(),
        )
        .unwrap();

        let info = copilot_marketplace_info(&home);
        assert_eq!(
            info.path.as_deref(),
            Some(stale.display().to_string().as_str())
        );
        assert!(!info.valid);
    }

    /// No settings.json on disk → defaults (no entry).
    #[test]
    fn copilot_marketplace_info_missing_file_defaults() {
        let home = unique_dir("copilot-mkt-missing");
        let info = copilot_marketplace_info(&home);
        assert!(info.path.is_none());
        assert!(!info.valid);
    }

    /// settings.json present but no `wt-local` entry → defaults.
    #[test]
    fn copilot_marketplace_info_no_wt_local_entry() {
        let home = unique_dir("copilot-mkt-no-entry");
        let copilot_dir = home.join(".copilot");
        fs::create_dir_all(&copilot_dir).unwrap();
        let settings = serde_json::json!({
            "extraKnownMarketplaces": {
                "superpowers-marketplace": {
                    "source": { "source": "github", "repo": "obra/superpowers-marketplace" }
                }
            }
        });
        fs::write(
            copilot_dir.join("settings.json"),
            serde_json::to_string_pretty(&settings).unwrap(),
        )
        .unwrap();

        let info = copilot_marketplace_info(&home);
        assert!(info.path.is_none());
        assert!(!info.valid);
    }

    /// `claude_marketplace_info` reads `known_marketplaces.json` (which is
    /// strict JSON, no JSONC banner) and surfaces the registered directory
    /// path + validity.
    #[test]
    fn claude_marketplace_info_directory_path_is_validated() {
        let home = unique_dir("claude-mkt-ok");
        let plugins_dir = home.join(".claude").join("plugins");
        fs::create_dir_all(&plugins_dir).unwrap();
        let bundle = unique_dir("claude-mkt-bundle");
        let known = serde_json::json!({
            MARKETPLACE_NAME: {
                "source": {
                    "source": "directory",
                    "path": bundle.display().to_string(),
                },
                "installLocation": bundle.display().to_string(),
            }
        });
        fs::write(
            plugins_dir.join("known_marketplaces.json"),
            serde_json::to_string_pretty(&known).unwrap(),
        )
        .unwrap();

        let info = claude_marketplace_info(&home);
        assert_eq!(
            info.path.as_deref(),
            Some(bundle.display().to_string().as_str())
        );
        assert!(info.valid);
    }

    /// Claude github-shaped marketplace (e.g. `claude-plugins-official`) →
    /// no path, always valid.
    #[test]
    fn claude_marketplace_info_github_source_is_valid_no_path() {
        let home = unique_dir("claude-mkt-github");
        let plugins_dir = home.join(".claude").join("plugins");
        fs::create_dir_all(&plugins_dir).unwrap();
        let known = serde_json::json!({
            MARKETPLACE_NAME: {
                "source": { "source": "github", "repo": "owner/repo" }
            }
        });
        fs::write(
            plugins_dir.join("known_marketplaces.json"),
            serde_json::to_string_pretty(&known).unwrap(),
        )
        .unwrap();

        let info = claude_marketplace_info(&home);
        assert!(info.path.is_none());
        assert!(info.valid);
    }

    #[test]
    fn claude_marketplace_info_missing_file_defaults() {
        let home = unique_dir("claude-mkt-missing");
        let info = claude_marketplace_info(&home);
        assert!(info.path.is_none());
        assert!(!info.valid);
    }

    /// `gemini_marketplace_info` reports the install dir as the
    /// "marketplace path" since Gemini has no marketplace registry.
    #[test]
    fn gemini_marketplace_info_uses_install_dir_when_present() {
        let home = unique_dir("gemini-mkt-ok");
        let ext_dir = gemini_extension_dir(&home);
        fs::create_dir_all(&ext_dir).unwrap();

        let info = gemini_marketplace_info(&home);
        assert_eq!(
            info.path.as_deref(),
            Some(ext_dir.display().to_string().as_str())
        );
        assert!(info.valid);
    }

    #[test]
    fn gemini_marketplace_info_missing_dir_defaults() {
        let home = unique_dir("gemini-mkt-missing");
        let info = gemini_marketplace_info(&home);
        assert!(info.path.is_none());
        assert!(!info.valid);
    }

    /// `populate_marketplace_path` is a no-op when `home` is `None`
    /// (e.g. `USERPROFILE` unset on a service account).
    #[test]
    fn populate_marketplace_path_noop_without_home() {
        let mut s = CliStatus {
            name: "copilot",
            binary_on_path: false,
            binary_path: None,
            marketplace_registered: false,
            marketplace_path: None,
            marketplace_path_valid: false,
            plugin_installed: false,
            plugin_enabled: false,
            detection_fallback: None,
        };
        populate_marketplace_path(&mut s, CliKind::Copilot, None);
        assert!(s.marketplace_path.is_none());
        assert!(!s.marketplace_path_valid);
    }

    /// End-to-end: a freshly-built `CliStatus` carries the new fields with
    /// safe defaults so consumers parsing schema v3 always see them.
    #[test]
    fn cli_status_serializes_new_fields() {
        let s = CliStatus {
            name: "copilot",
            binary_on_path: true,
            binary_path: Some("C:/x/copilot.exe".into()),
            marketplace_registered: true,
            marketplace_path: Some("C:/repo/wt-agent-hooks/copilot".into()),
            marketplace_path_valid: true,
            plugin_installed: true,
            plugin_enabled: true,
            detection_fallback: None,
        };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(
            v.get("marketplace_path").and_then(|x| x.as_str()),
            Some("C:/repo/wt-agent-hooks/copilot")
        );
        assert_eq!(
            v.get("marketplace_path_valid").and_then(|x| x.as_bool()),
            Some(true)
        );

        // marketplace_path: None must serialize to absent, not null,
        // so v2 consumers parsing v3 output don't see a surprise null.
        let s_no_path = CliStatus {
            marketplace_path: None,
            ..s
        };
        let v2 = serde_json::to_value(&s_no_path).unwrap();
        assert!(v2.get("marketplace_path").is_none());
        // marketplace_path_valid is always present (it's a bool, not Option).
        assert!(v2.get("marketplace_path_valid").is_some());
    }
}
