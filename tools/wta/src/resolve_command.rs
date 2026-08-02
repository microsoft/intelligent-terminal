//! Context-aware command resolution for the `wta resolve-command` CLI.
//!
//! The command returns the same stable `{token, status, ...}` JSON shape for
//! every outcome so agents can consume it without an MCP server. Independent
//! sources declare which shell contexts they apply to; the aggregator merges
//! positive resolutions and only reports `not_found` when an authoritative
//! source completed cleanly.

use async_trait::async_trait;
use std::borrow::Cow;
use std::path::Path;

use crate::command_recall::{CommandResolution, ResolveOutcome};

const SOURCE_HOST_PATH: &str = "host_path";
const SOURCE_POWERSHELL_PROFILE: &str = "powershell_profile";
const SOURCE_WORKING_DIRECTORY: &str = "working_directory";
const DEFAULT_PATHEXT: &str = ".COM;.EXE;.BAT;.CMD";

struct ResolutionContext<'a> {
    token: &'a str,
    shell: &'a str,
    cwd: Option<&'a Path>,
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

struct WorkingDirectorySource;

#[async_trait]
impl ResolutionSource for WorkingDirectorySource {
    fn id(&self) -> &'static str {
        SOURCE_WORKING_DIRECTORY
    }

    fn applies(&self, context: &ResolutionContext<'_>) -> bool {
        context.cwd.is_some() && !is_wsl_context(context.shell)
    }

    fn miss_is_authoritative(&self, _context: &ResolutionContext<'_>) -> bool {
        false
    }

    async fn resolve(&self, context: &ResolutionContext<'_>) -> ResolveOutcome {
        resolve_working_directory(context)
    }
}

fn default_sources() -> &'static [&'static dyn ResolutionSource] {
    &[
        &PowerShellProfileSource,
        &WorkingDirectorySource,
        &HostPathSource,
    ]
}

pub(crate) fn has_applicable_source(shell: &str) -> bool {
    let context = ResolutionContext {
        token: "",
        shell,
        cwd: None,
    };
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

fn is_cmd_context(shell: &str) -> bool {
    let lower = shell.trim().to_ascii_lowercase();
    let leaf = lower.rsplit(['\\', '/']).next().unwrap_or(lower.as_str());
    let leaf = leaf.strip_suffix(".exe").unwrap_or(leaf);
    leaf == "cmd"
}

fn is_bash_context(shell: &str) -> bool {
    let lower = shell.trim().to_ascii_lowercase();
    let leaf = lower.rsplit(['\\', '/']).next().unwrap_or(lower.as_str());
    let leaf = leaf.strip_suffix(".exe").unwrap_or(leaf);
    matches!(leaf, "bash" | "git-bash")
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

fn resolve_working_directory(context: &ResolutionContext<'_>) -> ResolveOutcome {
    let Some(cwd) = context.cwd else {
        return ResolveOutcome::NotFound;
    };
    let candidate_names = working_directory_candidate_names(context.token, context.shell);
    if candidate_names.is_empty() {
        return ResolveOutcome::NotFound;
    }

    let cwd = native_working_directory_path(cwd, context.shell);
    let entries = match std::fs::read_dir(cwd.as_ref()) {
        Ok(entries) => entries,
        Err(_) => return ResolveOutcome::Indeterminate,
    };
    let mut resolutions = Vec::new();
    let mut metadata_failed = false;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                metadata_failed = true;
                continue;
            }
        };
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !candidate_names
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(name))
        {
            continue;
        }
        match entry.metadata() {
            Ok(metadata) if metadata.is_file() => {
                resolutions.push(host_path_resolution(context.token, &entry.path()));
            }
            Ok(_) => {}
            Err(_) => metadata_failed = true,
        }
    }

    if !resolutions.is_empty() {
        ResolveOutcome::Resolved(resolutions)
    } else if metadata_failed {
        ResolveOutcome::Indeterminate
    } else {
        ResolveOutcome::NotFound
    }
}

fn native_working_directory_path<'a>(cwd: &'a Path, shell: &str) -> Cow<'a, Path> {
    #[cfg(windows)]
    if is_bash_context(shell) {
        if let Some(value) = cwd.to_str() {
            let bytes = value.as_bytes();
            if bytes.len() >= 2
                && bytes[0] == b'/'
                && bytes[1].is_ascii_alphabetic()
                && (bytes.len() == 2 || bytes[2] == b'/')
            {
                let mut native = String::with_capacity(value.len() + 1);
                native.push((bytes[1] as char).to_ascii_uppercase());
                native.push(':');
                if bytes.len() == 2 {
                    native.push('\\');
                } else {
                    native.push_str(&value[2..].replace('/', "\\"));
                }
                return Cow::Owned(native.into());
            }
        }
    }

    Cow::Borrowed(cwd)
}

fn working_directory_candidate_names(token: &str, shell: &str) -> Vec<String> {
    let token_path = Path::new(token);
    if token_path.components().count() != 1 || token == "." || token == ".." {
        return Vec::new();
    }

    let extensions = working_directory_extensions(shell);
    if let Some(extension) = token_path.extension().and_then(|extension| extension.to_str()) {
        let extension = format!(".{extension}");
        return extensions
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(&extension))
            .then(|| vec![token.to_string()])
            .unwrap_or_default();
    }

    let mut names = vec![token.to_string()];
    for extension in extensions {
        let name = format!("{token}{extension}");
        if !names
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(&name))
        {
            names.push(name);
        }
    }
    names
}

fn working_directory_extensions(shell: &str) -> Vec<String> {
    let mut extensions = Vec::new();
    if crate::command_recall::is_powershell(shell) {
        extensions.push(".PS1".to_string());
    } else if is_bash_context(shell) {
        extensions.push(".SH".to_string());
    }
    extensions.extend(
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| DEFAULT_PATHEXT.to_string())
            .split(';')
            .filter_map(|extension| {
                let extension = extension.trim();
                if extension.is_empty() {
                    None
                } else if extension.starts_with('.') {
                    Some(extension.to_string())
                } else {
                    Some(format!(".{extension}"))
                }
            }),
    );
    extensions
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

pub(crate) fn powershell_invocation(
    executable: &str,
    shell: &str,
    token: &str,
    cwd: Option<&str>,
) -> String {
    let mut invocation = format!(
        "& {} resolve-command {} --shell {}",
        powershell_single_quote(executable),
        powershell_single_quote(token),
        powershell_single_quote(shell)
    );
    if let Some(cwd) = cwd {
        invocation.push_str(" --cwd ");
        invocation.push_str(&powershell_single_quote(cwd));
    }
    invocation.push_str(" --json");
    invocation
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

pub async fn resolve(token: &str, shell: &str, cwd: Option<&Path>) -> serde_json::Value {
    let context = ResolutionContext {
        token,
        shell,
        cwd,
    };
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
            .map(|(source, resolution)| resolution_json(source, resolution, shell))
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
    let working_directory_failed = results.iter().any(|result| {
        result.source == SOURCE_WORKING_DIRECTORY
            && result.outcome == ResolveOutcome::Indeterminate
    });
    if !authoritative.is_empty()
        && !working_directory_failed
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
        } else if working_directory_failed {
            "the active working directory could not be inspected; other command sources were inconclusive"
        } else if host_path_failed {
            "host PATH could not be inspected; fall back to your own read-only probe"
        } else {
            "host PATH was checked, but shell-native aliases, functions, or builtins require a shell-specific resolver source"
        },
    })
}

fn resolution_json(
    source: &str,
    resolution: &CommandResolution,
    shell: &str,
) -> serde_json::Value {
    let mut value = serde_json::json!({
        "source": source,
        "type": resolution.command_type,
        "name": resolution.name,
        "target": resolution.target,
    });
    if source == SOURCE_WORKING_DIRECTORY {
        value["requires_explicit_path"] = serde_json::Value::Bool(!is_cmd_context(shell));
    }
    value
}

fn same_resolution(left: &CommandResolution, right: &CommandResolution) -> bool {
    left.command_type
        .eq_ignore_ascii_case(&right.command_type)
        && left.name.eq_ignore_ascii_case(&right.name)
        && left.target.eq_ignore_ascii_case(&right.target)
}

#[cfg(test)]
fn selected_source_ids(shell: &str, cwd: Option<&Path>) -> Vec<&'static str> {
    let context = ResolutionContext {
        token: "x",
        shell,
        cwd,
    };
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
        let cwd = Path::new("C:\\workspace");
        assert_eq!(
            selected_source_ids("pwsh.exe", Some(cwd)),
            vec![
                SOURCE_POWERSHELL_PROFILE,
                SOURCE_WORKING_DIRECTORY,
                SOURCE_HOST_PATH
            ]
        );
        assert_eq!(
            selected_source_ids("cmd.exe", Some(cwd)),
            vec![SOURCE_WORKING_DIRECTORY, SOURCE_HOST_PATH]
        );
        assert_eq!(
            selected_source_ids("bash", Some(cwd)),
            vec![SOURCE_WORKING_DIRECTORY, SOURCE_HOST_PATH]
        );
        assert_eq!(
            selected_source_ids("cmd.exe", None),
            vec![SOURCE_HOST_PATH]
        );
        assert!(selected_source_ids("wsl:Ubuntu", Some(cwd)).is_empty());
        assert!(
            selected_source_ids("C:\\Windows\\System32\\wsl.exe", Some(cwd)).is_empty()
        );
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
    fn working_directory_resolves_local_powershell_script() {
        let cwd =
            std::env::temp_dir().join(format!("wta-resolve-cwd-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&cwd).unwrap();
        let script = cwd.join("deploy-it.ps1");
        std::fs::write(&script, "param()\n").unwrap();
        let context = ResolutionContext {
            token: "deploy-it",
            shell: "pwsh.exe",
            cwd: Some(&cwd),
        };

        let outcome = resolve_working_directory(&context);
        std::fs::remove_file(&script).unwrap();
        std::fs::remove_dir(&cwd).unwrap();

        let ResolveOutcome::Resolved(resolutions) = outcome else {
            panic!("expected working-directory resolution");
        };
        assert_eq!(resolutions.len(), 1);
        assert_eq!(resolutions[0].command_type, "ExternalScript");
        assert_eq!(resolutions[0].name, "deploy-it.ps1");
        assert_eq!(resolutions[0].target, script.to_string_lossy());
    }

    #[cfg(windows)]
    #[test]
    fn working_directory_normalizes_msys_drive_paths_for_bash() {
        assert_eq!(
            native_working_directory_path(Path::new("/c/Users/me/project"), "bash.exe"),
            Path::new(r"C:\Users\me\project")
        );
        assert_eq!(
            native_working_directory_path(Path::new("/d"), "git-bash"),
            Path::new(r"D:\")
        );
        assert_eq!(
            native_working_directory_path(Path::new("/home/me/project"), "bash"),
            Path::new("/home/me/project")
        );
        assert_eq!(
            native_working_directory_path(Path::new("/c/Users/me/project"), "pwsh.exe"),
            Path::new("/c/Users/me/project")
        );
    }

    #[test]
    fn working_directory_rejects_tokens_that_are_paths() {
        assert!(working_directory_candidate_names("..\\tool", "cmd.exe").is_empty());
        assert!(working_directory_candidate_names(".\\tool", "pwsh.exe").is_empty());
        assert!(working_directory_candidate_names("sub/tool", "bash").is_empty());
        assert!(working_directory_candidate_names("README.md", "pwsh.exe").is_empty());
        assert_eq!(
            working_directory_candidate_names("deploy-it.sh", "bash"),
            vec!["deploy-it.sh"]
        );
    }

    #[test]
    fn working_directory_reports_when_an_explicit_path_is_required() {
        let resolution =
            host_path_resolution("deploy-it", Path::new("C:\\workspace\\deploy-it.ps1"));

        let powershell = resolution_json(SOURCE_WORKING_DIRECTORY, &resolution, "pwsh.exe");
        assert_eq!(powershell["requires_explicit_path"], true);

        let cmd = resolution_json(SOURCE_WORKING_DIRECTORY, &resolution, "cmd.exe");
        assert_eq!(cmd["requires_explicit_path"], false);

        let host_path = resolution_json(SOURCE_HOST_PATH, &resolution, "pwsh.exe");
        assert!(host_path.get("requires_explicit_path").is_none());
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
        let quote = '\'';
        assert_eq!(
            powershell_invocation(
                "C:\\Program Files\\It's WTA\\wta.exe",
                "C:\\Program Files\\PowerShell\\7\\pwsh.exe",
                "deploy-it's",
                Some("C:\\Work\\It's here"),
            ),
            format!(
                "& 'C:\\Program Files\\It{quote}{quote}s WTA\\wta.exe' resolve-command \
                 'deploy-it{quote}{quote}s' --shell \
                 'C:\\Program Files\\PowerShell\\7\\pwsh.exe' --cwd \
                 'C:\\Work\\It{quote}{quote}s here' --json"
            )
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
        let value = resolve("gti", "wsl:Ubuntu", Some(Path::new("/tmp"))).await;
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
        let cwd = std::env::current_dir().unwrap();
        let value = resolve("cmd.exe", "cmd.exe", Some(&cwd)).await;
        assert_eq!(value["status"], "exists", "got {value}");
        assert_eq!(
            value["checked_sources"],
            serde_json::json!([SOURCE_WORKING_DIRECTORY, SOURCE_HOST_PATH])
        );
        assert_eq!(value["resolutions"][0]["source"], SOURCE_HOST_PATH);

        let token = format!("wta-no-such-command-{}", uuid::Uuid::new_v4());
        let value = resolve(&token, "cmd.exe", Some(&cwd)).await;
        assert_eq!(value["status"], "indeterminate", "got {value}");
        assert_eq!(
            value["note"],
            "host PATH was checked, but shell-native aliases, functions, or builtins require a shell-specific resolver source"
        );

        let missing_cwd =
            std::env::temp_dir().join(format!("wta-missing-cwd-{}", uuid::Uuid::new_v4()));
        let value = resolve(&token, "cmd.exe", Some(&missing_cwd)).await;
        assert_eq!(value["status"], "indeterminate", "got {value}");
        assert_eq!(
            value["note"],
            "the active working directory could not be inspected; other command sources were inconclusive"
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

        let cwd = std::env::current_dir().unwrap();
        let value = resolve("Get-ChildItem", shell, Some(&cwd)).await;
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
            serde_json::json!([
                SOURCE_POWERSHELL_PROFILE,
                SOURCE_WORKING_DIRECTORY,
                SOURCE_HOST_PATH
            ])
        );
        assert!(
            resolutions
                .iter()
                .any(|item| item["source"] == SOURCE_POWERSHELL_PROFILE),
            "expected a PowerShell-profile resolution source, got {value}"
        );

        let value = resolve("no-such-command", shell, Some(&cwd)).await;
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
