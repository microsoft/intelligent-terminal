use anyhow::Result;

use super::args::HooksCliFilter;

pub(crate) fn run_install(cli: HooksCliFilter) -> Result<()> {
    // Logging is initialized in `main()`; the install attempt is observable in
    // %LOCALAPPDATA%\IntelligentTerminal\logs\wta-install-hooks.log.
    let scope = cli.into_scope();
    let spawn_failures = crate::agent_hooks_installer::ensure_installed_scoped(scope);

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
    let report = crate::agent_hooks_installer::status_scoped(scope);
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

    if spawn_failures.is_empty() && missing.is_empty() {
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
            source = r.bundle_source.kind,
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
        println!("  {:<10} {:<28}  ({})", c.name, summary, detail);
        if let Some(p) = c.marketplace_path.as_deref() {
            println!("    path: {}", p);
        }
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
    use super::format_install_failure;
    use crate::agent_hooks_installer::InstallFailure;

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
}
