use anyhow::Result;

use super::args::HooksCliFilter;

pub(crate) fn run_install(cli: HooksCliFilter, only_missing: bool, json_mode: bool) -> Result<()> {
    // Logging is initialized in `main()`; the install attempt is observable in
    // %LOCALAPPDATA%\IntelligentTerminal\logs\wta-install-hooks.log.
    let scope = cli.into_scope();

    // `--only-missing` trades one status pass up front for the far more
    // expensive install pass behind it. `<cli> plugin install` is two Node
    // spawns per CLI that rewrite the plugin directory even when the bytes
    // don't change — and that rewrite is exactly what Windows denies while a
    // running agent CLI holds the directory open, turning a no-op into a
    // reported failure. The Settings "Install hooks" button passes it so
    // clicking on an already-current machine costs one query, not five
    // reinstalls.
    let pre_status = only_missing.then(|| crate::agent_hooks_installer::status_scoped(scope));
    let selected = select_clis_to_install(scope, pre_status.as_ref());
    let spawn_failures = crate::agent_hooks_installer::ensure_installed_for(&selected);

    // Two independent failure signals, because neither one alone is sufficient.
    //
    // `spawn_failures` is what the install commands themselves reported. It is
    // the only signal that catches an install failing while a PREVIOUS install
    // is still on disk: `<cli> plugin install` replaces the whole plugin
    // directory and Windows denies that while a CLI process holds it open, so
    // the command fails, the stale plugin survives, and the status check below
    // still sees a plugin installed. That combination used to print success and
    // exit 0, leaving the user running hooks from an older build with no way to
    // know short of reading the trace log.
    //
    // The status check stays because it catches the opposite case: a command
    // that reports success without leaving anything usable behind.
    //
    // When `--only-missing` selected nothing, the pre-pass IS the verification:
    // no install ran, so nothing on disk moved, and re-querying would pay a
    // second round of per-CLI Node spawns to re-derive a report we still hold.
    let report = match pre_status {
        Some(pre) if selected.is_empty() => pre,
        _ => crate::agent_hooks_installer::status_scoped(scope),
    };
    let missing: Vec<&str> = report
        .clis
        .iter()
        .filter(|c| {
            let in_scope = match scope {
                crate::agent_hooks_installer::CliScope::All => true,
                crate::agent_hooks_installer::CliScope::One(kind) => c.name == kind.name(),
            };
            // A CLI is "failed" if it's in scope, present on the machine
            // (cli_found), but hooks are not installed.
            in_scope && c.binary_on_path && !c.plugin_installed
        })
        .map(|c| c.name)
        .collect();

    if json_mode {
        // Emitted for both outcomes: the Settings UI needs the per-CLI
        // breakdown precisely when the run failed, and the exit code below
        // still carries pass/fail for scripts.
        let install_report = build_install_report(scope, &report, &spawn_failures, &missing);
        println!(
            "{}",
            serde_json::to_string_pretty(&install_report)
                .unwrap_or_else(|_| serde_json::to_string(&install_report).unwrap_or_default())
        );
    }

    if spawn_failures.is_empty() && missing.is_empty() {
        if json_mode {
            return Ok(());
        }
        // The version rides inside the interpolated CLI list rather than in its
        // own placeholder, so adding it costs no re-translation across the
        // locale set — "name (vX.Y.Z)" reads the same in every language.
        let installed: Vec<String> = report
            .clis
            .iter()
            .filter(|c| c.binary_on_path && c.plugin_installed)
            .map(
                |c| match crate::agent_hooks_installer::installed_plugin_version(c.name) {
                    Some(v) => format!("{} (v{v})", c.name),
                    // A CLI whose version can't be read still installed fine;
                    // saying so beats omitting it or inventing a number.
                    None => c.name.to_string(),
                },
            )
            .collect();
        // Name the CLIs: with `--cli <x>` it confirms the scope took effect,
        // and without it, it distinguishes "installed everywhere" from
        // "silently skipped every CLI because none are on PATH".
        println!(
            "{}",
            t!("hooks.install_succeeded", clis = installed.join(", "))
        );
        return Ok(());
    }

    let message = format_install_failure(&spawn_failures, &missing);
    tracing::error!(target: "agent_hooks", "{}", message);
    anyhow::bail!(message)
}

/// Which in-scope CLIs the install pass should actually touch.
///
/// `pre_status` is `Some` only for `--only-missing`; that CLI is dropped when
/// [`is_up_to_date`](crate::agent_hooks_installer::is_up_to_date) can prove
/// re-installing it would change nothing. Without a pre-pass every in-scope
/// CLI is selected, which is the historical `wta hooks install` behavior.
///
/// Split out from [`run_install`] so the skip decision is testable without
/// spawning a single agent CLI.
fn select_clis_to_install(
    scope: crate::agent_hooks_installer::CliScope,
    pre_status: Option<&crate::agent_hooks_installer::StatusReport>,
) -> Vec<crate::agent_hooks_installer::CliKind> {
    use crate::agent_hooks_installer::{is_up_to_date, CliKind, CliScope};

    CliKind::ALL
        .iter()
        .copied()
        .filter(|kind| match scope {
            CliScope::All => true,
            CliScope::One(only) => only == *kind,
        })
        .filter(|kind| {
            let Some(status) = pre_status else {
                return true;
            };
            let up_to_date = status
                .clis
                .iter()
                .find(|c| c.name == kind.name())
                .is_some_and(is_up_to_date);
            if up_to_date {
                tracing::info!(
                    target: "agent_hooks",
                    cli = kind.name(),
                    "hooks already installed at the bundled version; skipping install",
                );
            }
            !up_to_date
        })
        .collect()
}

/// Fold the two independent failure signals and the post-install status
/// check into one per-CLI verdict.
///
/// Failure wins over the status check: a CLI whose install command failed
/// while a PREVIOUS plugin is still on disk reads as `plugin_installed` in
/// the status report, and reporting that as `installed` is exactly the
/// silent-stale-build case [`run_install`] exists to catch.
fn build_install_report(
    scope: crate::agent_hooks_installer::CliScope,
    status: &crate::agent_hooks_installer::StatusReport,
    spawn_failures: &[crate::agent_hooks_installer::InstallFailure],
    missing: &[&str],
) -> crate::agent_hooks_installer::InstallReport {
    use crate::agent_hooks_installer::{
        CliInstallResult, CliScope, InstallReport, INSTALL_OUTCOME_FAILED,
        INSTALL_OUTCOME_INSTALLED, INSTALL_OUTCOME_SKIPPED,
    };

    let clis = status
        .clis
        .iter()
        .filter(|c| match scope {
            CliScope::All => true,
            CliScope::One(kind) => c.name == kind.name(),
        })
        .map(|c| {
            if let Some(f) = spawn_failures.iter().find(|f| f.cli == c.name) {
                return CliInstallResult {
                    name: c.name,
                    outcome: INSTALL_OUTCOME_FAILED,
                    reason: Some(f.reason.clone()),
                };
            }
            if missing.contains(&c.name) {
                return CliInstallResult {
                    name: c.name,
                    outcome: INSTALL_OUTCOME_FAILED,
                    reason: None,
                };
            }
            CliInstallResult {
                name: c.name,
                outcome: if c.binary_on_path && c.plugin_installed {
                    INSTALL_OUTCOME_INSTALLED
                } else {
                    INSTALL_OUTCOME_SKIPPED
                },
                reason: None,
            }
        })
        .collect();

    InstallReport::new(clis)
}

/// Render the user-facing failure text for an install that did not fully land.
///
/// Split out from [`run_install`] so the wording — especially the lock hint,
/// which is the whole reason a failed install used to look like a successful
/// one — is testable without spawning any agent CLI.
fn format_install_failure(
    spawn_failures: &[crate::agent_hooks_installer::InstallFailure],
    missing: &[&str],
) -> String {
    let names: Vec<&str> = spawn_failures
        .iter()
        .map(|f| f.cli)
        .chain(missing.iter().copied())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();

    let mut out = format!("hooks installation failed for: {}", names.join(", "));
    for f in spawn_failures {
        out.push_str(&format!("\n  {}: {}", f.cli, f.reason));
    }
    for name in missing {
        // A CLI that already reported a spawn error would otherwise be listed
        // twice, once with the real reason and once with a vaguer one.
        if !spawn_failures.iter().any(|f| f.cli == *name) {
            out.push_str(&format!(
                "\n  {name}: install reported no error but no hooks are registered"
            ));
        }
    }
    out
}

pub(crate) fn run_status(json_mode: bool) -> Result<()> {
    let report = crate::agent_hooks_installer::status();
    if json_mode {
        println!(
            "{}",
            serde_json::to_string_pretty(&report)
                .unwrap_or_else(|_| serde_json::to_string(&report).unwrap_or_default())
        );
    } else {
        format_status_human(&report);
    }
    Ok(())
}

pub(crate) fn run_uninstall(cli: HooksCliFilter, json_mode: bool) -> Result<()> {
    let report = crate::agent_hooks_installer::uninstall(cli.into_scope());
    if json_mode {
        println!(
            "{}",
            serde_json::to_string_pretty(&report)
                .unwrap_or_else(|_| serde_json::to_string(&report).unwrap_or_default())
        );
    } else {
        format_uninstall_human(&report);
    }
    if report.succeeded() {
        Ok(())
    } else {
        anyhow::bail!("one or more hook uninstall steps failed")
    }
}

fn format_status_human(r: &crate::agent_hooks_installer::StatusReport) {
    let path_suffix = r
        .bundle_source
        .path
        .as_deref()
        .map(|p| format!(" ({})", p))
        .unwrap_or_default();
    println!(
        "{}",
        t!(
            "hooks.bundle_source",
            // The version rides inside the already-interpolated source value
            // rather than in a placeholder of its own, so surfacing it costs
            // no re-translation across the locale set.
            source = format_bundle_source(r.bundle_source.kind, unique_bundle_version(&r.clis)),
            path_suffix = path_suffix,
        )
    );
    println!();
    for c in &r.clis {
        let summary = if !c.binary_on_path {
            t!("hooks.cli_not_on_path").into_owned()
        } else if c.plugin_installed && c.plugin_enabled && c.marketplace_path_valid {
            t!("hooks.installed").into_owned()
        } else if c.plugin_installed && !c.marketplace_path_valid {
            t!("hooks.marketplace_path_stale").into_owned()
        } else if c.plugin_installed {
            t!("hooks.installed_but_disabled").into_owned()
        } else {
            t!("hooks.not_installed").into_owned()
        };
        let detail = format!(
            "marketplace={}, path_valid={}, plugin={}, enabled={}{}",
            yn(c.marketplace_registered),
            yn(c.marketplace_path_valid),
            yn(c.plugin_installed),
            yn(c.plugin_enabled),
            c.detection_fallback
                .map(|m| format!(", detection={}", m))
                .unwrap_or_default(),
        );
        let version = format_version_column(
            c.installed_version.as_deref(),
            c.bundle_version.as_deref(),
            c.plugin_installed,
        );
        println!(
            "  {:<10} {:<28}  {:<24}  ({})",
            c.name, summary, version, detail
        );
        if let Some(p) = c.marketplace_path.as_deref() {
            println!("    path: {}", p);
        }
    }
}

/// The single version this wta's bundle ships, or `None` unless every CLI
/// reports the same one.
///
/// Every CLI subtree carries its own manifest, so both a mixed bundle and a
/// partially-readable one are representable. Summarizing either as a single
/// number would be a lie — and the more dangerous case is the partial one,
/// because a CLI whose manifest we couldn't read shows no bundle suffix on its
/// row, which reads exactly like "matches the bundle". Staying silent on the
/// header line keeps the per-CLI column the only claim being made.
fn unique_bundle_version(clis: &[crate::agent_hooks_installer::CliStatus]) -> Option<String> {
    let mut versions = clis.iter().map(|c| c.bundle_version.as_deref());
    let first = versions.next()??;
    versions
        .all(|v| v == Some(first))
        .then(|| format!("v{}", first))
}

fn format_bundle_source(kind: &str, version: Option<String>) -> String {
    match version {
        Some(v) => format!("{kind} {v}"),
        None => kind.to_string(),
    }
}

/// Render the per-CLI version column.
///
/// The question this column exists to answer is "is the CLI running the hooks
/// this wta ships?", so the bundle version only appears when it disagrees with
/// what's installed; printing both on every row would bury the one row that
/// needs attention. It is labelled rather than arrowed because the mismatch
/// runs both ways in practice — a CLI registered against another worktree is
/// routinely *newer* than the bundle, and an arrow would read as a pending
/// upgrade. The header line carries the bundle version for the matching case.
fn format_version_column(
    installed: Option<&str>,
    bundle: Option<&str>,
    plugin_installed: bool,
) -> String {
    if !plugin_installed {
        return "-".to_string();
    }
    // "Installed but won't say which build" is a different problem from "not
    // installed", and the fs-fallback detection paths can genuinely land here.
    let Some(installed) = installed else {
        return "v?".to_string();
    };
    match bundle {
        Some(b) if b != installed => format!("v{installed} (bundle v{b})"),
        _ => format!("v{installed}"),
    }
}

fn format_uninstall_human(r: &crate::agent_hooks_installer::UninstallReport) {
    for c in &r.clis {
        let summary = if !c.attempted {
            t!("hooks.uninstall_skipped").into_owned()
        } else {
            let plugin = c
                .plugin_uninstalled
                .map(|b| if b { "ok" } else { "failed" })
                .unwrap_or("-");
            let mkt = c
                .marketplace_removed
                .map(|b| if b { "ok" } else { "failed" })
                .unwrap_or("-");
            format!(
                "plugin={} marketplace={} staging={}",
                plugin,
                mkt,
                if c.staging_dir_removed {
                    "ok"
                } else {
                    "failed"
                },
            )
        };
        println!("  {:<10} {}", c.name, summary);
        for m in &c.messages {
            println!("    \u{00b7} {}", m);
        }
    }
}

fn yn(b: bool) -> &'static str {
    if b {
        "yes"
    } else {
        "no"
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_install_report, format_bundle_source, format_install_failure, format_version_column,
        select_clis_to_install,
    };
    use crate::agent_hooks_installer::{
        BundleSourceInfo, CliScope, CliStatus, InstallFailure, StatusReport,
    };

    fn failure(cli: &'static str, reason: &str) -> InstallFailure {
        InstallFailure {
            cli,
            reason: reason.to_string(),
        }
    }

    /// The regression this whole path exists for: `<cli> plugin install` fails
    /// because a running CLI holds the plugin directory open, but a previous
    /// install is still on disk, so the on-disk status check sees a plugin and
    /// reports nothing wrong. The spawn error must still reach the user.
    #[test]
    fn spawn_failure_is_reported_even_when_a_stale_plugin_is_still_installed() {
        let failures = [failure(
            "copilot",
            "copilot plugin install wt-agent-hooks@wt-local failed: Access is denied. (os error 5)",
        )];
        let message = format_install_failure(&failures, &[]);
        assert!(
            message.contains("copilot"),
            "the failing CLI must be named: {message}"
        );
        assert!(
            message.contains("Access is denied"),
            "the underlying reason must survive: {message}"
        );
    }

    /// The opposite failure shape: the install command claimed success but left
    /// nothing behind. That is what the on-disk check is for.
    #[test]
    fn silent_no_op_install_is_reported_from_the_status_check() {
        let message = format_install_failure(&[], &["claude"]);
        assert!(message.contains("claude"), "{message}");
        assert!(
            message.contains("no hooks are registered"),
            "a silent no-op must be described as such: {message}"
        );
    }

    /// A CLI that both failed to spawn and shows no hooks installed is one
    /// problem, not two — reporting it twice buries the real reason.
    #[test]
    fn a_cli_in_both_signals_is_listed_once_with_the_real_reason() {
        let failures = [failure("copilot", "install failed: os error 5")];
        let message = format_install_failure(&failures, &["copilot"]);
        assert_eq!(
            message.matches("copilot:").count(),
            1,
            "expected exactly one per-CLI detail line: {message}"
        );
        assert!(
            !message.contains("no hooks are registered"),
            "the concrete reason must win over the generic one: {message}"
        );
        assert_eq!(
            message.lines().next().unwrap(),
            "hooks installation failed for: copilot",
            "the summary line must not repeat the CLI: {message}"
        );
    }

    fn cli_with_bundle(name: &'static str, bundle: Option<&str>) -> CliStatus {
        CliStatus {
            name,
            binary_on_path: true,
            binary_path: None,
            marketplace_registered: true,
            marketplace_path: None,
            marketplace_path_valid: true,
            plugin_installed: true,
            plugin_enabled: true,
            installed_version: None,
            bundle_version: bundle.map(str::to_string),
            detection_fallback: None,
        }
    }

    /// The whole point of the column: a CLI running a different build than the
    /// bundle must not read as plain "installed". The mismatch is labelled,
    /// not arrowed, because installed is routinely the *newer* side when the
    /// marketplace points at another worktree.
    #[test]
    fn version_column_shows_the_bundle_version_only_when_it_differs() {
        assert_eq!(
            format_version_column(Some("0.1.6"), Some("0.1.5"), true),
            "v0.1.6 (bundle v0.1.5)"
        );
        assert_eq!(
            format_version_column(Some("0.1.5"), Some("0.1.5"), true),
            "v0.1.5",
            "matching versions must not carry redundant bundle noise"
        );
    }

    /// "Installed but the version is unreadable" and "nothing installed" are
    /// different problems; collapsing them would hide the first one, which is
    /// what the fs-fallback detection path actually produces.
    #[test]
    fn version_column_distinguishes_unknown_from_absent() {
        assert_eq!(format_version_column(None, Some("0.1.5"), true), "v?");
        assert_eq!(format_version_column(None, Some("0.1.5"), false), "-");
    }

    /// A CLI whose bundle manifest is unreadable still installed a real
    /// version; the missing half must not erase the known half.
    #[test]
    fn version_column_survives_an_unknown_bundle_version() {
        assert_eq!(format_version_column(Some("0.1.5"), None, true), "v0.1.5");
    }

    /// The header line may only claim one bundle version when there is one.
    #[test]
    fn bundle_version_is_summarized_only_when_every_cli_agrees() {
        let agreeing = [
            cli_with_bundle("copilot", Some("0.1.5")),
            cli_with_bundle("claude", Some("0.1.5")),
        ];
        assert_eq!(
            super::unique_bundle_version(&agreeing).as_deref(),
            Some("v0.1.5")
        );

        let mixed = [
            cli_with_bundle("copilot", Some("0.1.5")),
            cli_with_bundle("claude", Some("0.1.4")),
        ];
        assert_eq!(
            super::unique_bundle_version(&mixed),
            None,
            "a mixed bundle must not be summarized as a single version"
        );

        let unknown = [cli_with_bundle("copilot", None)];
        assert_eq!(super::unique_bundle_version(&unknown), None);
    }

    /// One unreadable manifest is enough to disqualify the header claim. This
    /// is the dangerous case: the CLI we know nothing about shows no bundle
    /// suffix on its row, which reads just like "matches the bundle", so a
    /// confident header version would compound the error rather than expose it.
    #[test]
    fn one_unknown_bundle_version_suppresses_the_header_claim() {
        let partial = [
            cli_with_bundle("copilot", Some("0.1.5")),
            cli_with_bundle("claude", None),
        ];
        assert_eq!(super::unique_bundle_version(&partial), None);

        // Order must not matter — the unknown may come first.
        let partial_reversed = [
            cli_with_bundle("copilot", None),
            cli_with_bundle("claude", Some("0.1.5")),
        ];
        assert_eq!(super::unique_bundle_version(&partial_reversed), None);

        assert_eq!(super::unique_bundle_version(&[]), None);
    }

    /// An unresolvable bundle must still print its `kind`, because that is the
    /// field that explains *why* there's no version to show.
    #[test]
    fn bundle_source_label_degrades_to_the_bare_kind() {
        assert_eq!(
            format_bundle_source("exe-sibling", Some("v0.1.5".to_string())),
            "exe-sibling v0.1.5"
        );
        assert_eq!(format_bundle_source("none", None), "none");
    }

    // ---- install report (`wta hooks install --json`) --------------------

    fn status_of(clis: Vec<CliStatus>) -> StatusReport {
        StatusReport {
            schema_version: 4,
            clis,
            bundle_source: BundleSourceInfo {
                kind: "exe-sibling",
                path: None,
            },
        }
    }

    fn absent_cli(name: &'static str) -> CliStatus {
        CliStatus {
            name,
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
        }
    }

    fn no_failures() -> [InstallFailure; 0] {
        []
    }

    fn outcome_of<'a>(
        report: &'a crate::agent_hooks_installer::InstallReport,
        name: &str,
    ) -> &'a str {
        report
            .clis
            .iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("{name} missing from report"))
            .outcome
    }

    /// The reason the JSON exists: the Settings UI needs to name the CLI that
    /// failed. A spawn failure must be reported per-CLI, with its reason, and
    /// must not contaminate the CLIs that installed fine.
    #[test]
    fn install_report_names_the_failing_cli_and_carries_its_reason() {
        let status = status_of(vec![
            cli_with_bundle("copilot", Some("0.1.6")),
            cli_with_bundle("codex", Some("0.1.6")),
        ]);
        let failures = [failure(
            "codex",
            "codex plugin marketplace add failed: already added from a different source",
        )];

        let report = build_install_report(CliScope::All, &status, &failures, &[]);

        assert_eq!(outcome_of(&report, "copilot"), "installed");
        assert_eq!(outcome_of(&report, "codex"), "failed");
        let codex = report.clis.iter().find(|c| c.name == "codex").unwrap();
        assert!(
            codex
                .reason
                .as_deref()
                .unwrap_or_default()
                .contains("already added from a different source"),
            "the actionable reason must survive into the report: {:?}",
            codex.reason
        );
    }

    /// Mirrors `spawn_failure_is_reported_even_when_a_stale_plugin_is_still_installed`:
    /// the status check sees the PREVIOUS plugin on disk, so only the spawn
    /// failure distinguishes "installed" from "still running the old build".
    #[test]
    fn install_report_prefers_the_spawn_failure_over_a_stale_on_disk_plugin() {
        let status = status_of(vec![cli_with_bundle("copilot", Some("0.1.5"))]);
        let failures = [failure(
            "copilot",
            "install failed: Access is denied. (os error 5)",
        )];

        let report = build_install_report(CliScope::All, &status, &failures, &[]);

        assert_eq!(
            outcome_of(&report, "copilot"),
            "failed",
            "a stale plugin left on disk must not read as a successful install"
        );
    }

    /// A CLI that simply isn't on the machine is not a failure — reporting it
    /// as one would name every uninstalled CLI in the Settings error line.
    #[test]
    fn install_report_marks_an_absent_cli_as_skipped() {
        let status = status_of(vec![
            cli_with_bundle("copilot", Some("0.1.6")),
            absent_cli("gemini"),
        ]);

        let report = build_install_report(CliScope::All, &status, &no_failures(), &[]);

        assert_eq!(outcome_of(&report, "gemini"), "skipped");
        assert_eq!(outcome_of(&report, "copilot"), "installed");
    }

    /// The silent no-op: the install command reported success but left nothing
    /// registered. It has no spawn reason, so `reason` stays absent while the
    /// outcome is still `failed`.
    #[test]
    fn install_report_reports_a_silent_no_op_as_failed_without_a_reason() {
        let status = status_of(vec![absent_cli("claude")]);

        let report = build_install_report(CliScope::All, &status, &no_failures(), &["claude"]);

        let claude = report.clis.iter().find(|c| c.name == "claude").unwrap();
        assert_eq!(claude.outcome, "failed");
        assert!(claude.reason.is_none());
    }

    /// `--cli <x>` must narrow the report too, or the UI would name CLIs the
    /// user never asked to install.
    #[test]
    fn install_report_honors_a_single_cli_scope() {
        use crate::agent_hooks_installer::CliKind;

        let status = status_of(vec![
            cli_with_bundle("copilot", Some("0.1.6")),
            cli_with_bundle("codex", Some("0.1.6")),
        ]);

        let report =
            build_install_report(CliScope::One(CliKind::Codex), &status, &no_failures(), &[]);

        assert_eq!(report.clis.len(), 1);
        assert_eq!(report.clis[0].name, "codex");
    }

    /// The C++ parser rejects an unexpected `schema_version` outright, so the
    /// version this code emits is part of the contract, not an implementation
    /// detail.
    #[test]
    fn install_report_pins_its_schema_version() {
        let report =
            build_install_report(CliScope::All, &status_of(vec![]), &no_failures(), &[]);
        assert_eq!(report.schema_version, 1);
    }

    // ---- `--only-missing` selection --------------------------------------

    fn current_cli(name: &'static str, version: &str) -> CliStatus {
        CliStatus {
            installed_version: Some(version.to_string()),
            ..cli_with_bundle(name, Some(version))
        }
    }

    /// The `--only-missing` contract the Settings "Install hooks" button relies
    /// on: a CLI already carrying the bundled version is dropped from the
    /// install pass, and every state that is anything less than current is
    /// kept — including the ones that superficially read as installed.
    #[test]
    fn only_missing_skips_current_clis_and_keeps_everything_else() {
        use crate::agent_hooks_installer::CliKind;

        let status = status_of(vec![
            current_cli("copilot", "0.1.6"),
            // Older than the bundle.
            CliStatus {
                installed_version: Some("0.1.5".to_string()),
                ..cli_with_bundle("claude", Some("0.1.6"))
            },
            // Installed, but the version can't be read.
            cli_with_bundle("gemini", Some("0.1.6")),
            // Present but disabled — a partial state the button repairs.
            CliStatus {
                plugin_enabled: false,
                installed_version: Some("0.1.6".to_string()),
                ..cli_with_bundle("codex", Some("0.1.6"))
            },
            absent_cli("opencode"),
        ]);

        assert_eq!(
            select_clis_to_install(CliScope::All, Some(&status)),
            vec![
                CliKind::Claude,
                CliKind::Gemini,
                CliKind::Codex,
                CliKind::OpenCode
            ],
        );
    }

    /// A CLI the status pass never reported on is unknown, not current. Absent
    /// evidence must fall back to doing the work.
    #[test]
    fn only_missing_installs_clis_absent_from_the_status_report() {
        use crate::agent_hooks_installer::CliKind;

        let status = status_of(vec![current_cli("copilot", "0.1.6")]);

        assert_eq!(
            select_clis_to_install(CliScope::All, Some(&status)),
            vec![
                CliKind::Claude,
                CliKind::Gemini,
                CliKind::Codex,
                CliKind::OpenCode
            ],
        );
    }

    /// Without the flag, `wta hooks install` stays a full (re)install — the
    /// escape hatch for a break that status can't see.
    #[test]
    fn a_plain_install_still_selects_every_in_scope_cli() {
        use crate::agent_hooks_installer::CliKind;

        assert_eq!(
            select_clis_to_install(CliScope::All, None),
            CliKind::ALL.to_vec()
        );
        assert_eq!(
            select_clis_to_install(CliScope::One(CliKind::Codex), None),
            vec![CliKind::Codex]
        );
    }

    /// Scope wins over state in both directions: another CLI needing an
    /// install must not widen a `--cli` run, and a current CLI must still be
    /// skipped when it is the one named.
    #[test]
    fn only_missing_respects_a_single_cli_scope() {
        use crate::agent_hooks_installer::CliKind;

        let status = status_of(vec![
            cli_with_bundle("copilot", Some("0.1.6")),
            current_cli("codex", "0.1.6"),
        ]);

        assert!(select_clis_to_install(CliScope::One(CliKind::Codex), Some(&status)).is_empty());
        assert_eq!(
            select_clis_to_install(CliScope::One(CliKind::Copilot), Some(&status)),
            vec![CliKind::Copilot]
        );
    }
}
