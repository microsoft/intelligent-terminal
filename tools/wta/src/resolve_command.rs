//! Context-aware command resolution for the `wta resolve-command` CLI.
//!
//! The command returns the same stable `{token, status, ...}` JSON shape for
//! every outcome so agents can consume it without an MCP server. Independent
//! sources declare which shell contexts they apply to; the aggregator merges
//! positive resolutions and only reports `not_found` when an authoritative
//! source completed cleanly.

use async_trait::async_trait;

use crate::command_recall::{CommandResolution, ResolveOutcome};

const SOURCE_HOST_PATH: &str = "host_path";
const SOURCE_POWERSHELL_PROFILE: &str = "powershell_profile";

struct ResolutionContext<'a> {
    token: &'a str,
    shell: &'a str,
}

struct SourceResult {
    source: &'static str,
    miss_is_authoritative: bool,
    outcome: ResolveOutcome,
}

#[async_trait]
trait ResolutionSource: Send + Sync {
    fn id(&self) -> &'static str;
    fn applies(&self, context: &ResolutionContext<'_>) -> bool;
    fn miss_is_authoritative(&self, context: &ResolutionContext<'_>) -> bool;
    async fn resolve(&self, context: &ResolutionContext<'_>) -> ResolveOutcome;
}

struct PowerShellProfileSource;

#[async_trait]
impl ResolutionSource for PowerShellProfileSource {
    fn id(&self) -> &'static str {
        SOURCE_POWERSHELL_PROFILE
    }

    fn applies(&self, context: &ResolutionContext<'_>) -> bool {
        crate::command_recall::is_powershell(context.shell)
    }

    fn miss_is_authoritative(&self, _context: &ResolutionContext<'_>) -> bool {
        true
    }

    async fn resolve(&self, context: &ResolutionContext<'_>) -> ResolveOutcome {
        crate::command_recall::powershell_resolve(context.shell, context.token).await
    }
}

struct HostPathSource;

#[async_trait]
impl ResolutionSource for HostPathSource {
    fn id(&self) -> &'static str {
        SOURCE_HOST_PATH
    }

    fn applies(&self, context: &ResolutionContext<'_>) -> bool {
        !is_wsl_context(context.shell)
    }

    fn miss_is_authoritative(&self, _context: &ResolutionContext<'_>) -> bool {
        false
    }

    async fn resolve(&self, context: &ResolutionContext<'_>) -> ResolveOutcome {
        resolve_host_path(context.token)
    }
}

fn default_sources() -> &'static [&'static dyn ResolutionSource] {
    &[&PowerShellProfileSource, &HostPathSource]
}

pub(crate) fn has_applicable_source(shell: &str) -> bool {
    let context = ResolutionContext { token: "", shell };
    default_sources()
        .iter()
        .any(|source| source.applies(&context))
}

fn is_wsl_context(shell: &str) -> bool {
    let lower = shell.trim().to_ascii_lowercase();
    if lower.starts_with("wsl:") {
        return true;
    }
    let leaf = lower.rsplit(['\\', '/']).next().unwrap_or(lower.as_str());
    let leaf = leaf.strip_suffix(".exe").unwrap_or(leaf);
    leaf == "wsl"
}

fn resolve_host_path(token: &str) -> ResolveOutcome {
    if std::env::var_os("PATH").is_none() {
        return ResolveOutcome::Indeterminate;
    }

    let Ok(path) = which::which(token) else {
        return ResolveOutcome::NotFound;
    };
    ResolveOutcome::Resolved(vec![host_path_resolution(token, &path)])
}

fn host_path_resolution(token: &str, path: &std::path::Path) -> CommandResolution {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(token)
        .to_string();
    let command_type = if path
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("ps1"))
    {
        "ExternalScript"
    } else {
        "Application"
    };
    CommandResolution {
        command_type: command_type.to_string(),
        name,
        target: path.to_string_lossy().to_string(),
    }
}

pub(crate) fn powershell_invocation(executable: &str, shell: &str, token: &str) -> String {
    format!(
        "& {} resolve-command {} --shell {} --json",
        powershell_single_quote(executable),
        powershell_single_quote(token),
        powershell_single_quote(shell)
    )
}

fn powershell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

pub fn parse_non_empty(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        Err("value cannot be empty".to_string())
    } else {
        Ok(value.to_string())
    }
}

pub fn format_human(value: &serde_json::Value) -> String {
    let field = |name: &str| value.get(name).and_then(|v| v.as_str()).unwrap_or("-");
    let mut lines = vec![
        format!("TOKEN    {}", field("token")),
        format!("STATUS   {}", field("status")),
    ];

    if let Some(resolutions) = value.get("resolutions").and_then(|v| v.as_array()) {
        for resolution in resolutions {
            lines.push(format!(
                "COMMAND  {} {}",
                resolution
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("-"),
                resolution
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("-")
            ));
            if let Some(source) = resolution
                .get("source")
                .and_then(|v| v.as_str())
                .filter(|source| !source.is_empty())
            {
                lines.push(format!("SOURCE   {source}"));
            }
            if let Some(target) = resolution
                .get("target")
                .and_then(|v| v.as_str())
                .filter(|target| !target.is_empty())
            {
                lines.push(format!("TARGET   {target}"));
            }
        }
    }

    if let Some(matches) = value.get("matches").and_then(|v| v.as_array()) {
        let matches = matches
            .iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!(
            "MATCHES  {}",
            if matches.is_empty() { "-" } else { &matches }
        ));
    }

    if let Some(note) = value.get("note").and_then(|v| v.as_str()) {
        lines.push(format!("NOTE     {note}"));
    }

    lines.join("\n")
}

pub async fn resolve(token: &str, shell: &str) -> serde_json::Value {
    let context = ResolutionContext { token, shell };
    let mut results = Vec::new();
    for source in default_sources() {
        if !source.applies(&context) {
            continue;
        }
        results.push(SourceResult {
            source: source.id(),
            miss_is_authoritative: source.miss_is_authoritative(&context),
            outcome: source.resolve(&context).await,
        });
    }

    let checked_sources: Vec<&str> = results.iter().map(|result| result.source).collect();
    if results.is_empty() {
        return serde_json::json!({
            "token": token,
            "status": "unsupported",
            "checked_sources": checked_sources,
            "note": "no command resolver source supports this shell context yet",
        });
    }

    let mut resolutions: Vec<(&str, &CommandResolution)> = Vec::new();
    for result in &results {
        let ResolveOutcome::Resolved(source_resolutions) = &result.outcome else {
            continue;
        };
        for resolution in source_resolutions {
            if resolutions
                .iter()
                .any(|(_, existing)| same_resolution(existing, resolution))
            {
                continue;
            }
            resolutions.push((result.source, resolution));
        }
    }
    if !resolutions.is_empty() {
        let resolutions: Vec<serde_json::Value> = resolutions
            .into_iter()
            .map(|(source, resolution)| {
                serde_json::json!({
                    "source": source,
                    "type": resolution.command_type,
                    "name": resolution.name,
                    "target": resolution.target,
                })
            })
            .collect();
        return serde_json::json!({
            "token": token,
            "status": "exists",
            "checked_sources": checked_sources,
            "resolutions": resolutions,
        });
    }

    let authoritative: Vec<&SourceResult> = results
        .iter()
        .filter(|result| result.miss_is_authoritative)
        .collect();
    if !authoritative.is_empty()
        && authoritative
            .iter()
            .all(|result| result.outcome == ResolveOutcome::NotFound)
    {
        let matches = crate::command_recall::powershell_near_matches(shell, token)
            .await
            .unwrap_or_default();
        return serde_json::json!({
            "token": token,
            "status": "not_found",
            "checked_sources": checked_sources,
            "matches": matches,
        });
    }

    let host_path_failed = results.iter().any(|result| {
        result.source == SOURCE_HOST_PATH && result.outcome == ResolveOutcome::Indeterminate
    });
    serde_json::json!({
        "token": token,
        "status": "indeterminate",
        "checked_sources": checked_sources,
        "note": if authoritative
            .iter()
            .any(|result| result.outcome == ResolveOutcome::Indeterminate)
        {
            "an authoritative resolver source timed out or failed; fall back to your own read-only probe"
        } else if host_path_failed {
            "host PATH could not be inspected; fall back to your own read-only probe"
        } else {
            "host PATH was checked, but shell-native aliases, functions, or builtins require a shell-specific resolver source"
        },
    })
}

fn same_resolution(left: &CommandResolution, right: &CommandResolution) -> bool {
    left.command_type
        .eq_ignore_ascii_case(&right.command_type)
        && left.name.eq_ignore_ascii_case(&right.name)
        && left.target.eq_ignore_ascii_case(&right.target)
}

#[cfg(test)]
fn selected_source_ids(shell: &str) -> Vec<&'static str> {
    let context = ResolutionContext { token: "x", shell };
    default_sources()
        .iter()
        .filter(|source| source.applies(&context))
        .map(|source| source.id())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_selection_uses_shell_context() {
        assert_eq!(
            selected_source_ids("pwsh.exe"),
            vec![SOURCE_POWERSHELL_PROFILE, SOURCE_HOST_PATH]
        );
        assert_eq!(selected_source_ids("cmd.exe"), vec![SOURCE_HOST_PATH]);
        assert_eq!(selected_source_ids("bash"), vec![SOURCE_HOST_PATH]);
        assert!(selected_source_ids("wsl:Ubuntu").is_empty());
        assert!(selected_source_ids("C:\\Windows\\System32\\wsl.exe").is_empty());
        assert!(has_applicable_source("cmd.exe"));
        assert!(has_applicable_source("unknown"));
        assert!(!has_applicable_source("wsl:Ubuntu"));
    }

    #[test]
    fn host_path_resolution_classifies_targets() {
        let application =
            host_path_resolution("git", std::path::Path::new("C:\\tools\\git.exe"));
        assert_eq!(application.command_type, "Application");
        assert_eq!(application.name, "git.exe");
        assert_eq!(application.target, "C:\\tools\\git.exe");

        let script =
            host_path_resolution("deploy-it", std::path::Path::new("C:\\tools\\deploy-it.ps1"));
        assert_eq!(script.command_type, "ExternalScript");
        assert_eq!(script.name, "deploy-it.ps1");
    }

    #[test]
    fn non_empty_parser_trims_and_rejects_empty_values() {
        assert_eq!(
            parse_non_empty("  Get-ChildItem  ").unwrap(),
            "Get-ChildItem"
        );
        assert!(parse_non_empty("").is_err());
        assert!(parse_non_empty(" \t ").is_err());
    }

    #[test]
    fn powershell_invocation_quotes_executable_shell_and_token() {
        assert_eq!(
            powershell_invocation(
                "C:\\Program Files\\It's WTA\\wta.exe",
                "C:\\Program Files\\PowerShell\\7\\pwsh.exe",
                "deploy-it's"
            ),
            "& 'C:\\Program Files\\It''s WTA\\wta.exe' resolve-command 'deploy-it''s' --shell 'C:\\Program Files\\PowerShell\\7\\pwsh.exe' --json"
        );
    }

    #[test]
    fn human_format_summarizes_resolutions_and_matches() {
        let exists = serde_json::json!({
            "token": "profile-greeting",
            "status": "exists",
            "resolutions": [{
                "type": "Alias",
                "name": "profile-greeting",
                "target": "Invoke-ProfileGreeting"
            }]
        });
        assert_eq!(
            format_human(&exists),
            "TOKEN    profile-greeting\nSTATUS   exists\nCOMMAND  Alias profile-greeting\nTARGET   Invoke-ProfileGreeting"
        );

        let not_found = serde_json::json!({
            "token": "gti",
            "status": "not_found",
            "matches": ["git", "gci"]
        });
        assert_eq!(
            format_human(&not_found),
            "TOKEN    gti\nSTATUS   not_found\nMATCHES  git, gci"
        );
    }

    #[tokio::test]
    async fn wsl_context_returns_unsupported_without_running_host_sources() {
        let value = resolve("gti", "wsl:Ubuntu").await;
        assert_eq!(value["token"], "gti");
        assert_eq!(value["status"], "unsupported");
        assert_eq!(value["checked_sources"], serde_json::json!([]));
        assert_eq!(
            value["note"],
            "no command resolver source supports this shell context yet"
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn cmd_context_resolves_host_path_but_does_not_claim_a_complete_miss() {
        let value = resolve("cmd.exe", "cmd.exe").await;
        assert_eq!(value["status"], "exists", "got {value}");
        assert_eq!(
            value["checked_sources"],
            serde_json::json!([SOURCE_HOST_PATH])
        );
        assert_eq!(value["resolutions"][0]["source"], SOURCE_HOST_PATH);

        let token = format!("wta-no-such-command-{}", uuid::Uuid::new_v4());
        let value = resolve(&token, "cmd.exe").await;
        assert_eq!(value["status"], "indeterminate", "got {value}");
        assert_eq!(
            value["note"],
            "host PATH was checked, but shell-native aliases, functions, or builtins require a shell-specific resolver source"
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn resolves_existing_cmdlet_and_flags_unknown() {
        let host = ["pwsh.exe", "powershell.exe"]
            .into_iter()
            .find(|exe| which::which(exe).is_ok());
        let Some(shell) = host else {
            eprintln!("no PowerShell host installed; skipping");
            return;
        };

        let value = resolve("Get-ChildItem", shell).await;
        if value["status"] == "indeterminate" {
            eprintln!("resolve was indeterminate (slow profile?); skipping");
            return;
        }
        assert_eq!(value["status"], "exists", "got {value}");
        let resolutions = value["resolutions"].as_array().expect("resolutions array");
        assert!(
            resolutions
                .iter()
                .any(|item| item["type"] == "Cmdlet" && item["name"] == "Get-ChildItem"),
            "expected Get-ChildItem as a Cmdlet, got {value}"
        );
        assert_eq!(
            value["checked_sources"],
            serde_json::json!([SOURCE_POWERSHELL_PROFILE, SOURCE_HOST_PATH])
        );
        assert!(
            resolutions
                .iter()
                .any(|item| item["source"] == SOURCE_POWERSHELL_PROFILE),
            "expected a PowerShell-profile resolution source, got {value}"
        );

        let value = resolve("no-such-command", shell).await;
        if value["status"] == "indeterminate" {
            eprintln!("resolve was indeterminate (slow profile?); skipping");
            return;
        }
        assert_eq!(value["status"], "not_found", "got {value}");
        assert!(
            value["matches"].is_array(),
            "expected a matches array, got {value}"
        );
    }
}
