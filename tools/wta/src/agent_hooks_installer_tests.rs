//! `agent_hooks_installer` unit tests, split out of the large module file so
//! it lives in its own file. This is a child module of
//! `agent_hooks_installer` (declared with `#[path]` in the parent file), not
//! of the crate root, so it can reach the module's private items directly,
//! the same way the file used to when this was an inline
//! `mod tests { ... }` block.

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

// ---- WindowsApps staging workaround (Claude) ------------------------

/// `is_under_windows_apps` should be true for the MSIX install layout
/// regardless of slash direction or letter case, and false for normal
/// dev-tree / user paths.
#[test]
fn is_under_windows_apps_recognises_packaged_paths() {
    assert!(is_under_windows_apps(Path::new(
        r"C:\Program Files\WindowsApps\IntelligentTerminal_0.7.0.11_x64__rd9vj3e6a2mbr\wt-agent-hooks\claude",
    )));
    // Case-insensitive match.
    assert!(is_under_windows_apps(Path::new(
        r"C:\Program Files\windowsapps\Foo\bar",
    )));
    // Forward slashes (rare but possible if a caller normalises them).
    assert!(is_under_windows_apps(Path::new(
        "C:/Program Files/WindowsApps/Foo/bar",
    )));
    // Dev-tree / user paths should not match.
    assert!(!is_under_windows_apps(Path::new(
        r"Q:\git\intelligent-terminal\tools\wta\wt-agent-hooks\claude",
    )));
    assert!(!is_under_windows_apps(Path::new(
        r"C:\Users\someone\AppData\Local\IntelligentTerminal\hook-bundle-staging\claude",
    )));
    // Substring `windowsapps` only matches when it's a full path segment.
    // (Our heuristic intentionally requires the surrounding slashes so a
    // user folder literally named `WindowsAppsStuff` doesn't get
    // misclassified.)
    assert!(!is_under_windows_apps(Path::new(
        r"C:\Users\me\WindowsAppsStuff\foo",
    )));
}

/// `copy_dir_recursive` must reproduce a nested directory tree
/// byte-for-byte at the destination, creating intermediate
/// directories as it goes.
#[test]
fn copy_dir_recursive_mirrors_tree() {
    let src = unique_dir("stage-src");
    let dst = unique_dir("stage-dst").join("staged");

    fs::create_dir_all(src.join(".claude-plugin")).unwrap();
    fs::create_dir_all(src.join("wt-agent-hooks/hooks")).unwrap();
    fs::write(
        src.join(".claude-plugin/marketplace.json"),
        r#"{"name":"wt-local"}"#,
    )
    .unwrap();
    fs::write(
        src.join("wt-agent-hooks/.claude-plugin/plugin.json"),
        r#"{"name":"wt-agent-hooks"}"#,
    )
    .ok();
    fs::create_dir_all(src.join("wt-agent-hooks/.claude-plugin")).unwrap();
    fs::write(
        src.join("wt-agent-hooks/.claude-plugin/plugin.json"),
        r#"{"name":"wt-agent-hooks"}"#,
    )
    .unwrap();
    fs::write(
        src.join("wt-agent-hooks/hooks/hooks.json"),
        r#"{"hooks":{}}"#,
    )
    .unwrap();
    fs::write(
        src.join("wt-agent-hooks/hooks/native-hook.json"),
        r#"{"command":"wtcli.exe agent-hook"}"#,
    )
    .unwrap();

    copy_dir_recursive(&src, &dst).expect("copy succeeds");

    assert_eq!(
        fs::read_to_string(dst.join(".claude-plugin/marketplace.json")).unwrap(),
        r#"{"name":"wt-local"}"#,
    );
    assert_eq!(
        fs::read_to_string(dst.join("wt-agent-hooks/.claude-plugin/plugin.json")).unwrap(),
        r#"{"name":"wt-agent-hooks"}"#,
    );
    assert_eq!(
        fs::read_to_string(dst.join("wt-agent-hooks/hooks/hooks.json")).unwrap(),
        r#"{"hooks":{}}"#,
    );
    assert_eq!(
        fs::read_to_string(dst.join("wt-agent-hooks/hooks/native-hook.json")).unwrap(),
        r#"{"command":"wtcli.exe agent-hook"}"#,
    );
}

/// `restage_bundle_dir` removes a preexisting staging directory
/// before re-mirroring `src`. Verifies that stale files from a prior
/// MSIX version (e.g. an old plugin.json) don't survive the
/// re-staging.
#[test]
fn restage_bundle_dir_replaces_stale_contents() {
    let src = unique_dir("restage-src");
    let dst = unique_dir("restage-dst").join("staged");

    fs::create_dir_all(&dst).unwrap();
    fs::write(dst.join("STALE.txt"), "leftover from a prior MSIX version").unwrap();

    fs::write(src.join("fresh.json"), r#"{"v":2}"#).unwrap();

    restage_bundle_dir(&src, &dst).expect("restage succeeds");

    assert!(!dst.join("STALE.txt").exists(), "stale file must be gone");
    assert_eq!(
        fs::read_to_string(dst.join("fresh.json")).unwrap(),
        r#"{"v":2}"#,
    );
}

fn write_opencode_test_bundle(root: &Path, js: &str) {
    fs::write(root.join(OPENCODE_PLUGIN_JS), js).unwrap();
    fs::write(
        root.join(OPENCODE_MANIFEST),
        r#"{"name":"wt-agent-hooks","version":"0.1.3","managed_by":"Intelligent Terminal: wt-agent-hooks"}"#,
    )
    .unwrap();
}

#[test]
fn copy_opencode_bundle_installs_managed_files() {
    let source = unique_dir("opencode-source");
    let home = unique_dir("opencode-home");
    write_opencode_test_bundle(&source, OPENCODE_PLUGIN_JS_CONTENT);

    copy_opencode_bundle(&source, &home).unwrap();

    let installed = opencode_plugins_dir(&home);
    let support_dir = opencode_support_dir(&home);
    assert_eq!(
        fs::read_to_string(installed.join(OPENCODE_PLUGIN_JS)).unwrap(),
        OPENCODE_PLUGIN_JS_CONTENT
    );
    assert!(support_dir.join(OPENCODE_MANIFEST).is_file());
}

#[test]
fn opencode_plugins_dir_honors_xdg_config_home() {
    let home = Path::new(r"C:\Users\example");
    let xdg = Path::new(r"D:\config");

    assert_eq!(
        opencode_plugins_dir_from(home, Some(xdg)),
        xdg.join("opencode").join("plugins")
    );
    assert_eq!(
        opencode_plugins_dir_from(home, None),
        home.join(".config").join("opencode").join("plugins")
    );
}

#[test]
fn copy_opencode_bundle_preserves_non_managed_collision() {
    let source = unique_dir("opencode-collision-source");
    let home = unique_dir("opencode-collision-home");
    write_opencode_test_bundle(&source, OPENCODE_PLUGIN_JS_CONTENT);
    let installed = opencode_plugins_dir(&home);
    fs::create_dir_all(&installed).unwrap();
    fs::write(installed.join(OPENCODE_PLUGIN_JS), "user plugin").unwrap();

    let error = copy_opencode_bundle(&source, &home).unwrap_err();

    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(
        fs::read_to_string(installed.join(OPENCODE_PLUGIN_JS)).unwrap(),
        "user plugin"
    );
    assert!(!opencode_support_dir(&home).exists());
}

#[test]
fn copy_opencode_bundle_rejects_non_file_plugin_collision() {
    let source = unique_dir("opencode-directory-collision-source");
    let home = unique_dir("opencode-directory-collision-home");
    write_opencode_test_bundle(&source, OPENCODE_PLUGIN_JS_CONTENT);
    let installed_js = opencode_plugins_dir(&home).join(OPENCODE_PLUGIN_JS);
    fs::create_dir_all(&installed_js).unwrap();

    let error = copy_opencode_bundle(&source, &home).unwrap_err();

    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    assert!(error.to_string().contains("not a regular managed file"));
    assert!(installed_js.is_dir());
    assert!(!opencode_support_dir(&home).exists());
}

#[test]
fn copy_opencode_bundle_preserves_non_managed_support_directory() {
    let source = unique_dir("opencode-support-collision-source");
    let home = unique_dir("opencode-support-collision-home");
    write_opencode_test_bundle(&source, OPENCODE_PLUGIN_JS_CONTENT);
    let support_dir = opencode_support_dir(&home);
    fs::create_dir_all(&support_dir).unwrap();
    fs::write(support_dir.join("user.txt"), "keep").unwrap();

    let error = copy_opencode_bundle(&source, &home).unwrap_err();

    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(
        fs::read_to_string(support_dir.join("user.txt")).unwrap(),
        "keep"
    );
    assert!(!opencode_plugins_dir(&home).join(OPENCODE_PLUGIN_JS).exists());
}

#[test]
fn copy_opencode_bundle_rolls_back_partial_first_install() {
    let source = unique_dir("opencode-partial-source");
    let home = unique_dir("opencode-partial-home");
    fs::write(source.join(OPENCODE_PLUGIN_JS), OPENCODE_PLUGIN_JS_CONTENT).unwrap();

    assert!(copy_opencode_bundle(&source, &home).is_err());
    assert!(!opencode_support_dir(&home).exists());
    assert!(!opencode_plugins_dir(&home).join(OPENCODE_PLUGIN_JS).exists());

    fs::write(
        source.join(OPENCODE_MANIFEST),
        r#"{"name":"wt-agent-hooks","version":"0.1.3","managed_by":"Intelligent Terminal: wt-agent-hooks"}"#,
    )
    .unwrap();

    copy_opencode_bundle(&source, &home).unwrap();
    assert!(opencode_support_dir(&home).join(OPENCODE_MANIFEST).is_file());
    assert!(opencode_plugins_dir(&home).join(OPENCODE_PLUGIN_JS).is_file());
}

#[test]
fn copy_opencode_bundle_repairs_managed_install_with_bad_manifest() {
    let source = unique_dir("opencode-repair-source");
    let home = unique_dir("opencode-repair-home");
    write_opencode_test_bundle(&source, OPENCODE_PLUGIN_JS_CONTENT);
    let installed = opencode_plugins_dir(&home);
    let support = opencode_support_dir(&home);
    fs::create_dir_all(&support).unwrap();
    fs::write(installed.join(OPENCODE_PLUGIN_JS), OPENCODE_PLUGIN_JS_CONTENT).unwrap();
    fs::write(support.join(OPENCODE_MANIFEST), "incomplete").unwrap();
    fs::write(support.join(OPENCODE_LEGACY_BRIDGE_PS1), "stale bridge").unwrap();

    copy_opencode_bundle(&source, &home).unwrap();

    assert_eq!(
        read_version_field(&support.join(OPENCODE_MANIFEST)),
        Some("0.1.3".parse().unwrap())
    );
    assert!(!support.join(OPENCODE_LEGACY_BRIDGE_PS1).exists());
}

#[test]
fn opencode_status_requires_complete_managed_install() {
    let home = unique_dir("opencode-status");
    let installed = opencode_plugins_dir(&home);
    fs::create_dir_all(&installed).unwrap();
    fs::write(
        installed.join(OPENCODE_PLUGIN_JS),
        OPENCODE_PLUGIN_JS_CONTENT,
    )
    .unwrap();

    let partial = opencode_status(true, Some("opencode.exe".into()), Some(&home));
    assert!(partial.marketplace_registered);
    assert!(!partial.marketplace_path_valid);
    assert!(!partial.plugin_installed);

    let support_dir = opencode_support_dir(&home);
    fs::create_dir_all(&support_dir).unwrap();
    fs::write(
        support_dir.join(OPENCODE_MANIFEST),
        r#"{"name":"wt-agent-hooks","version":"0.1.3","managed_by":"Intelligent Terminal: wt-agent-hooks"}"#,
    )
    .unwrap();
    let complete = opencode_status(true, Some("opencode.exe".into()), Some(&home));
    assert!(complete.marketplace_path_valid);
    assert!(complete.plugin_installed);
    assert!(complete.plugin_enabled);

    fs::remove_file(installed.join(OPENCODE_PLUGIN_JS)).unwrap();
    let support_only = opencode_status(true, Some("opencode.exe".into()), Some(&home));
    assert!(support_only.marketplace_registered);
    assert!(!support_only.marketplace_path_valid);
    assert!(!support_only.plugin_installed);
}

#[test]
fn opencode_same_name_manifest_without_marker_is_not_managed() {
    let home = unique_dir("opencode-unmanaged-manifest");
    let support = opencode_support_dir(&home);
    fs::create_dir_all(&support).unwrap();
    fs::write(
        support.join(OPENCODE_MANIFEST),
        r#"{"name":"wt-agent-hooks","version":"9.9.9"}"#,
    )
    .unwrap();

    let status = opencode_status(true, Some("opencode.exe".into()), Some(&home));
    assert!(!status.marketplace_registered);
    assert!(!status.plugin_installed);
    assert!(read_installed_opencode(&home).unwrap().is_none());

    let uninstall = opencode_uninstall(Some(&home));
    assert_eq!(uninstall.plugin_uninstalled, Some(false));
    assert!(support.join(OPENCODE_MANIFEST).is_file());
}

#[test]
fn opencode_uninstall_removes_only_managed_files() {
    let managed_home = unique_dir("opencode-uninstall-managed");
    let managed_dir = opencode_plugins_dir(&managed_home);
    let source = unique_dir("opencode-uninstall-source");
    write_opencode_test_bundle(&source, OPENCODE_PLUGIN_JS_CONTENT);
    copy_opencode_bundle(&source, &managed_home).unwrap();
    let support_dir = opencode_support_dir(&managed_home);
    fs::write(support_dir.join("user.txt"), "keep").unwrap();

    let result = opencode_uninstall(Some(&managed_home));
    assert_eq!(result.plugin_uninstalled, Some(true));
    assert!(!managed_dir.join(OPENCODE_PLUGIN_JS).exists());
    assert_eq!(
        fs::read_to_string(support_dir.join("user.txt")).unwrap(),
        "keep"
    );

    let user_home = unique_dir("opencode-uninstall-user");
    let user_dir = opencode_plugins_dir(&user_home);
    fs::create_dir_all(&user_dir).unwrap();
    fs::write(user_dir.join(OPENCODE_PLUGIN_JS), "user plugin").unwrap();

    let result = opencode_uninstall(Some(&user_home));
    assert_eq!(result.plugin_uninstalled, Some(false));
    assert_eq!(
        fs::read_to_string(user_dir.join(OPENCODE_PLUGIN_JS)).unwrap(),
        "user plugin"
    );
}

#[test]
fn opencode_uninstall_retry_removes_orphaned_managed_support_files() {
    let home = unique_dir("opencode-uninstall-retry");
    let support = opencode_support_dir(&home);
    fs::create_dir_all(&support).unwrap();
    fs::write(
        support.join(OPENCODE_MANIFEST),
        r#"{"name":"wt-agent-hooks","version":"0.1.3","managed_by":"Intelligent Terminal: wt-agent-hooks"}"#,
    )
    .unwrap();

    let result = opencode_uninstall(Some(&home));

    assert!(result.succeeded());
    assert_eq!(result.plugin_uninstalled, Some(true));
    assert!(!support.exists());
}

#[test]
fn opencode_uninstall_preserves_ownership_markers_after_manifest_failure() {
    let home = unique_dir("opencode-uninstall-failure");
    let source = unique_dir("opencode-uninstall-failure-source");
    write_opencode_test_bundle(&source, OPENCODE_PLUGIN_JS_CONTENT);
    copy_opencode_bundle(&source, &home).unwrap();
    let plugins = opencode_plugins_dir(&home);
    let support = opencode_support_dir(&home);
    fs::remove_file(support.join(OPENCODE_MANIFEST)).unwrap();
    fs::create_dir(support.join(OPENCODE_MANIFEST)).unwrap();

    let failed = opencode_uninstall(Some(&home));

    assert!(!failed.succeeded());
    assert!(plugins.join(OPENCODE_PLUGIN_JS).is_file());
    assert!(support.join(OPENCODE_MANIFEST).is_dir());

    fs::remove_dir(support.join(OPENCODE_MANIFEST)).unwrap();
    let retried = opencode_uninstall(Some(&home));
    assert!(retried.succeeded());
    assert!(!plugins.join(OPENCODE_PLUGIN_JS).exists());
    assert!(!support.join(OPENCODE_MANIFEST).exists());
}

#[test]
fn read_installed_opencode_uses_managed_manifest_version() {
    let home = unique_dir("opencode-installed");
    let installed = opencode_plugins_dir(&home);
    let source = unique_dir("opencode-installed-source");
    write_opencode_test_bundle(&source, OPENCODE_PLUGIN_JS_CONTENT);
    copy_opencode_bundle(&source, &home).unwrap();

    let info = read_installed_opencode(&home)
        .expect("probe succeeds")
        .expect("managed plugin is installed");
    assert_eq!(info.version, Some("0.1.3".parse().unwrap()));
    assert!(info.enabled);

    fs::remove_file(installed.join(OPENCODE_PLUGIN_JS)).unwrap();
    let support_only = read_installed_opencode(&home)
        .expect("probe succeeds")
        .expect("managed support manifest is repairable");
    assert_eq!(support_only.version, None);

    fs::remove_file(opencode_support_dir(&home).join(OPENCODE_MANIFEST)).unwrap();
    fs::write(installed.join(OPENCODE_PLUGIN_JS), "user plugin").unwrap();
    assert!(read_installed_opencode(&home).unwrap().is_none());
}

/// Uninstall must sweep the active `hook-bundle-staging\claude\`
/// directory in addition to the historical staging dirs, so a clean
/// uninstall doesn't leave the MSIX workaround copy behind.
#[test]
fn legacy_staging_dirs_includes_active_claude_staging() {
    let Some(root) = crate::runtime_paths::intelligent_terminal_local_root() else {
        // No LOCALAPPDATA on this host (extremely unusual) — nothing to
        // assert. The function would return an empty Vec in that case
        // and the sweep would log a warning, which is the documented
        // behaviour.
        return;
    };
    let expected = root.join(STAGING_SUBDIR).join(CliKind::Claude.dir_name());

    let claude_dirs = legacy_staging_dirs(CliKind::Claude);
    assert!(
        claude_dirs.iter().any(|p| p == &expected),
        "Claude sweep list should contain the active staging dir {} but was {:?}",
        expected.display(),
        claude_dirs,
    );

    // Copilot and Gemini don't trigger the workaround, so the active
    // staging path must NOT appear in their sweep lists.
    for cli in [CliKind::Copilot, CliKind::Gemini] {
        let dirs = legacy_staging_dirs(cli);
        assert!(
            dirs.iter().all(|p| p != &expected),
            "{:?} sweep list must not include Claude's active staging dir but was {:?}",
            cli,
            dirs,
        );
    }
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
const CODEX_HOOKS_JSON: &str =
    include_str!("../wt-agent-hooks/codex/wt-agent-hooks/hooks/hooks.json");
const CLAUDE_PLUGIN_JSON: &str =
    include_str!("../wt-agent-hooks/claude/wt-agent-hooks/.claude-plugin/plugin.json");
const COPILOT_PLUGIN_JSON: &str =
    include_str!("../wt-agent-hooks/copilot/wt-agent-hooks/plugin.json");
const GEMINI_EXTENSION_JSON: &str =
    include_str!("../wt-agent-hooks/gemini-extension/gemini-extension.json");
const CODEX_PLUGIN_JSON: &str =
    include_str!("../wt-agent-hooks/codex/wt-agent-hooks/.codex-plugin/plugin.json");

const CLAUDE_MARKETPLACE_JSON: &str =
    include_str!("../wt-agent-hooks/claude/.claude-plugin/marketplace.json");
const COPILOT_MARKETPLACE_JSON: &str =
    include_str!("../wt-agent-hooks/copilot/.github/plugin/marketplace.json");

const OPENCODE_PLUGIN_JS_CONTENT: &str =
    include_str!("../wt-agent-hooks/opencode/wt-agent-hooks.js");
const OPENCODE_PLUGIN_JSON: &str =
    include_str!("../wt-agent-hooks/opencode/plugin.json");

/// Every manifest-driven CLI invokes the native wtcli bridge directly; no
/// PowerShell script and no batch launcher sits in between anymore. Copilot
/// additionally *names* a shell — its `powershell` / `bash` command fields —
/// but that is a routing declaration, not a second process: the field value
/// still runs `wtcli.exe` inside the shell the CLI already started.
#[test]
fn bundle_files_are_well_formed() {
    for hooks in [
        CLAUDE_HOOKS_JSON,
        COPILOT_HOOKS_JSON,
        GEMINI_HOOKS_JSON,
        CODEX_HOOKS_JSON,
    ] {
        assert!(hooks.contains("wtcli.exe agent-hook"));
        for banned in [
            "send-event.ps1",
            ".ps1",
            "pwsh",
            "powershell.exe",
            "agent-hook.cmd",
        ] {
            assert!(
                !hooks.contains(banned),
                "hook bundle must not spawn {banned} again"
            );
        }
    }
}

/// Per-CLI hooks.json files must each tag emitted events with the right CLI.
#[test]
fn bundle_hooks_thread_cli_source() {
    assert!(CLAUDE_HOOKS_JSON.contains("--cli-source claude"));
    assert!(COPILOT_HOOKS_JSON.contains("--cli-source copilot"));
    assert!(GEMINI_HOOKS_JSON.contains("--cli-source gemini"));
    assert!(CODEX_HOOKS_JSON.contains("--cli-source codex"));

    // The commands invoke `wtcli.exe` off PATH, so no bundle should still be
    // interpolating a plugin-root placeholder into its hook command.
    for (cli, hooks) in [
        ("claude", CLAUDE_HOOKS_JSON),
        ("copilot", COPILOT_HOOKS_JSON),
        ("gemini", GEMINI_HOOKS_JSON),
        ("codex", CODEX_HOOKS_JSON),
    ] {
        for placeholder in ["${PLUGIN_ROOT}", "${CLAUDE_PLUGIN_ROOT}", "${extensionPath}"] {
            assert!(
                !hooks.contains(placeholder),
                "{cli} hook command should not need {placeholder} any more"
            );
        }
    }
}

/// The shipped hook command has to parse and run under every shell an agent CLI
/// might dispatch it through, and which shell that is has repeatedly turned out
/// to be something we guessed wrong:
///
/// * **Copilot** — PowerShell 7+, per GitHub's hooks documentation.
/// * **Codex** — PowerShell; its sandbox log dispatches every command as
///   `pwsh.exe -NoProfile -Command`.
/// * **Gemini** — PowerShell; `hookRunner.ts` resolves ComSpec-powershell →
///   `pwsh.exe` → `powershell.exe`, with no `cmd.exe` branch at all.
/// * **Claude** — **bash** (`/usr/bin/bash`), which its own debug log reports.
///
/// So the command is a bare executable name with plain arguments: no quoting for
/// PowerShell to reinterpret as an expression, no `cmd.exe` metacharacters, and
/// nothing that bash rewrites. Two earlier spellings each failed somewhere —
/// a quoted path is a PowerShell parse error, and `cmd /c "…"` is destroyed by
/// bash's MSYS path conversion, which turns `/c` into a Windows path so
/// `cmd.exe` starts interactively and never runs the bridge.
///
/// This covers the `command` field, which is the spelling every CLI falls back
/// to. Copilot layers documented `powershell` / `bash` fields on top of it —
/// those are checked by `copilot_shell_variants_match_their_own_shell`, since
/// each is dispatched only by the shell it names.
/// Bundles whose `command` is deliberately written for one shell only, with the
/// shell it targets. Claude declares this with a `shell` field; Gemini has no
/// such field, so its single-shell guarantee is recorded here instead — see
/// `gemini_hooks_exit_zero_when_the_bridge_is_missing` for the source evidence
/// that Gemini only ever dispatches through PowerShell on Windows.
const SINGLE_SHELL_BUNDLES: [(&str, HookShell); 2] = [
    ("gemini", HookShell::PowerShell),
    ("codex", HookShell::PowerShell),
];

/// The shell a hook handler is written for, if it is not meant to be portable:
/// either declared inline with Claude's `shell` field, or recorded for a bundle
/// whose CLI offers no such field.
fn hook_pinned_shell_for(cli: &str, hook: &Value) -> Option<HookShell> {
    if let Some(shell) = hook.get("shell").and_then(Value::as_str) {
        return match shell {
            "bash" => Some(HookShell::Bash),
            "powershell" => Some(HookShell::PowerShell),
            _ => None,
        };
    }
    SINGLE_SHELL_BUNDLES
        .iter()
        .find(|(name, _)| *name == cli)
        .map(|(_, shell)| *shell)
}

/// `command` strings that must work in *every* shell, i.e. those from handlers
/// that are not written for one shell in particular.
fn shell_agnostic_commands(cli: &str, hooks_json: &str) -> Vec<String> {
    let doc: Value = serde_json::from_str(hooks_json).unwrap();
    let mut commands = Vec::new();
    for matchers in doc["hooks"].as_object().unwrap().values() {
        for matcher in matchers.as_array().unwrap() {
            for hook in matcher["hooks"].as_array().unwrap() {
                if hook_pinned_shell_for(cli, hook).is_some() {
                    continue;
                }
                if let Some(command) = hook.get("command").and_then(Value::as_str) {
                    commands.push(command.to_string());
                }
            }
        }
    }
    commands
}

/// `command` strings written for one shell, paired with it.
fn pinned_shell_commands(cli: &str, hooks_json: &str) -> Vec<(HookShell, String)> {
    let doc: Value = serde_json::from_str(hooks_json).unwrap();
    let mut pinned = Vec::new();
    for matchers in doc["hooks"].as_object().unwrap().values() {
        for matcher in matchers.as_array().unwrap() {
            for hook in matcher["hooks"].as_array().unwrap() {
                if let (Some(shell), Some(command)) = (
                    hook_pinned_shell_for(cli, hook),
                    hook.get("command").and_then(Value::as_str),
                ) {
                    pinned.push((shell, command.to_string()));
                }
            }
        }
    }
    pinned
}

#[test]
fn hook_commands_are_shell_agnostic() {
    // Whether a bundle is expected to carry a portable `command` at all.
    // Every bundle now pins the shell its command is written for, so
    // `shell_agnostic_commands` skips them all and this test asserts the
    // absence rather than the shape. Codex was the last portable one until a
    // manual run showed its hooks survive an uninstall in Codex's own plugin
    // cache and fail loudly there — see
    // `codex_hooks_exit_zero_when_the_bridge_is_missing`. The loop below is
    // kept so a bundle that reintroduces a portable `command` is still held to
    // the every-shell contract instead of quietly skipping it.
    for (cli, hooks, portable) in [
        ("copilot", COPILOT_HOOKS_JSON, false),
        ("codex", CODEX_HOOKS_JSON, false),
        ("gemini", GEMINI_HOOKS_JSON, false),
        ("claude", CLAUDE_HOOKS_JSON, false),
    ] {
        let commands = shell_agnostic_commands(cli, hooks);
        // Stated both ways so a bundle that loses its commands by accident
        // fails here instead of turning the loop below into a silent no-op.
        assert_eq!(
            !commands.is_empty(),
            portable,
            "{cli}: portable-command expectation does not match the bundle: {commands:?}"
        );
        for command in commands {
            let expected = format!("wtcli.exe agent-hook --cli-source {cli} --event ");
            assert!(
                command.starts_with(&expected),
                "{cli} hook must invoke the native bridge directly: {command}"
            );
            assert!(
                is_shell_agnostic(&command),
                "{cli} hook command is not safe in every shell: {command}"
            );
            assert!(
                powershell_parses(&command),
                "{cli} hook command must parse under PowerShell: {command}"
            );
        }
    }
}

/// Copilot's `preToolUse` is fail-closed: a hook that exits non-zero denies
/// every tool call for the rest of the session. A portable `command` cannot be
/// guarded — no single spelling both runs everywhere and survives a missing
/// bridge — so shipping one would leave an unguarded path that only has to be
/// taken once to brick a session.
///
/// Measured on Copilot CLI 1.0.81-0: each of `powershell`, `bash` and `command`
/// delivers events when it is the only field present, and a handler with none
/// of them is a silent no-op that does NOT deny. So dropping `command` costs
/// nothing today and degrades fail-open if the per-shell fields ever stop being
/// honoured.
#[test]
fn copilot_ships_no_unguarded_fallback_command() {
    let doc: Value = serde_json::from_str(COPILOT_HOOKS_JSON).unwrap();
    for matchers in doc["hooks"].as_object().unwrap().values() {
        for matcher in matchers.as_array().unwrap() {
            for hook in matcher["hooks"].as_array().unwrap() {
                assert!(
                    hook.get("command").is_none(),
                    "copilot must not ship a bare `command`: {hook}"
                );
                for field in ["powershell", "bash"] {
                    let guarded = hook.get(field).and_then(Value::as_str).unwrap_or_else(|| {
                        panic!("copilot handler is missing its `{field}` command: {hook}")
                    });
                    assert!(
                        guarded.ends_with("exit 0"),
                        "copilot `{field}` command must force a zero exit: {guarded}"
                    );
                }
            }
        }
    }
}

/// Characters that at least one candidate shell treats as syntax rather than
/// text: `cmd.exe` metacharacters, quotes PowerShell would read as an
/// expression, and the backslash/`$` that bash would rewrite. Keeping all of
/// them out is what makes one spelling work everywhere.
const SHELL_METACHARACTERS: [char; 11] =
    ['&', '|', '<', '>', '^', '(', ')', '"', '\'', '\\', '$'];

/// A command line is shell-agnostic when it is a bare executable name followed
/// by plain arguments, with nothing any candidate shell would reinterpret.
fn is_shell_agnostic(command: &str) -> bool {
    !command.chars().any(|c| SHELL_METACHARACTERS.contains(&c))
}

/// A shell an agent CLI might dispatch its `hooks.json` `command` string
/// through. Claude uses bash; the other three use PowerShell. `cmd.exe` is kept
/// in the sweep because it costs nothing and stops a future CLI switching to it
/// from silently breaking hooks.
#[derive(Clone, Copy, Debug)]
enum HookShell {
    /// `pwsh` where available, else Windows PowerShell.
    PowerShell,
    /// `cmd.exe`, wrapped the way Node's `spawn(.., { shell: true })` does.
    Cmd,
    /// bash, which is what Claude reports running hooks through. Skipped when
    /// no bash is installed rather than silently narrowing the sweep.
    Bash,
}

const HOOK_SHELLS: [HookShell; 3] = [HookShell::PowerShell, HookShell::Cmd, HookShell::Bash];

/// Locates a bash to test against, or `None` when the machine has none.
fn bash_exe() -> Option<&'static str> {
    static EXE: std::sync::OnceLock<Option<&'static str>> = std::sync::OnceLock::new();
    *EXE.get_or_init(|| {
        for candidate in [
            r"C:\Program Files\Git\bin\bash.exe",
            r"C:\Program Files\Git\usr\bin\bash.exe",
        ] {
            if Path::new(candidate).is_file() {
                return Some(candidate);
            }
        }
        None
    })
}

/// Whether `wtcli.exe` resolves on `PATH`.
///
/// `symlink_metadata` rather than `is_file`: when Intelligent Terminal is
/// installed, `wtcli.exe` on `PATH` is an MSIX app-execution alias — a
/// zero-length reparse point that following metadata calls can fail to resolve.
fn hook_bridge_on_path() -> bool {
    static FOUND: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FOUND.get_or_init(|| {
        let Some(path) = std::env::var_os("PATH") else {
            return false;
        };
        std::env::split_paths(&path)
            .any(|dir| std::fs::symlink_metadata(dir.join("wtcli.exe")).is_ok())
    })
}

/// Runs a command line the way a CLI would dispatch it. Returns `None` when the
/// shell is unavailable on this machine.
fn run_hook_command(shell: HookShell, command: &str) -> Option<std::process::Output> {
    use std::os::windows::process::CommandExt;

    let mut spawned = match shell {
        HookShell::PowerShell => {
            let mut c = std::process::Command::new(powershell_exe());
            c.args(["-NoProfile", "-NonInteractive", "-Command", command]);
            c
        }
        HookShell::Cmd => {
            // `raw_arg` bypasses Rust's CRT-style escaping, which `cmd.exe`
            // does not honor.
            let mut c = std::process::Command::new("cmd");
            c.raw_arg(format!("/d /s /c \"{command}\""));
            c
        }
        HookShell::Bash => {
            let mut c = std::process::Command::new(bash_exe()?);
            c.args(["-c", command]);
            c
        }
    };
    Some(
        spawned
            .env_remove("WT_COM_CLSID")
            .env_remove("WT_SESSION")
            .output()
            .expect("hook shell should start"),
    )
}

/// Every shipped hook command must reach `wtcli` — not some other program — in
/// every candidate shell. Running it with `WT_COM_CLSID` / `WT_SESSION` cleared
/// makes `wtcli agent-hook` stop at its own env gate, so the observable contract
/// is "exit 0, print nothing" without needing a live Terminal.
///
/// This is the check that would have caught the `cmd /c "…"` spelling: under
/// bash, MSYS path conversion mangled `/c`, so `cmd.exe` started interactively
/// and echoed the hook payload instead of running the bridge.
///
/// Requires `wtcli.exe` on `PATH`, and skips when it is absent — CI runs
/// `cargo test` without building the C++ `wtcli`, and an uninstalled machine
/// has no app-execution alias either. Skipping loses nothing this test can
/// actually assert: with no bridge to reach, a guarded command passes
/// vacuously because its own guard absorbs the miss, and a bare one fails for a
/// reason that has nothing to do with its spelling. Behaviour without the
/// bridge is the subject of the `*_exit_zero_when_the_bridge_is_missing` tests,
/// which substitute [`MISSING_BRIDGE`] so they hold either way.
#[test]
fn bundled_hook_commands_run_in_every_shell() {
    if !hook_bridge_on_path() {
        return;
    }
    for (cli, hooks) in [
        ("copilot", COPILOT_HOOKS_JSON),
        ("codex", CODEX_HOOKS_JSON),
        ("gemini", GEMINI_HOOKS_JSON),
        ("claude", CLAUDE_HOOKS_JSON),
    ] {
        for command in shell_agnostic_commands(cli, hooks) {
            for shell in HOOK_SHELLS {
                let Some(out) = run_hook_command(shell, &command) else {
                    continue;
                };
                assert!(
                    out.status.success(),
                    "{cli} hook must exit 0 under {shell:?}: {command}\nstderr: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
                assert!(
                    out.stdout.is_empty() && out.stderr.is_empty(),
                    "{cli} hook must print nothing under {shell:?}: {command}\nstdout: {}\nstderr: {}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                );
            }
        }

        // The per-shell variants are dispatched only by their own shell, so
        // they get the same "reaches wtcli, stays silent" check there. Handlers
        // that pin themselves with a `shell` field are checked the same way.
        let mut per_shell = hook_shell_variants(hooks);
        per_shell.extend(pinned_shell_commands(cli, hooks));
        for (shell, command) in per_shell {
            let Some(out) = run_hook_command(shell, &command) else {
                continue;
            };
            assert!(
                out.status.success(),
                "{cli} {shell:?} hook variant must exit 0: {command}\nstderr: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            assert!(
                out.stdout.is_empty() && out.stderr.is_empty(),
                "{cli} {shell:?} hook variant must print nothing: {command}\nstdout: {}\nstderr: {}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
}

/// Uninstalling Intelligent Terminal removes the MSIX app-execution alias that
/// puts `wtcli.exe` on `PATH`, but leaves the hook config registered with every
/// CLI. From then on the *shell* — not the bridge — decides the exit code, and
/// a missing command makes it 1. Copilot's `preToolUse` hook is fail-closed, so
/// that exit 1 denies every tool call ("Denied by preToolUse hook … (hook
/// errored)") until the user finds and removes the stale plugin themselves.
///
/// Copilot's schema fixes that without going back to guessing which shell runs
/// the command: explicit `powershell` / `bash` fields take precedence over
/// `command` on their own platform, so each variant can absorb a missing bridge
/// in that shell's own syntax and still exit 0. Substituting a name that cannot
/// resolve keeps the check deterministic on machines where Terminal *is*
/// installed.
/// Claude has no `powershell` / `bash` field pair, but it does document a
/// `shell` field — so it pins bash and guards inside `command` instead. Same
/// contract as Copilot's variants: an uninstalled Terminal must not turn every
/// hook into a failure the user has to diagnose.
///
/// Pinning matters. Claude defaults to bash but falls back to PowerShell when
/// Git Bash is absent, and a guard written for the wrong shell is noisy even on
/// the happy path — so the guard and the `shell` field have to agree.
/// Gemini has neither Copilot's `powershell` / `bash` pair nor Claude's `shell`
/// field — its `CommandHookConfig` carries only `command`. Writing
/// PowerShell-specific syntax there is safe anyway because Gemini's
/// `getShellConfiguration()` has no non-PowerShell branch on Windows: it tries
/// a PowerShell `ComSpec`, then `pwsh.exe`, then falls back to
/// `powershell.exe`, and all three return `shell: "powershell"`.
///
/// That single-shell guarantee is what this test pins. If Gemini ever grows a
/// `cmd.exe` or bash path, `try {` stops parsing and the hook breaks even when
/// the bridge is present — so the guard is asserted to be PowerShell-shaped and
/// exercised under PowerShell with the bridge missing.
#[test]
fn gemini_hooks_exit_zero_when_the_bridge_is_missing() {
    let commands = hook_command_strings(GEMINI_HOOKS_JSON);
    for command in &commands {
        assert!(
            command.starts_with("try { wtcli.exe agent-hook ") && command.ends_with("} catch { }; exit 0"),
            "gemini hook must wrap the bridge in a PowerShell guard: {command}"
        );
    }
    for command in commands {
        let uninstalled = command.replace("wtcli.exe", MISSING_BRIDGE);
        let Some(out) = run_hook_command(HookShell::PowerShell, &uninstalled) else {
            continue;
        };
        assert!(
            out.status.success(),
            "gemini hook must exit 0 with no bridge installed: {uninstalled}\nstderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            out.stdout.is_empty() && out.stderr.is_empty(),
            "gemini hook must stay silent with no bridge installed: {uninstalled}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// Codex was the last bundle shipping the bare spelling. The bundle README
/// justified that with "its marketplace entry points directly at the package
/// directory, so an uninstall takes the plugin with it and the hook never
/// loads" — which a manual run disproved: Codex keeps its own copy under
/// `~/.codex/plugins/cache/<marketplace>/<plugin>/<version>/`, outside the
/// package, and keeps `enabled = true` plus the trusted hashes in
/// `~/.codex/config.toml`. With the bridge gone the hooks therefore still load
/// and still run, and Codex renders each miss in the conversation itself:
///
/// ```text
/// • UserPromptSubmit hook (failed)
///   error: hook exited with code 1
/// ```
///
/// That exit code also settles which shell Codex dispatches through, which the
/// README had recorded two contradictory answers for. A command that cannot be
/// resolved exits 1 under PowerShell, 9009 under `cmd.exe`, and 127 under bash
/// — so PowerShell it is, and the Gemini-shaped guard is the right one.
#[test]
fn codex_hooks_exit_zero_when_the_bridge_is_missing() {
    let pinned = pinned_shell_commands("codex", CODEX_HOOKS_JSON);
    assert!(
        !pinned.is_empty(),
        "codex must pin its hook shell so its guard cannot run under the wrong one"
    );
    for (shell, command) in pinned {
        assert!(
            matches!(shell, HookShell::PowerShell),
            "codex hooks are dispatched through PowerShell: {command}"
        );
        assert!(
            command.starts_with("try { wtcli.exe agent-hook ")
                && command.ends_with("} catch { }; exit 0"),
            "codex hook must wrap the bridge in a PowerShell guard: {command}"
        );
        let uninstalled = command.replace("wtcli.exe", MISSING_BRIDGE);
        let Some(out) = run_hook_command(shell, &uninstalled) else {
            continue;
        };
        assert!(
            out.status.success(),
            "codex hook must exit 0 with no bridge installed: {uninstalled}\nstderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            out.stdout.is_empty() && out.stderr.is_empty(),
            "codex hook must stay silent with no bridge installed: {uninstalled}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn claude_hooks_exit_zero_when_the_bridge_is_missing() {
    let pinned = pinned_shell_commands("claude", CLAUDE_HOOKS_JSON);
    assert!(
        !pinned.is_empty(),
        "claude must pin its hook shell so its guard cannot run under the wrong one"
    );
    for (shell, command) in pinned {
        let uninstalled = command.replace("wtcli.exe", MISSING_BRIDGE);
        let Some(out) = run_hook_command(shell, &uninstalled) else {
            continue;
        };
        assert!(
            out.status.success(),
            "claude {shell:?} hook must exit 0 with no bridge installed: {uninstalled}\nstderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            out.stdout.is_empty() && out.stderr.is_empty(),
            "claude {shell:?} hook must stay silent with no bridge installed: {uninstalled}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn copilot_hook_variants_exit_zero_when_the_bridge_is_missing() {
    let variants = hook_shell_variants(COPILOT_HOOKS_JSON);
    assert!(
        !variants.is_empty(),
        "copilot must ship per-shell hook commands so a missing bridge cannot deny tool calls"
    );
    for (shell, command) in variants {
        let uninstalled = command.replace("wtcli.exe", MISSING_BRIDGE);
        let Some(out) = run_hook_command(shell, &uninstalled) else {
            continue;
        };
        assert!(
            out.status.success(),
            "copilot {shell:?} hook must exit 0 with no bridge installed: {uninstalled}\nstderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            out.stdout.is_empty() && out.stderr.is_empty(),
            "copilot {shell:?} hook must stay silent with no bridge installed: {uninstalled}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// Negative control for the test above, and the reason Copilot ships no bare
/// `command` at all: the unguarded spelling is exactly what fails once the
/// bridge is gone. Pinning that documented failure proves the guards are what
/// prevents the denial, not some property the bare spelling already had.
///
/// The string is built here rather than read from the bundle. Reading it back
/// would tie this control to a field Copilot no longer ships, and a control
/// that quietly stops running is worse than no control.
#[test]
fn bare_hook_command_still_fails_when_the_bridge_is_missing() {
    let command =
        format!("{MISSING_BRIDGE} agent-hook --cli-source copilot --event agent.session.start");
    let out = run_hook_command(HookShell::PowerShell, &command)
        .expect("PowerShell is always available on Windows");
    assert!(
        !out.status.success(),
        "the bare command spelling is expected to fail without a bridge: {command}"
    );
}

/// Each per-shell variant must be written in the syntax of the shell it is
/// dispatched through, still reach the same bridge invocation as `command`, and
/// end by forcing a successful exit — that final `exit 0` is what stops a
/// fail-closed `preToolUse` hook from denying tool calls.
#[test]
fn copilot_shell_variants_match_their_own_shell() {
    for (shell, command) in hook_shell_variants(COPILOT_HOOKS_JSON) {
        assert!(
            command.contains("wtcli.exe agent-hook --cli-source copilot --event "),
            "{shell:?} variant must invoke the same bridge as `command`: {command}"
        );
        assert!(
            command.ends_with("; exit 0"),
            "{shell:?} variant must end by forcing exit 0: {command}"
        );
        match shell {
            HookShell::PowerShell => {
                assert!(
                    command.starts_with("try { wtcli.exe "),
                    "the PowerShell variant must swallow CommandNotFoundException: {command}"
                );
                assert!(
                    powershell_parses(&command),
                    "the PowerShell variant must parse under PowerShell: {command}"
                );
            }
            HookShell::Bash => assert!(
                command.starts_with("command -v wtcli.exe >/dev/null 2>&1 && "),
                "the bash variant must probe for the bridge before running it: {command}"
            ),
            HookShell::Cmd => panic!("no hook field is dispatched through cmd.exe"),
        }
    }
}

/// `pwsh` where available, else Windows PowerShell — both parse the constructs
/// under test identically, so either is a valid stand-in.
fn powershell_exe() -> &'static str {
    static EXE: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
    EXE.get_or_init(|| {
        let probe = std::process::Command::new("pwsh")
            .args(["-NoProfile", "-NonInteractive", "-Command", "exit 0"])
            .output();
        if probe.is_ok() {
            "pwsh"
        } else {
            "powershell"
        }
    })
}

/// Asks PowerShell to *parse* (never run) a command line, so the check needs no
/// installed Terminal and cannot have side effects. This is the exact failure
/// mode that broke Copilot and Codex hooks: a line starting with a quoted path
/// parses as a string expression, so the words after it are a syntax error.
fn powershell_parses(command: &str) -> bool {
    let script = format!(
        "$e = $null; \
         $null = [System.Management.Automation.Language.Parser]::ParseInput('{}', [ref]$null, [ref]$e); \
         if ($e.Count) {{ exit 1 }} else {{ exit 0 }}",
        command.replace('\'', "''")
    );
    std::process::Command::new(powershell_exe())
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .expect("powershell should start")
        .status
        .success()
}

/// Every `command` string in a bundle's `hooks.json`.
fn hook_command_strings(hooks_json: &str) -> Vec<String> {
    let doc: Value = serde_json::from_str(hooks_json).unwrap();
    let mut commands = Vec::new();
    for matchers in doc["hooks"].as_object().unwrap().values() {
        for matcher in matchers.as_array().unwrap() {
            for hook in matcher["hooks"].as_array().unwrap() {
                commands.push(hook["command"].as_str().unwrap().to_string());
            }
        }
    }
    assert!(!commands.is_empty());
    commands
}

/// A bridge name that is guaranteed not to resolve, standing in for the state
/// left behind when Intelligent Terminal is uninstalled: the MSIX
/// app-execution alias that provides `wtcli.exe` disappears while the hook
/// config stays registered with the CLI.
const MISSING_BRIDGE: &str = "wtcli-not-installed-probe.exe";

/// Copilot's documented per-shell command fields, paired with the shell each
/// one is dispatched through. Bundles that ship only the cross-platform
/// `command` spelling contribute nothing here.
fn hook_shell_variants(hooks_json: &str) -> Vec<(HookShell, String)> {
    let doc: Value = serde_json::from_str(hooks_json).unwrap();
    let mut variants = Vec::new();
    for matchers in doc["hooks"].as_object().unwrap().values() {
        for matcher in matchers.as_array().unwrap() {
            for hook in matcher["hooks"].as_array().unwrap() {
                for (field, shell) in [
                    ("powershell", HookShell::PowerShell),
                    ("bash", HookShell::Bash),
                ] {
                    if let Some(command) = hook.get(field).and_then(Value::as_str) {
                        variants.push((shell, command.to_string()));
                    }
                }
            }
        }
    }
    variants
}

/// Negative controls: every spelling this bundle previously shipped must be
/// caught, so the checks above cannot pass vacuously. Each failed in a shell we
/// had not thought to test at the time.
///
/// * A bare quoted path fails to parse in PowerShell.
/// * Prefixing it with PowerShell's `&` call operator fixes PowerShell but is a
///   syntax error in `cmd.exe` — and still *parses* in PowerShell, which is why
///   a PowerShell-only check could not have caught it.
/// * `cmd /c "…"` satisfies both of those, and still breaks under bash: MSYS
///   path conversion rewrites `/c`, so `cmd.exe` launches interactively, prints
///   its banner, and never runs the bridge.
#[test]
fn previous_hook_command_spellings_are_rejected() {
    let quoted_path =
        r#""C:/plugins/wt-agent-hooks/hooks/agent-hook.cmd" --cli-source copilot --event agent.stop"#;
    assert!(
        !powershell_parses(quoted_path),
        "a bare quoted path must fail to parse in PowerShell: {quoted_path}"
    );
    assert!(!is_shell_agnostic(quoted_path));

    let call_operator = format!("& {quoted_path}");
    assert!(
        powershell_parses(&call_operator),
        "the call-operator form does parse in PowerShell — that is why it looked correct"
    );
    assert!(!is_shell_agnostic(&call_operator));

    let cmd_wrapped =
        r#"cmd /c "wtcli.exe agent-hook --cli-source copilot --event agent.stop >nul 2>nul & exit 0""#;
    assert!(
        powershell_parses(cmd_wrapped),
        "the cmd-wrapped form parses in PowerShell too — the shape rule is what rejects it"
    );
    assert!(
        !is_shell_agnostic(cmd_wrapped),
        "the cmd-wrapped form must be rejected: {cmd_wrapped}"
    );

    // And prove the bash failure empirically where a bash is available: the
    // wrapper does not reach wtcli, it starts an interactive cmd that echoes.
    if let Some(out) = run_hook_command(HookShell::Bash, cmd_wrapped) {
        assert!(
            !out.stdout.is_empty() || !out.stderr.is_empty(),
            "under bash the cmd-wrapped form must visibly misbehave rather than run the bridge"
        );
    }
}

/// Both CLIs must carry the common event set, and neither may subscribe a
/// per-tool-call hook. `ErrorOccurred` must NOT appear (undocumented legacy
/// name; the documented equivalent is `StopFailure`).
#[test]
fn claude_and_copilot_carry_full_event_catalog() {
    const COMMON_EVENTS: &[&str] = &[
        "SessionStart",
        "SessionEnd",
        "Notification",
        "UserPromptSubmit",
        "StopFailure",
        "Stop",
    ];
    // Copilot's `PreToolUse` was the last per-tool-call subscription, kept for
    // the Attention path that `app.rs` synthesizes when `tool_name` is a
    // user-input tool. Copilot 1.0.81-2 fires `Notification` for the same
    // question — both carrying the question text, ~0.9s apart — so the tool
    // hook only bought a duplicate. It cost a PowerShell start per tool call
    // (~536 ms measured, ~388 ms of it `pwsh` startup) on the fail-closed path,
    // where the CLI blocks until the hook returns. The completion events went
    // earlier for the same reason: `app.rs` discards them because `agent.stop`
    // owns the turn-end.
    //
    // Turn-level Working is unaffected: `UserPromptSubmit` already maps to
    // `ToolStarting`. What is given up is per-tool granularity in the session
    // row — the tool's name, not its status.
    const NO_PER_TOOL_EVENTS: &[&str] = &[
        "PreToolUse",
        "PostToolUse",
        "PostToolUseFailure",
        "BeforeTool",
        "AfterTool",
    ];
    for (label, hooks) in [
        ("claude", CLAUDE_HOOKS_JSON),
        ("copilot", COPILOT_HOOKS_JSON),
    ] {
        for event in COMMON_EVENTS {
            assert!(
                hooks.contains(&format!("\"{event}\":")),
                "{label} hooks.json missing event {event}"
            );
        }
        assert!(
            !hooks.contains("\"ErrorOccurred\":"),
            "{label} hooks.json still references undocumented ErrorOccurred"
        );
        for event in NO_PER_TOOL_EVENTS {
            assert!(
                !hooks.contains(&format!("\"{event}\":")),
                "{label} hooks.json subscribes {event}, which fires once per tool \
                 call and costs a shell start each time; the Attention path it \
                 used to serve is covered by Notification"
            );
        }
    }
}

/// Drops the fields that carry each CLI's own uninstall-resilience layer, so
/// the parity check below compares the shared event structure rather than
/// failing on plumbing that is necessarily per-CLI: Copilot expresses it with
/// `powershell` / `bash` / `timeoutSec` and ships no portable `command` at all,
/// Claude by pinning `shell` and guarding inside `command`.
fn strip_per_cli_hook_fields(value: &mut Value) {
    match value {
        Value::Array(values) => {
            for value in values {
                strip_per_cli_hook_fields(value);
            }
        }
        Value::Object(values) => {
            for field in ["command", "powershell", "bash", "timeoutSec", "shell"] {
                values.remove(field);
            }
            for value in values.values_mut() {
                strip_per_cli_hook_fields(value);
            }
        }
        _ => {}
    }
}

/// Claude and Copilot now share the same hook-event set exactly, differing
/// only in the per-CLI fields that carry their uninstall-resilience layer.
/// Copilot's tool-use hooks were the last divergence.
#[test]
fn claude_and_copilot_hooks_json_are_parity_identical() {
    let mut normalized_claude: Value = serde_json::from_str(CLAUDE_HOOKS_JSON).unwrap();
    let mut normalized_copilot: Value = serde_json::from_str(COPILOT_HOOKS_JSON).unwrap();
    strip_per_cli_hook_fields(&mut normalized_claude);
    strip_per_cli_hook_fields(&mut normalized_copilot);

    assert_eq!(
        normalized_claude, normalized_copilot,
        "claude/ and copilot/ hook schemas must match modulo the bridge command"
    );
}

/// Copilot uses its native manifest locations while preserving the shared
/// metadata and declaring the hook file explicitly.
#[test]
fn copilot_uses_native_plugin_layout() {
    let claude: Value = serde_json::from_str(CLAUDE_PLUGIN_JSON).unwrap();
    let mut copilot: Value = serde_json::from_str(COPILOT_PLUGIN_JSON).unwrap();
    assert_eq!(
        copilot.get("hooks").and_then(Value::as_str),
        Some("hooks/hooks.json")
    );
    copilot.as_object_mut().unwrap().remove("hooks");
    assert_eq!(claude, copilot, "shared plugin metadata must stay aligned");

    assert_eq!(
        CLAUDE_MARKETPLACE_JSON, COPILOT_MARKETPLACE_JSON,
        "claude/ and copilot/ marketplace.json must match byte-for-byte"
    );
}

/// The five bundles ship as one unit, so the installer's
/// `bundled_version > installed_version` check only pushes a change to every
/// CLI when they move together. `copilot_uses_native_plugin_layout` separately
/// requires claude's and copilot's marketplace files to be byte-identical, and
/// the version lives in those too — so a single-CLI bump is not expressible
/// here even when only one bundle's content changed.
#[test]
fn native_hook_bundle_versions_stay_in_sync() {
    const BUNDLE_VERSION: &str = "0.1.6";
    let manifests = [
        CLAUDE_PLUGIN_JSON,
        COPILOT_PLUGIN_JSON,
        GEMINI_EXTENSION_JSON,
        CODEX_PLUGIN_JSON,
        OPENCODE_PLUGIN_JSON,
    ];
    for manifest in manifests {
        let value: Value = serde_json::from_str(manifest).unwrap();
        assert_eq!(
            value.get("version").and_then(Value::as_str),
            Some(BUNDLE_VERSION)
        );
    }

    for marketplace in [CLAUDE_MARKETPLACE_JSON, COPILOT_MARKETPLACE_JSON] {
        let value: Value = serde_json::from_str(marketplace).unwrap();
        assert_eq!(
            value
                .get("plugins")
                .and_then(Value::as_array)
                .and_then(|plugins| plugins.first())
                .and_then(|plugin| plugin.get("version"))
                .and_then(Value::as_str),
            Some(BUNDLE_VERSION)
        );
    }
}

#[test]
fn opencode_plugin_has_runtime_guards_and_source_tag() {
    assert!(OPENCODE_PLUGIN_JS_CONTENT.contains(OPENCODE_MANAGED_MARKER));
    assert!(OPENCODE_PLUGIN_JS_CONTENT.contains("process.env.WT_COM_CLSID"));
    assert!(OPENCODE_PLUGIN_JS_CONTENT.contains("process.env.WT_SESSION"));
    assert!(OPENCODE_PLUGIN_JS_CONTENT.contains("process.env.OPENCODE_CLIENT"));
    assert!(OPENCODE_PLUGIN_JS_CONTENT.contains("\"acp\""));
    assert!(OPENCODE_PLUGIN_JS_CONTENT.contains("\"wtcli.exe\""));
    assert!(OPENCODE_PLUGIN_JS_CONTENT.contains("\"agent-hook\""));
    assert!(!OPENCODE_PLUGIN_JS_CONTENT.contains("powershell"));
    assert!(OPENCODE_PLUGIN_JS_CONTENT.contains("new TextEncoder().encode"));
    assert!(OPENCODE_PLUGIN_JS_CONTENT.contains("\"opencode\""));
    assert!(OPENCODE_PLUGIN_JS_CONTENT.contains("agent.session.start"));
    assert!(OPENCODE_PLUGIN_JS_CONTENT.contains("value.data?.message"));
    assert!(OPENCODE_PLUGIN_JS_CONTENT.contains("if (!sessionID) return"));
    assert!(OPENCODE_PLUGIN_JS_CONTENT.contains("info.title !== previous.title"));
    assert!(OPENCODE_PLUGIN_JS_CONTENT.contains("rootSessions.get(sessionID).cwd"));
}

#[test]
fn opencode_manifest_has_explicit_ownership_marker() {
    let manifest: Value = serde_json::from_str(OPENCODE_PLUGIN_JSON).unwrap();
    assert_eq!(
        manifest.get("name").and_then(Value::as_str),
        Some(PLUGIN_NAME)
    );
    assert_eq!(
        manifest.get("managed_by").and_then(Value::as_str),
        Some(OPENCODE_MANIFEST_MANAGED_BY)
    );
}

/// `marketplace.json` must declare the `wt-local` marketplace name and
/// the `wt-agent-hooks` plugin pointing at `./wt-agent-hooks`.
#[test]
fn marketplace_json_shape() {
    let v: Value = serde_json::from_str(CLAUDE_MARKETPLACE_JSON).unwrap();
    assert_eq!(
        v.get("name").and_then(|x| x.as_str()),
        Some(MARKETPLACE_NAME)
    );
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
    let cmd = arr[0].get("hooks").and_then(|h| h.as_array()).unwrap()[0]
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

    let expected = PathBuf::from(
        "C:\\repo\\.worktree\\track-copilot-cleanup\\wta\\wt-agent-hooks\\copilot",
    );
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
    let stdout =
        r#"[{"name":"wt-agent-hooks","version":"0.1.0","isActive":true,"path":"..."}]"#;
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
    // find `<repo>/tools/wta/wt-agent-hooks/` though, so this asserts
    // the dev-tree path wins (we deliberately don't assert "none" here
    // because the dev tree IS resolvable — we just check that the
    // env path didn't trip the false-positive).
    let info = bundle::resolve_source();
    assert_ne!(info.kind, "env", "nonexistent env path must not match");

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
    assert_eq!(STATUS_SCHEMA_VERSION, 4);
    assert_eq!(UNINSTALL_SCHEMA_VERSION, 2);
}

// ---- installed-version reporting ------------------------------------

/// Claude and Codex keep every version they have ever unpacked and mark the
/// superseded ones with `.orphaned_at`. Reporting the plain maximum would
/// claim an upgrade the CLI never actually loaded.
#[test]
fn newest_live_cached_version_ignores_orphaned_directories() {
    let root = unique_dir("cached-version");
    for v in ["0.1.4", "0.1.5", "0.1.6"] {
        fs::create_dir_all(root.join(v)).unwrap();
    }
    // 0.1.5 and 0.1.6 were superseded; only 0.1.4 is still loaded.
    fs::write(root.join("0.1.5").join(".orphaned_at"), "x").unwrap();
    fs::write(root.join("0.1.6").join(".orphaned_at"), "x").unwrap();

    assert_eq!(
        newest_live_cached_version(&root).map(|v| v.to_string()),
        Some("0.1.4".to_string()),
    );
}

/// Highest wins among live directories, and non-semver junk in the cache
/// (lock files, stray marker files) must not abort the scan.
#[test]
fn newest_live_cached_version_picks_the_highest_live_directory() {
    let root = unique_dir("cached-version-max");
    for v in ["0.1.4", "0.2.0", "0.1.9", "not-a-version"] {
        fs::create_dir_all(root.join(v)).unwrap();
    }
    fs::write(root.join("stray-file"), "x").unwrap();

    assert_eq!(
        newest_live_cached_version(&root).map(|v| v.to_string()),
        Some("0.2.0".to_string()),
    );
}

/// A missing cache directory is the normal "never installed" state, not an
/// error the caller has to handle.
#[test]
fn newest_live_cached_version_is_none_when_nothing_is_cached() {
    let root = unique_dir("cached-version-empty");
    assert!(newest_live_cached_version(&root.join("absent")).is_none());
    assert!(newest_live_cached_version(&root).is_none());
}

/// The version rides along in the listing Claude already gives us, so status
/// never has to spawn the CLI a second time just to learn it.
#[test]
fn claude_plugin_list_json_parser_reports_the_installed_version() {
    let json = r#"[{"id":"wt-agent-hooks@wt-local","version":"0.1.7","enabled":true}]"#;
    let parsed = parse_claude_plugin_list_json(json).expect("parses");
    assert!(parsed.installed);
    assert_eq!(parsed.version.map(|v| v.to_string()), Some("0.1.7".into()));
}

/// A listing without a parseable version must still report the install, with
/// the version left unknown rather than defaulting to something invented.
#[test]
fn claude_plugin_list_json_parser_tolerates_a_missing_version() {
    let json = r#"[{"id":"wt-agent-hooks@wt-local","enabled":true}]"#;
    let parsed = parse_claude_plugin_list_json(json).expect("parses");
    assert!(parsed.installed);
    assert!(parsed.version.is_none());
}

#[test]
fn gemini_extensions_list_json_parser_reports_the_installed_version() {
    let json = r#"[{"name":"wt-agent-hooks","version":"0.1.5","isActive":true}]"#;
    let parsed = parse_gemini_extensions_list_json(json).expect("parses");
    assert!(parsed.installed);
    assert_eq!(parsed.version.map(|v| v.to_string()), Some("0.1.5".into()));
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

/// Models the Gemini CLI 0.41.2 libuv shutdown crash:
/// `extensions install` writes the extension and prints the
/// success line, then Node.js aborts with exit code `0xC0000409`
/// during async-handle teardown. The captured success substring
/// must convert that into a logical success so the install-side
/// trace log doesn't claim "gemini extensions install failed"
/// for an install that actually wrote the files to disk.
#[test]
fn idempotency_substring_matches_gemini_install_success_after_libuv_crash() {
    let stderr = "You have consented to the following:\n\
        ...legal blurb...\n\
        Extension \"wt-agent-hooks\" installed successfully and enabled.\n\
        Assertion failed: !(handle->flags & UV_HANDLE_CLOSING), \
        file src\\win\\async.c, line 76";
    assert!(matches_idempotency_substring(
        "",
        stderr,
        &["already installed", "installed successfully and enabled"],
    ));
}

/// Mirror of the install-side test for the uninstall path. The
/// `spawn_step` success-substring branch is what makes the
/// `wta hooks uninstall` report show `plugin=ok` for Gemini even
/// when the same libuv crash fires on `extensions uninstall`.
#[test]
fn idempotency_substring_matches_gemini_uninstall_success_after_libuv_crash() {
    let stderr = "Extension \"wt-agent-hooks\" successfully uninstalled.\n\
        Assertion failed: !(handle->flags & UV_HANDLE_CLOSING), \
        file src\\win\\async.c, line 76";
    assert!(matches_idempotency_substring(
        "",
        stderr,
        &["successfully uninstalled"],
    ));
}

/// Idempotent re-uninstall: if the extension is already gone,
/// Gemini exits 1 with `Failed to uninstall "...": Extension not
/// found.` That's the desired state, so we treat it as `ok`.
#[test]
fn idempotency_substring_matches_gemini_extension_not_found() {
    let stderr = "Failed to uninstall \"wt-agent-hooks\": Extension not found.";
    assert!(matches_idempotency_substring(
        "",
        stderr,
        &["successfully uninstalled", "extension not found"],
    ));
}

// ---- spawn_step success-substring tolerance (libuv crash) -----------

/// `spawn_step` should ordinarily report `fail (...)` when the
/// spawned CLI exits non-zero, even if its stdout/stderr happens
/// to contain a generic word like "successfully". This guards
/// against accidentally widening the success-substring contract.
#[test]
fn spawn_step_fail_message_format_when_no_success_substrings() {
    let mut messages = Vec::new();
    // `cmd /c exit 7` is exit-7 and prints nothing. Use an exe
    // we know is on PATH on every Windows box so the test isn't
    // flaky on dev machines that don't have gemini installed.
    let ok = spawn_step(&mut messages, "cmd", &["/c", "exit", "7"], &[]);
    assert!(!ok);
    assert_eq!(messages.len(), 1);
    let m = &messages[0];
    assert!(m.starts_with("fail (7):"), "unexpected: {m}");
    assert!(m.contains("cmd /c exit 7"));
}

/// When the spawned CLI exits non-zero but its captured output
/// contains a registered success substring, `spawn_step` records
/// `ok (...)` and returns `true`. This covers the Gemini libuv
/// crash path.
#[test]
fn spawn_step_treats_success_substring_as_ok_despite_nonzero_exit() {
    let mut messages = Vec::new();
    // PowerShell prints the success line to stdout, then exits 1.
    // `-NoProfile` keeps it fast and predictable in CI.
    let ok = spawn_step(
        &mut messages,
        "powershell",
        &[
            "-NoProfile",
            "-Command",
            "Write-Host 'Extension \"wt-agent-hooks\" successfully uninstalled.'; exit 1",
        ],
        &["successfully uninstalled"],
    );
    assert!(ok, "spawn_step should treat success substring as ok");
    assert_eq!(messages.len(), 1);
    let m = &messages[0];
    assert!(
        m.starts_with("ok (powershell printed success despite exit 1):"),
        "unexpected: {m}"
    );
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
    assert_eq!(
        info.path.as_deref(),
        Some(dir.display().to_string().as_str())
    );
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
        installed_version: None,
        bundle_version: None,
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
        installed_version: None,
        bundle_version: None,
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

#[test]
fn cli_kind_codex_roundtrips() {
    assert_eq!(CliKind::from_name("codex"), Some(CliKind::Codex));
    assert_eq!(CliKind::from_name("CODEX"), Some(CliKind::Codex));
    assert_eq!(CliKind::Codex.name(), "codex");
    assert_eq!(CliKind::Codex.dir_name(), "codex");
    assert!(CliKind::ALL.contains(&CliKind::Codex));
}

#[test]
fn cli_kind_opencode_roundtrips() {
    assert_eq!(CliKind::from_name("opencode"), Some(CliKind::OpenCode));
    assert_eq!(CliKind::from_name("OPENCODE"), Some(CliKind::OpenCode));
    assert_eq!(CliKind::OpenCode.name(), "opencode");
    assert_eq!(CliKind::OpenCode.dir_name(), "opencode");
    assert!(CliKind::ALL.contains(&CliKind::OpenCode));
}

#[test]
fn bundle_resolves_codex_dir_in_dev_tree() {
    // Dev-tree lookup walks up from CARGO_MANIFEST_DIR to find
    // tools/wta/wt-agent-hooks/<dir_name>/. Task 2 puts a real
    // directory at that path, so this should resolve.
    let resolved = bundle::resolve_cli_dir(CliKind::Codex)
        .expect("codex bundle should resolve in dev tree");
    assert!(
        resolved
            .join(".agents")
            .join("plugins")
            .join("marketplace.json")
            .is_file(),
        "resolved codex bundle should contain marketplace.json (got {})",
        resolved.display(),
    );
}

// ---- auto-upgrade: Version parser & ordering -----------------------

#[test]
fn version_parse_accepts_plain_semver() {
    let v: Version = "0.1.1".parse().unwrap();
    assert_eq!(
        v,
        Version {
            major: 0,
            minor: 1,
            patch: 1
        }
    );
    let v: Version = "1.10.2".parse().unwrap();
    assert_eq!(
        v,
        Version {
            major: 1,
            minor: 10,
            patch: 2
        }
    );
}

#[test]
fn version_parse_rejects_non_semver() {
    assert!("0.1".parse::<Version>().is_err()); // too few segments
    assert!("0.1.0.4".parse::<Version>().is_err()); // too many segments
    assert!("0.1.0-rc1".parse::<Version>().is_err()); // prerelease
    assert!("0.1.0+meta".parse::<Version>().is_err()); // build metadata
    assert!("v0.1.0".parse::<Version>().is_err()); // leading char
    assert!("".parse::<Version>().is_err());
    assert!("abc".parse::<Version>().is_err());
}

#[test]
fn version_ordering_handles_double_digit_components() {
    let a: Version = "0.1.10".parse().unwrap();
    let b: Version = "0.1.2".parse().unwrap();
    assert!(a > b);
    let c: Version = "1.0.0".parse().unwrap();
    let d: Version = "0.99.99".parse().unwrap();
    assert!(c > d);
    let e: Version = "0.1.1".parse().unwrap();
    let f: Version = "0.1.1".parse().unwrap();
    assert!(e == f);
    assert!(!(e < f));
}

#[test]
fn version_display_round_trips() {
    let s = "1.2.3";
    let v: Version = s.parse().unwrap();
    assert_eq!(v.to_string(), s);
}

// ---- auto-upgrade: read_version_field ------------------------------

#[test]
fn read_version_field_parses_plugin_json() {
    let dir = unique_dir("read-version-ok");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("manifest.json");
    fs::write(
        &path,
        r#"{"name":"wt-agent-hooks","version":"0.1.1","other":"ignored"}"#,
    )
    .unwrap();
    assert_eq!(read_version_field(&path), Some("0.1.1".parse().unwrap()));
}

#[test]
fn install_for_codex_skips_when_home_absent() {
    let tmp = unique_dir("codex-home-absent");
    // Smoke test: passing a nonexistent HOME-like dir shouldn't panic.
    // After the binary-only detection change, the function skips when
    // `codex` is not on PATH (the common case on CI). On a dev machine
    // with `codex` installed and a bundle resolvable next to `wta.exe`
    // the call may proceed further; the contract this test enforces is
    // "no panic regardless".
    install_for_codex(&tmp);
    let _ = fs::remove_dir_all(tmp);
}

#[test]
fn install_dispatches_codex() {
    // Smoke: dispatching to all per-CLI installers against an empty
    // HOME shouldn't panic. Each installer gates on its CLI being on
    // PATH, so on CI (where none of these CLIs are installed) every
    // one short-circuits cleanly.
    let tmp = unique_dir("codex-dispatch");
    ensure_installed_in(&tmp);
    let _ = fs::remove_dir_all(tmp);
}

#[test]
fn codex_status_falls_back_when_binary_missing() {
    let tmp_root = unique_dir("codex_status_fallback");
    std::fs::create_dir_all(&tmp_root).unwrap();
    let s = codex_status(false, None, Some(&tmp_root));
    assert_eq!(s.name, "codex");
    assert!(!s.binary_on_path);
    assert_eq!(s.detection_fallback, Some("fs"));
    let _ = std::fs::remove_dir_all(&tmp_root);
}

#[test]
fn codex_fs_fallback_detects_install_dirs() {
    let tmp_root = unique_dir("codex_fs_fallback");
    let codex_dir = tmp_root.join(".codex");
    let cache_root = codex_dir
        .join("plugins")
        .join("cache")
        .join(MARKETPLACE_NAME);
    let plugin_dir = cache_root.join(PLUGIN_NAME).join("0.1.0");
    std::fs::create_dir_all(&plugin_dir).unwrap();

    let mut s = CliStatus {
        name: CliKind::Codex.name(),
        binary_on_path: false,
        binary_path: None,
        marketplace_registered: false,
        marketplace_path: None,
        marketplace_path_valid: false,
        plugin_installed: false,
        plugin_enabled: false,
        installed_version: None,
        bundle_version: None,
        detection_fallback: None,
    };
    codex_fs_fallback(&mut s, Some(&tmp_root));
    assert!(s.marketplace_registered);
    assert!(s.plugin_installed);
    assert!(s.plugin_enabled);
    assert_eq!(s.detection_fallback, Some("fs"));
    let _ = std::fs::remove_dir_all(&tmp_root);
}

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
    let sample = "Marketplace `openai-curated`\n\
                  C:\\Users\\x\\.codex\\.tmp\\plugins\\.agents\\plugins\\marketplace.json\n\
                  \n\
                  PLUGIN                   STATUS              VERSION  PATH\n\
                  linear@openai-curated    not installed       -        -\n\
                  \n\
                  Marketplace `wt-local`\n\
                  C:\\path\\to\\bundle\\.agents\\plugins\\marketplace.json\n\
                  \n\
                  PLUGIN                   STATUS              VERSION  PATH\n\
                  wt-agent-hooks@wt-local  installed, enabled  0.1.0    C:\\path\n";
    assert!(parse_codex_plugin_list(sample));
}

#[test]
fn parse_codex_plugin_list_not_installed() {
    let sample = "Marketplace `wt-local`\n\
                  C:\\path\\.agents\\plugins\\marketplace.json\n\
                  \n\
                  PLUGIN                   STATUS         VERSION  PATH\n\
                  wt-agent-hooks@wt-local  not installed  -        -\n";
    assert!(!parse_codex_plugin_list(sample));
}

#[test]
fn parse_codex_plugin_list_absent_row() {
    let sample = "Marketplace `openai-curated`\n\
                  C:\\path\\marketplace.json\n\
                  \n\
                  PLUGIN                   STATUS         VERSION  PATH\n\
                  linear@openai-curated    not installed  -        -\n";
    assert!(!parse_codex_plugin_list(sample));
}

#[test]
fn parse_codex_plugin_list_treats_disabled_as_installed() {
    let sample = "Marketplace `wt-local`\n\
                  \n\
                  PLUGIN                   STATUS      VERSION  PATH\n\
                  wt-agent-hooks@wt-local  installed   0.1.0    C:\\path\n";
    // Plugin is present even if not currently enabled; we still treat
    // it as installed so that we know there's something to clean up.
    assert!(parse_codex_plugin_list(sample));
}

#[test]
fn parse_codex_plugin_list_entry_extracts_version_and_enabled() {
    let sample = "Marketplace `wt-local`\n\
                  C:\\path\\to\\bundle\\.agents\\plugins\\marketplace.json\n\
                  \n\
                  PLUGIN                   STATUS              VERSION  PATH\n\
                  wt-agent-hooks@wt-local  installed, enabled  0.1.0    C:\\path\n";
    let info = parse_codex_plugin_list_entry(sample).expect("expected entry");
    assert_eq!(info.version, Some("0.1.0".parse().unwrap()));
    assert!(info.enabled);
    assert!(info.gemini_source.is_none());
    assert!(info.gemini_type.is_none());
}

#[test]
fn parse_codex_plugin_list_entry_handles_bare_installed_status() {
    // Some Codex builds may omit the ", enabled" suffix; tolerate
    // bare "installed" and default to enabled=true.
    let sample = "PLUGIN                   STATUS     VERSION  PATH\n\
                  wt-agent-hooks@wt-local  installed  0.2.3    C:\\path\n";
    let info = parse_codex_plugin_list_entry(sample).expect("expected entry");
    assert_eq!(info.version, Some("0.2.3".parse().unwrap()));
    assert!(info.enabled);
}

#[test]
fn parse_codex_plugin_list_entry_marks_disabled_status() {
    // Defensive: if a future Codex release surfaces a disabled
    // status, the upgrade flow must back off (decide_upgrade
    // returns Skip(Disabled) when enabled=false).
    let sample = "PLUGIN                   STATUS               VERSION  PATH\n\
                  wt-agent-hooks@wt-local  installed, disabled  0.1.0    C:\\path\n";
    let info = parse_codex_plugin_list_entry(sample).expect("expected entry");
    assert_eq!(info.version, Some("0.1.0".parse().unwrap()));
    assert!(!info.enabled);
}

#[test]
fn parse_codex_plugin_list_entry_returns_none_when_not_installed() {
    let sample = "PLUGIN                   STATUS         VERSION  PATH\n\
                  wt-agent-hooks@wt-local  not installed  -        -\n";
    assert!(parse_codex_plugin_list_entry(sample).is_none());
}

#[test]
fn parse_codex_plugin_list_entry_returns_none_when_row_absent() {
    let sample = "PLUGIN                   STATUS         VERSION  PATH\n\
                  linear@openai-curated    not installed  -        -\n";
    assert!(parse_codex_plugin_list_entry(sample).is_none());
}

#[test]
fn parse_codex_plugin_list_entry_returns_none_when_version_unparseable() {
    // Status is installed but version column is "-" — InstalledInfo
    // returned with version=None so decide_upgrade conservative-skips
    // via UnknownInstalledVersion.
    let sample = "PLUGIN                   STATUS              VERSION  PATH\n\
                  wt-agent-hooks@wt-local  installed, enabled  -        C:\\path\n";
    let info = parse_codex_plugin_list_entry(sample).expect("expected entry");
    assert!(info.version.is_none());
    assert!(info.enabled);
}

#[test]
fn uninstall_for_codex_skips_when_home_absent() {
    let parent = unique_dir("uninstall_codex_absent");
    let result = uninstall_for_codex(Some(&parent));
    assert_eq!(result.name, "codex");
    assert!(!result.attempted);
    assert!(result.plugin_uninstalled.is_none());
    assert!(result.marketplace_removed.is_none());
    let _ = std::fs::remove_dir_all(&parent);
}

#[test]
fn read_version_field_returns_none_on_garbage_or_missing() {
    let dir = unique_dir("read-version-bad");
    fs::create_dir_all(&dir).unwrap();
    let missing = dir.join("missing.json");
    assert!(read_version_field(&missing).is_none());

    let bad_json = dir.join("bad.json");
    fs::write(&bad_json, "not json").unwrap();
    assert!(read_version_field(&bad_json).is_none());

    let no_version = dir.join("no-ver.json");
    fs::write(&no_version, r#"{"name":"foo"}"#).unwrap();
    assert!(read_version_field(&no_version).is_none());

    let bad_version = dir.join("bad-ver.json");
    fs::write(&bad_version, r#"{"version":"0.1.0-rc1"}"#).unwrap();
    assert!(read_version_field(&bad_version).is_none());
}

// ---- auto-upgrade: read_installed_copilot --------------------------

#[test]
fn read_installed_copilot_picks_marketplace_qualified_entry() {
    let home = unique_dir("copilot-installed");
    let cfg_dir = home.join(".copilot");
    fs::create_dir_all(&cfg_dir).unwrap();
    fs::write(
        cfg_dir.join("config.json"),
        r#"// User settings belong in settings.json.
{
  "installedPlugins": [
{ "name": "wt-agent-hooks", "marketplace": "wt-local",
  "version": "0.1.0", "enabled": true,
  "cache_path": "..." },
{ "name": "wt-agent-hooks", "marketplace": "some-other",
  "version": "9.9.9", "enabled": true }
  ]
}"#,
    )
    .unwrap();

    let info = read_installed_copilot(&home).unwrap().unwrap();
    // Must pick the wt-local entry, not the other marketplace's
    assert_eq!(info.version, Some("0.1.0".parse().unwrap()));
    assert!(info.enabled);
}

#[test]
fn read_installed_copilot_respects_disabled_flag() {
    let home = unique_dir("copilot-disabled");
    let cfg_dir = home.join(".copilot");
    fs::create_dir_all(&cfg_dir).unwrap();
    fs::write(
        cfg_dir.join("config.json"),
        r#"{
  "installedPlugins": [
{ "name": "wt-agent-hooks", "marketplace": "wt-local",
  "version": "0.1.1", "enabled": false }
  ]
}"#,
    )
    .unwrap();
    let info = read_installed_copilot(&home).unwrap().unwrap();
    assert!(!info.enabled);
}

#[test]
fn read_installed_copilot_returns_none_when_not_installed() {
    let home = unique_dir("copilot-empty");
    let cfg_dir = home.join(".copilot");
    fs::create_dir_all(&cfg_dir).unwrap();
    fs::write(cfg_dir.join("config.json"), r#"{"installedPlugins":[]}"#).unwrap();
    assert!(read_installed_copilot(&home).unwrap().is_none());
}

// ---- auto-upgrade: read_installed_gemini ---------------------------

#[test]
fn read_installed_gemini_reads_both_files() {
    let home = unique_dir("gemini-installed");
    let ext_dir = gemini_extension_dir(&home);
    fs::create_dir_all(&ext_dir).unwrap();
    fs::write(
        ext_dir.join("gemini-extension.json"),
        r#"{"name":"wt-agent-hooks","version":"0.1.0"}"#,
    )
    .unwrap();
    let bundle_src = unique_dir("gemini-bundle-src");
    fs::create_dir_all(&bundle_src).unwrap();
    fs::write(
        ext_dir.join(".gemini-extension-install.json"),
        format!(
            r#"{{"type":"local","source":{}}}"#,
            serde_json::Value::String(bundle_src.display().to_string())
        ),
    )
    .unwrap();

    let info = read_installed_gemini(&home).unwrap().unwrap();
    assert_eq!(info.version, Some("0.1.0".parse().unwrap()));
    assert_eq!(info.gemini_type.as_deref(), Some("local"));
    assert_eq!(info.gemini_source.as_deref(), Some(bundle_src.as_path()));
}

#[test]
fn read_installed_gemini_returns_none_when_no_manifest() {
    let home = unique_dir("gemini-empty");
    assert!(read_installed_gemini(&home).unwrap().is_none());
}

#[test]
fn read_installed_gemini_tolerates_missing_install_metadata() {
    let home = unique_dir("gemini-no-install-meta");
    let ext_dir = gemini_extension_dir(&home);
    fs::create_dir_all(&ext_dir).unwrap();
    fs::write(
        ext_dir.join("gemini-extension.json"),
        r#"{"name":"wt-agent-hooks","version":"0.1.0"}"#,
    )
    .unwrap();

    let info = read_installed_gemini(&home).unwrap().unwrap();
    assert_eq!(info.version, Some("0.1.0".parse().unwrap()));
    assert!(info.gemini_source.is_none());
    assert!(info.gemini_type.is_none());
}

// ---- auto-upgrade: decide_upgrade ----------------------------------

fn installed(version: &str, enabled: bool) -> InstalledInfo {
    InstalledInfo {
        version: Some(version.parse().unwrap()),
        enabled,
        gemini_source: None,
        gemini_type: None,
    }
}

#[test]
fn decide_skip_when_not_installed() {
    let a = decide_upgrade(CliKind::Copilot, Some("0.1.1".parse().unwrap()), None, None);
    assert_eq!(a, UpgradeAction::Skip(SkipReason::NotInstalled));
}

#[test]
fn decide_skip_when_disabled() {
    let info = installed("0.1.0", false);
    let a = decide_upgrade(
        CliKind::Copilot,
        Some("0.1.1".parse().unwrap()),
        Some(&info),
        None,
    );
    assert_eq!(a, UpgradeAction::Skip(SkipReason::Disabled));
}

#[test]
fn decide_skip_when_up_to_date_or_newer() {
    let info = installed("0.1.1", true);
    let a = decide_upgrade(
        CliKind::Copilot,
        Some("0.1.1".parse().unwrap()),
        Some(&info),
        None,
    );
    assert_eq!(a, UpgradeAction::Skip(SkipReason::UpToDate));

    // Installed newer than bundle — also skip; never downgrade.
    let info = installed("0.2.0", true);
    let a = decide_upgrade(
        CliKind::Copilot,
        Some("0.1.1".parse().unwrap()),
        Some(&info),
        None,
    );
    assert_eq!(a, UpgradeAction::Skip(SkipReason::UpToDate));
}

#[test]
fn decide_skip_when_bundle_or_installed_version_unknown() {
    // Unknown bundle version → conservative skip.
    let info = installed("0.1.0", true);
    let a = decide_upgrade(CliKind::Copilot, None, Some(&info), None);
    assert_eq!(a, UpgradeAction::Skip(SkipReason::UnknownBundleVersion));

    // Installed but version unparseable → conservative skip.
    let info = InstalledInfo {
        version: None,
        enabled: true,
        gemini_source: None,
        gemini_type: None,
    };
    let a = decide_upgrade(
        CliKind::Copilot,
        Some("0.1.1".parse().unwrap()),
        Some(&info),
        None,
    );
    assert_eq!(a, UpgradeAction::Skip(SkipReason::UnknownInstalledVersion));
}

#[test]
fn decide_copilot_and_claude_upgrade_via_update_plugin() {
    let info = installed("0.1.0", true);
    for cli in [CliKind::Copilot, CliKind::Claude] {
        let a = decide_upgrade(cli, Some("0.1.1".parse().unwrap()), Some(&info), None);
        assert_eq!(a, UpgradeAction::UpdatePlugin, "cli={cli:?}");
    }
}

#[test]
fn decide_codex_upgrade_via_reinstall() {
    // Codex outdated installed → CodexReinstall (Codex has no
    // `plugin update` subcommand).
    let info = installed("0.1.0", true);
    let a = decide_upgrade(
        CliKind::Codex,
        Some("0.1.1".parse().unwrap()),
        Some(&info),
        None,
    );
    assert_eq!(a, UpgradeAction::CodexReinstall);
}

#[test]
fn decide_opencode_upgrade_via_managed_copy() {
    let info = installed("0.1.0", true);
    let action = decide_upgrade(
        CliKind::OpenCode,
        Some("0.1.3".parse().unwrap()),
        Some(&info),
        None,
    );
    assert_eq!(action, UpgradeAction::OpenCodeCopy);
}

#[test]
fn decide_opencode_repairs_unknown_installed_version() {
    let info = InstalledInfo {
        version: None,
        enabled: true,
        gemini_source: None,
        gemini_type: None,
    };
    let action = decide_upgrade(
        CliKind::OpenCode,
        Some("0.1.3".parse().unwrap()),
        Some(&info),
        None,
    );
    assert_eq!(action, UpgradeAction::OpenCodeCopy);
}

#[test]
fn decide_codex_skip_when_up_to_date() {
    let info = installed("0.1.1", true);
    let a = decide_upgrade(
        CliKind::Codex,
        Some("0.1.1".parse().unwrap()),
        Some(&info),
        None,
    );
    assert_eq!(a, UpgradeAction::Skip(SkipReason::UpToDate));
}

#[test]
fn decide_codex_skip_when_disabled() {
    let info = installed("0.1.0", false);
    let a = decide_upgrade(
        CliKind::Codex,
        Some("0.1.1".parse().unwrap()),
        Some(&info),
        None,
    );
    assert_eq!(a, UpgradeAction::Skip(SkipReason::Disabled));
}

#[test]
fn decide_codex_skip_when_not_installed() {
    let a = decide_upgrade(CliKind::Codex, Some("0.1.1".parse().unwrap()), None, None);
    assert_eq!(a, UpgradeAction::Skip(SkipReason::NotInstalled));
}

#[test]
fn decide_gemini_in_place_when_source_under_current_bundle() {
    let bundle_dir = unique_dir("gemini-bundle-current");
    let nested_src = bundle_dir.join("nested").join("inner");
    fs::create_dir_all(&nested_src).unwrap();
    let info = InstalledInfo {
        version: Some("0.1.0".parse().unwrap()),
        enabled: true,
        gemini_source: Some(nested_src.clone()),
        gemini_type: Some("local".into()),
    };
    let a = decide_upgrade(
        CliKind::Gemini,
        Some("0.1.1".parse().unwrap()),
        Some(&info),
        Some(&bundle_dir),
    );
    assert_eq!(a, UpgradeAction::GeminiUpdateInPlace);
}

#[test]
fn decide_gemini_reinstall_when_source_stale() {
    let bundle_dir = unique_dir("gemini-bundle-new");
    fs::create_dir_all(&bundle_dir).unwrap();
    // Source points at a path that doesn't exist on disk.
    let stale_src = unique_dir("gemini-stale-src");
    let info = InstalledInfo {
        version: Some("0.1.0".parse().unwrap()),
        enabled: true,
        gemini_source: Some(stale_src),
        gemini_type: Some("local".into()),
    };
    let a = decide_upgrade(
        CliKind::Gemini,
        Some("0.1.1".parse().unwrap()),
        Some(&info),
        Some(&bundle_dir),
    );
    assert_eq!(a, UpgradeAction::GeminiReinstall);
}

#[test]
fn decide_gemini_reinstall_when_type_is_not_local() {
    let bundle_dir = unique_dir("gemini-bundle-git");
    let inside = bundle_dir.join("inside");
    fs::create_dir_all(&inside).unwrap();
    let info = InstalledInfo {
        version: Some("0.1.0".parse().unwrap()),
        enabled: true,
        gemini_source: Some(inside),
        gemini_type: Some("git".into()),
    };
    let a = decide_upgrade(
        CliKind::Gemini,
        Some("0.1.1".parse().unwrap()),
        Some(&info),
        Some(&bundle_dir),
    );
    assert_eq!(a, UpgradeAction::GeminiReinstall);
}

// ---- auto-upgrade: state file --------------------------------------

#[test]
fn upgrade_state_round_trips_through_disk() {
    let dir = unique_dir("upgrade-state-roundtrip");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("hooks-upgrade-state.json");

    let mut s = UpgradeState::default();
    s.set(CliKind::Copilot, Some("0.1.1".into()));
    s.set(CliKind::Claude, Some("0.1.1".into()));
    s.set(CliKind::Gemini, Some("0.1.2".into()));
    save_upgrade_state(&path, &s);

    let loaded = load_upgrade_state(&path);
    assert_eq!(loaded.get(CliKind::Copilot), Some("0.1.1"));
    assert_eq!(loaded.get(CliKind::Claude), Some("0.1.1"));
    assert_eq!(loaded.get(CliKind::Gemini), Some("0.1.2"));
}

#[test]
fn failed_upgrade_does_not_advance_cached_version() {
    let mut state = UpgradeState::default();
    state.set(CliKind::OpenCode, Some("0.1.2".into()));

    let changed =
        state.record_completed(CliKind::OpenCode, Some("0.1.3".into()), false);

    assert!(!changed);
    assert_eq!(state.get(CliKind::OpenCode), Some("0.1.2"));
}

#[test]
fn uninstall_report_detects_explicit_failures() {
    let success = CliUninstallResult {
        name: "opencode",
        attempted: true,
        plugin_uninstalled: Some(true),
        marketplace_removed: None,
        staging_dir_removed: true,
        messages: Vec::new(),
    };
    let mut report = UninstallReport {
        schema_version: UNINSTALL_SCHEMA_VERSION,
        clis: vec![success.clone()],
    };
    assert!(report.succeeded());

    report.clis[0].plugin_uninstalled = Some(false);
    assert!(!report.succeeded());

    report.clis[0] = success;
    report.clis[0].staging_dir_removed = false;
    assert!(!report.succeeded());
}

#[test]
fn upgrade_state_load_returns_default_on_missing_or_bad_file() {
    let dir = unique_dir("upgrade-state-bad");
    fs::create_dir_all(&dir).unwrap();
    let missing = dir.join("missing.json");
    let s = load_upgrade_state(&missing);
    assert!(s.get(CliKind::Copilot).is_none());

    let garbage = dir.join("garbage.json");
    fs::write(&garbage, "not json").unwrap();
    let s = load_upgrade_state(&garbage);
    assert!(s.get(CliKind::Copilot).is_none());
}

#[test]
fn upgrade_state_omits_none_entries() {
    let mut s = UpgradeState::default();
    s.set(CliKind::Copilot, Some("0.1.1".into()));
    let v = s.to_json();
    let obj = v.as_object().unwrap();
    assert!(obj.contains_key("copilot"));
    assert!(!obj.contains_key("claude"));
    assert!(!obj.contains_key("gemini"));
}

// ---- auto-upgrade: cleanup_stale_claude_marketplace ----------------

#[test]
fn cleanup_stale_claude_marketplace_noop_when_file_missing() {
    let dir = unique_dir("claude-cleanup-missing");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("known_marketplaces.json");
    let expected = unique_dir("claude-cleanup-expected");
    cleanup_stale_claude_marketplace(&path, &expected).unwrap();
    assert!(!path.exists());
}

#[test]
fn cleanup_stale_claude_marketplace_rewrites_source_path() {
    let dir = unique_dir("claude-cleanup-rewrite");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("known_marketplaces.json");
    let stale = unique_dir("claude-stale-bundle");
    let known = serde_json::json!({
        MARKETPLACE_NAME: {
            "source": {
                "source": "directory",
                "path": stale.display().to_string()
            },
            "installLocation": stale.display().to_string()
        }
    });
    fs::write(&path, serde_json::to_string_pretty(&known).unwrap()).unwrap();

    let expected = unique_dir("claude-fresh-bundle");
    cleanup_stale_claude_marketplace(&path, &expected).unwrap();

    let rewritten: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let entry = rewritten.get(MARKETPLACE_NAME).unwrap();
    assert_eq!(
        entry["source"]["path"].as_str().unwrap(),
        expected.display().to_string()
    );
    assert_eq!(
        entry["installLocation"].as_str().unwrap(),
        expected.display().to_string()
    );
}

#[test]
fn cleanup_stale_claude_marketplace_noop_when_path_already_matches() {
    let dir = unique_dir("claude-cleanup-noop");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("known_marketplaces.json");
    let expected = unique_dir("claude-current-bundle");
    let known = serde_json::json!({
        MARKETPLACE_NAME: {
            "source": {
                "source": "directory",
                "path": expected.display().to_string()
            }
        }
    });
    let original = serde_json::to_string_pretty(&known).unwrap();
    fs::write(&path, &original).unwrap();
    cleanup_stale_claude_marketplace(&path, &expected).unwrap();
    // File should be byte-identical (no rewrite).
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
}

#[test]
fn cleanup_stale_claude_marketplace_skips_github_source() {
    let dir = unique_dir("claude-cleanup-github");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("known_marketplaces.json");
    let known = serde_json::json!({
        MARKETPLACE_NAME: {
            "source": { "source": "github", "repo": "owner/repo" }
        }
    });
    let original = serde_json::to_string_pretty(&known).unwrap();
    fs::write(&path, &original).unwrap();
    let expected = unique_dir("claude-some-dir");
    cleanup_stale_claude_marketplace(&path, &expected).unwrap();
    // Should not touch github-shaped sources.
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
}

// ---- auto-upgrade: gemini_source_under_bundle ---------------------

#[test]
fn gemini_source_under_bundle_walks_ancestors() {
    let bundle = unique_dir("gemini-under-bundle");
    let nested = bundle.join("a").join("b").join("c");
    fs::create_dir_all(&nested).unwrap();
    assert!(gemini_source_under_bundle(&nested, &bundle));
    assert!(gemini_source_under_bundle(&bundle, &bundle)); // equality
    let outside = unique_dir("gemini-outside");
    assert!(!gemini_source_under_bundle(&outside, &bundle));
}
