use anyhow::Result;

use super::args::HooksCliFilter;

pub(crate) fn run_install(cli: HooksCliFilter, force: bool, json_mode: bool) -> Result<()> {
    // Logging is initialized in `main()`; the install attempt is written under
    // the canonical `logging::log_dir()` (the packaged cache path when packaged).
    let scope = cli.into_scope();

    let (plan, spawn_failures, report, missing) = if force {
        let plan = full_install_plan(scope);
        let spawn_failures = crate::agent_hooks_installer::apply_install_plan(&plan);
        let report = crate::agent_hooks_installer::status_scoped(scope);
        let missing = missing_installs(scope, &report);
        (plan, spawn_failures, report, missing)
    } else {
        let reconciled = crate::agent_hooks_installer::reconcile_agent_hooks(scope);
        (
            reconciled.plan,
            reconciled.spawn_failures,
            reconciled.status,
            reconciled.missing,
        )
    };
    let install_report = build_install_report(scope, &report, &spawn_failures, &missing);
    for cli in &install_report.clis {
        let attempted = plan.iter().any(|(kind, action)| {
            kind.name() == cli.name
                && !matches!(action, crate::agent_hooks_installer::InstallAction::Skip)
        });
        let outcome = if attempted { cli.outcome } else { "skipped" };
        crate::telemetry::log_hook_operation_completed("Install", cli.name, outcome);
    }

    if json_mode {
        // Emit per-CLI diagnostics even on failure; the exit code below
        // independently carries pass/fail for scripts.
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

/// Preserve the historical full-install behavior for explicit
/// `wta hooks install --force` recovery calls.
fn full_install_plan(
    scope: crate::agent_hooks_installer::CliScope,
) -> Vec<(
    crate::agent_hooks_installer::CliKind,
    crate::agent_hooks_installer::InstallAction,
)> {
    use crate::agent_hooks_installer::{CliKind, CliScope, InstallAction};

    CliKind::ALL
        .iter()
        .copied()
        .filter(|kind| match scope {
            CliScope::All => true,
            CliScope::One(only) => only == *kind,
        })
        .map(|kind| (kind, InstallAction::Install))
        .collect()
}

fn missing_installs(
    scope: crate::agent_hooks_installer::CliScope,
    status: &crate::agent_hooks_installer::StatusReport,
) -> Vec<&'static str> {
    crate::agent_hooks_installer::build_reconciliation_plan(scope, status)
        .iter()
        .map(|(kind, _)| kind.name())
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
                "\n  {name}: hooks are still missing, incomplete, disabled, or outdated after the operation"
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
        print!("{}", format_status_human(&report));
    }
    Ok(())
}

pub(crate) fn run_uninstall(cli: HooksCliFilter, json_mode: bool) -> Result<()> {
    let report = crate::agent_hooks_installer::uninstall(cli.into_scope());
    for cli in &report.clis {
        let outcome = if !cli.attempted {
            "skipped"
        } else if cli.plugin_uninstalled == Some(false)
            || cli.marketplace_removed == Some(false)
            || !cli.staging_dir_removed
        {
            "failed"
        } else {
            "succeeded"
        };
        crate::telemetry::log_hook_operation_completed("Uninstall", cli.name, outcome);
    }
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

fn format_status_human(r: &crate::agent_hooks_installer::StatusReport) -> String {
    let mut output = String::new();
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
        let version = format_version_column(c.installed_version.as_deref(), c.plugin_installed);
        output.push_str(&format!(
            "  {:<10} {:<28}  {:<24}  ({})\n",
            c.name, summary, version, detail
        ));
        if let Some(p) = c.marketplace_path.as_deref() {
            output.push_str(&format!("    path: {}\n", p));
        }
    }
    output
}

/// Render the per-CLI version column.
fn format_version_column(installed: Option<&str>, plugin_installed: bool) -> String {
    if !plugin_installed {
        return "-".to_string();
    }
    // "Installed but won't say which build" is a different problem from "not
    // installed", and the fs-fallback detection paths can genuinely land here.
    let Some(installed) = installed else {
        return "v?".to_string();
    };
    format!("v{installed}")
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
        build_install_report, format_install_failure, format_status_human, format_version_column,
        full_install_plan, missing_installs,
    };
    use crate::agent_hooks_installer::{
        build_reconciliation_plan, BundleSourceInfo, CliScope, CliStatus, InstallFailure,
        StatusReport,
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
            message.contains("hooks are still missing"),
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
            !message.contains("hooks are still missing"),
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

    #[test]
    fn version_column_shows_only_the_installed_version() {
        assert_eq!(format_version_column(Some("0.1.6"), true), "v0.1.6");
        assert_eq!(format_version_column(Some("0.1.6"), false), "-");
    }

    /// "Installed but the version is unreadable" and "nothing installed" are
    /// different problems; collapsing them would hide the first one, which is
    /// what the fs-fallback detection path actually produces.
    #[test]
    fn version_column_distinguishes_unknown_from_absent() {
        assert_eq!(format_version_column(None, true), "v?");
        assert_eq!(format_version_column(None, false), "-");
    }

    #[test]
    fn status_keeps_bundle_diagnostics_only_in_json() {
        let _locale = crate::test_support::lock_locale();
        rust_i18n::set_locale("en-US");

        let mut installed = cli_with_bundle("copilot", Some("0.1.5"));
        installed.installed_version = Some("0.1.6".to_string());
        installed.marketplace_path = Some(r"C:\installed\copilot".to_string());
        let mut report = status_of(vec![installed, absent_cli("claude")]);
        let bundle_path = r"C:\development\wt-agent-hooks";
        report.bundle_source.path = Some(bundle_path.to_string());

        let output = format_status_human(&report);
        let lines: Vec<_> = output.lines().collect();
        assert_eq!(lines.len(), 3, "{output}");
        assert!(lines[0].starts_with("  copilot "), "{output}");
        assert!(lines[0].contains("v0.1.6"), "{output}");
        assert!(
            lines[0].contains("marketplace=yes, path_valid=yes, plugin=yes, enabled=yes"),
            "{output}"
        );
        assert_eq!(lines[1], r"    path: C:\installed\copilot");
        assert!(lines[2].starts_with("  claude "), "{output}");
        assert!(lines[2].contains("CLI not on PATH"), "{output}");
        assert!(!output.contains("bundle"), "{output}");
        assert!(!output.contains(report.bundle_source.kind), "{output}");
        assert!(!output.contains(bundle_path), "{output}");

        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["bundle_source"]["kind"], "exe-sibling");
        assert_eq!(json["bundle_source"]["path"], bundle_path);
        assert_eq!(json["clis"][0]["bundle_version"], "0.1.5");
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

    /// A spawn failure must be reported per-CLI, with its reason, and
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

    /// The report's schema version remains part of the public CLI contract.
    #[test]
    fn install_report_pins_its_schema_version() {
        let report = build_install_report(CliScope::All, &status_of(vec![]), &no_failures(), &[]);
        assert_eq!(report.schema_version, 1);
    }

    // ---- reconciliation planning -----------------------------------------

    fn installed_cli(name: &'static str, version: &str) -> CliStatus {
        CliStatus {
            installed_version: Some(version.to_string()),
            ..cli_with_bundle(name, Some(version))
        }
    }

    #[test]
    fn forced_install_validation_accepts_current_hooks_and_skips_absent_clis() {
        let status = status_of(vec![
            installed_cli("copilot", "0.1.6"),
            absent_cli("gemini"),
        ]);
        let missing = missing_installs(CliScope::All, &status);
        assert!(missing.is_empty());

        let report = build_install_report(CliScope::All, &status, &[], &missing);
        assert_eq!(outcome_of(&report, "copilot"), "installed");
        assert_eq!(outcome_of(&report, "gemini"), "skipped");
    }

    #[test]
    fn forced_install_validation_rejects_incomplete_or_outdated_hooks() {
        let healthy = installed_cli("copilot", "0.1.6");
        let cases = [
            CliStatus {
                plugin_installed: false,
                ..healthy.clone()
            },
            CliStatus {
                plugin_enabled: false,
                ..healthy.clone()
            },
            CliStatus {
                marketplace_registered: false,
                ..healthy.clone()
            },
            CliStatus {
                marketplace_path_valid: false,
                ..healthy.clone()
            },
            CliStatus {
                installed_version: Some("0.1.5".to_string()),
                ..healthy.clone()
            },
            CliStatus {
                detection_fallback: Some("filesystem"),
                ..healthy
            },
        ];
        for cli in cases {
            let status = status_of(vec![cli]);
            let missing = missing_installs(CliScope::All, &status);
            assert_eq!(missing, vec!["copilot"], "{status:?}");

            let report = build_install_report(CliScope::All, &status, &[], &missing);
            assert_eq!(outcome_of(&report, "copilot"), "failed", "{status:?}");
        }
    }

    #[test]
    fn forced_install_validation_rejects_a_stale_registration_even_at_the_current_version() {
        use crate::agent_hooks_installer::{expected_registration_dir_for, CliKind};

        let expected = expected_registration_dir_for(CliKind::Copilot)
            .expect("the repository supplies the hook bundle");
        let status = status_of(vec![CliStatus {
            marketplace_path: Some(
                expected
                    .with_file_name("previous-hook-bundle")
                    .to_string_lossy()
                    .into_owned(),
            ),
            ..installed_cli("copilot", "0.1.6")
        }]);
        assert_eq!(missing_installs(CliScope::All, &status), vec!["copilot"]);
    }

    #[test]
    fn forced_install_validation_respects_cli_scope_and_preserves_spawn_errors() {
        use crate::agent_hooks_installer::CliKind;

        let status = status_of(vec![
            CliStatus {
                plugin_enabled: false,
                ..installed_cli("copilot", "0.1.6")
            },
            installed_cli("codex", "0.1.6"),
        ]);
        assert_eq!(
            missing_installs(CliScope::One(CliKind::Copilot), &status),
            vec!["copilot"]
        );
        let scope = CliScope::One(CliKind::Codex);
        let missing = missing_installs(scope, &status);
        assert!(missing.is_empty());
        let failures = [failure("codex", "install failed: Access is denied")];
        let report = build_install_report(scope, &status, &failures, &missing);
        assert_eq!(report.clis.len(), 1);
        assert_eq!(outcome_of(&report, "codex"), "failed");
        assert_eq!(report.clis[0].reason, Some(failures[0].reason.clone()));
    }

    /// The automatic reconciliation contract: complete-and-current CLIs drop
    /// out, out-of-date ones are routed to the upgrade flow, and incomplete
    /// installed CLIs are repaired.
    #[test]
    fn install_plans_skip_upgrade_and_install_separately() {
        use crate::agent_hooks_installer::{CliKind, InstallAction};

        let status = status_of(vec![
            installed_cli("copilot", "0.1.6"),
            // Complete but a release behind — `plugin install` would answer
            // "already installed", so this has to go through `plugin update`.
            CliStatus {
                installed_version: Some("0.1.5".to_string()),
                ..cli_with_bundle("claude", Some("0.1.6"))
            },
            // Marketplace registered but the plugin never landed.
            CliStatus {
                plugin_installed: false,
                plugin_enabled: false,
                ..cli_with_bundle("gemini", Some("0.1.6"))
            },
            // Present but disabled — a partial state the button repairs.
            CliStatus {
                plugin_enabled: false,
                ..installed_cli("codex", "0.1.6")
            },
            absent_cli("opencode"),
        ]);

        assert_eq!(
            build_reconciliation_plan(CliScope::All, &status),
            vec![
                (CliKind::Claude, InstallAction::Upgrade),
                (CliKind::Gemini, InstallAction::Install),
                (CliKind::Codex, InstallAction::Install),
            ],
        );
    }

    /// A CLI that is not installed is outside reconciliation. This prevents
    /// automatic triggers from invoking every absent third-party CLI.
    #[test]
    fn reconciliation_skips_absent_clis() {
        let status = status_of(vec![
            installed_cli("copilot", "0.1.6"),
            absent_cli("opencode"),
        ]);
        assert!(build_reconciliation_plan(CliScope::All, &status).is_empty());
    }

    /// `--force` stays a full (re)install — the escape hatch for a break that
    /// status can't see. It must never plan an upgrade because it deliberately
    /// bypasses status-based planning.
    #[test]
    fn a_forced_install_plans_install_for_every_in_scope_cli() {
        use crate::agent_hooks_installer::{CliKind, InstallAction};

        assert_eq!(
            full_install_plan(CliScope::All),
            CliKind::ALL
                .iter()
                .map(|k| (*k, InstallAction::Install))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            full_install_plan(CliScope::One(CliKind::Codex)),
            vec![(CliKind::Codex, InstallAction::Install)]
        );
    }

    /// Scope wins over state in both directions: another CLI needing work must
    /// not widen a `--cli` run, and a complete CLI must still be skipped when
    /// it is the one named.
    #[test]
    fn reconciliation_respects_a_single_cli_scope() {
        use crate::agent_hooks_installer::{CliKind, InstallAction};

        let status = status_of(vec![
            CliStatus {
                plugin_installed: false,
                plugin_enabled: false,
                ..cli_with_bundle("copilot", Some("0.1.6"))
            },
            installed_cli("codex", "0.1.6"),
        ]);

        assert!(build_reconciliation_plan(CliScope::One(CliKind::Codex), &status).is_empty());
        assert_eq!(
            build_reconciliation_plan(CliScope::One(CliKind::Copilot), &status),
            vec![(CliKind::Copilot, InstallAction::Install)]
        );
    }
}
