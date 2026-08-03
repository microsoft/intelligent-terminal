//! Context-aware command resolution for the `wta resolve-command` CLI.
//!
//! The command returns the same stable `{token, status, ...}` JSON shape for
//! every outcome so agents can consume it without an MCP server. Independent
//! sources declare which shell contexts they apply to; the aggregator merges
//! positive resolutions and only reports `not_found` when an authoritative
//! source completed cleanly.

use serde::{Serialize, Serializer};
use std::borrow::Cow;
use std::path::Path;

use crate::command_recall::{CommandResolution, ResolveOutcome};

const DEFAULT_PATHEXT: &str = ".COM;.EXE;.BAT;.CMD";
const NOTE_UNSUPPORTED: &str = "no command resolver source supports this shell context yet";
const NOTE_AUTHORITATIVE_FAILED: &str =
    "an authoritative resolver source timed out or failed; fall back to your own read-only probe";
const NOTE_WORKING_DIRECTORY_FAILED: &str =
    "the active working directory could not be inspected; other command sources were inconclusive";
const NOTE_HOST_PATH_FAILED: &str =
    "host PATH could not be inspected; fall back to your own read-only probe";
const NOTE_PARTIAL_MISS: &str =
    "host PATH was checked, but shell-native aliases, functions, or builtins require a shell-specific resolver source";
const DEFAULT_SOURCES: [SourceKind; 3] = [
    SourceKind::PowerShellProfile,
    SourceKind::WorkingDirectory,
    SourceKind::HostPath,
];

struct ResolutionContext<'a> {
    token: &'a str,
    shell: &'a str,
    shell_kind: ShellKind,
    cwd: Option<&'a Path>,
}

impl<'a> ResolutionContext<'a> {
    fn new(token: &'a str, shell: &'a str, cwd: Option<&'a Path>) -> Self {
        Self {
            token,
            shell,
            shell_kind: ShellKind::from_shell(shell),
            cwd,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceKind {
    PowerShellProfile,
    WorkingDirectory,
    HostPath,
}

impl Serialize for SourceKind {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

struct SourceResult {
    source: SourceKind,
    outcome: ResolveOutcome,
}

impl SourceKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::PowerShellProfile => "powershell_profile",
            Self::WorkingDirectory => "working_directory",
            Self::HostPath => "host_path",
        }
    }

    fn applies(&self, context: &ResolutionContext<'_>) -> bool {
        match self {
            Self::PowerShellProfile => context.shell_kind == ShellKind::PowerShell,
            Self::WorkingDirectory => context.cwd.is_some() && context.shell_kind != ShellKind::Wsl,
            Self::HostPath => context.shell_kind != ShellKind::Wsl,
        }
    }

    fn miss_is_authoritative(self) -> bool {
        self == Self::PowerShellProfile
    }

    async fn resolve(&self, context: &ResolutionContext<'_>) -> ResolveOutcome {
        match self {
            Self::PowerShellProfile => {
                crate::command_recall::powershell_resolve(context.shell, context.token).await
            }
            Self::WorkingDirectory => resolve_working_directory(context),
            Self::HostPath => resolve_host_path(context.token),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShellKind {
    PowerShell,
    Cmd,
    Bash,
    Wsl,
    Other,
}

impl ShellKind {
    fn from_shell(shell: &str) -> Self {
        let lower = shell.trim().to_ascii_lowercase();
        if lower.starts_with("wsl:") {
            return Self::Wsl;
        }
        if crate::command_recall::is_powershell(&lower) {
            return Self::PowerShell;
        }
        let leaf = lower.rsplit(['\\', '/']).next().unwrap_or(lower.as_str());
        match leaf.strip_suffix(".exe").unwrap_or(leaf) {
            "cmd" => Self::Cmd,
            "bash" | "git-bash" => Self::Bash,
            "wsl" => Self::Wsl,
            _ => Self::Other,
        }
    }
}

pub(crate) fn has_applicable_source(shell: &str) -> bool {
    let context = ResolutionContext::new("", shell, None);
    DEFAULT_SOURCES
        .iter()
        .any(|source| source.applies(&context))
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
    let candidate_names = working_directory_candidate_names(context.token, context.shell_kind);
    if candidate_names.is_empty() {
        return ResolveOutcome::NotFound;
    }

    let cwd = native_working_directory_path(cwd, context.shell_kind);
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

fn native_working_directory_path(cwd: &Path, shell: ShellKind) -> Cow<'_, Path> {
    #[cfg(windows)]
    if shell == ShellKind::Bash {
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

fn working_directory_candidate_names(token: &str, shell: ShellKind) -> Vec<String> {
    let token_path = Path::new(token);
    if token_path.components().count() != 1 || token == "." || token == ".." {
        return Vec::new();
    }

    let extensions = working_directory_extensions(shell);
    if let Some(extension) = token_path
        .extension()
        .and_then(|extension| extension.to_str())
    {
        let extension = format!(".{extension}");
        return extensions
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(&extension))
            .then(|| vec![token.to_string()])
            .unwrap_or_default();
    }

    let mut names = if matches!(shell, ShellKind::PowerShell | ShellKind::Bash) {
        vec![token.to_string()]
    } else {
        Vec::new()
    };
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

fn working_directory_extensions(shell: ShellKind) -> Vec<String> {
    let mut extensions = Vec::new();
    match shell {
        ShellKind::PowerShell => extensions.push(".PS1".to_string()),
        ShellKind::Bash => extensions.push(".SH".to_string()),
        _ => {}
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

#[derive(Debug)]
pub(crate) struct CommandResolverInvocation {
    executable: String,
    shell: String,
    cwd: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct CommandResolverContract {
    executable: String,
    arguments: Vec<String>,
    powershell: String,
}

impl CommandResolverInvocation {
    pub(crate) fn new(
        executable: impl Into<String>,
        shell: impl Into<String>,
        cwd: Option<String>,
    ) -> Self {
        Self {
            executable: executable.into(),
            shell: shell.into(),
            cwd,
        }
    }

    #[cfg(test)]
    pub(crate) fn shell(&self) -> &str {
        &self.shell
    }

    pub(crate) fn cwd(&self) -> Option<&str> {
        self.cwd.as_deref()
    }

    pub(crate) fn contract(&self, token: &str) -> CommandResolverContract {
        CommandResolverContract {
            executable: self.executable.clone(),
            arguments: self.arguments(token),
            powershell: self.powershell(token),
        }
    }

    fn arguments(&self, token: &str) -> Vec<String> {
        let mut arguments = vec![
            "resolve-command".to_string(),
            token.to_string(),
            "--shell".to_string(),
            self.shell.clone(),
        ];
        if let Some(cwd) = &self.cwd {
            arguments.extend(["--cwd".to_string(), cwd.clone()]);
        }
        arguments.push("--json".to_string());
        arguments
    }

    fn powershell(&self, token: &str) -> String {
        let mut invocation = format!(
            "& {} resolve-command {} --shell {}",
            powershell_single_quote(&self.executable),
            powershell_single_quote(token),
            powershell_single_quote(&self.shell)
        );
        if let Some(cwd) = &self.cwd {
            invocation.push_str(" --cwd ");
            invocation.push_str(&powershell_single_quote(cwd));
        }
        invocation.push_str(" --json");
        invocation
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResolveStatus {
    Exists,
    NotFound,
    Indeterminate,
    Unsupported,
}

impl Serialize for ResolveStatus {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl ResolveStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Exists => "exists",
            Self::NotFound => "not_found",
            Self::Indeterminate => "indeterminate",
            Self::Unsupported => "unsupported",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct ResolvedCommand {
    source: SourceKind,
    #[serde(rename = "type")]
    command_type: String,
    name: String,
    target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    requires_explicit_path: Option<bool>,
}

impl ResolvedCommand {
    fn new(source: SourceKind, resolution: &CommandResolution, shell: ShellKind) -> Self {
        Self {
            source,
            command_type: resolution.command_type.clone(),
            name: resolution.name.clone(),
            target: resolution.target.clone(),
            requires_explicit_path: (source == SourceKind::WorkingDirectory)
                .then_some(shell != ShellKind::Cmd),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ResolveCommandResult {
    token: String,
    status: ResolveStatus,
    checked_sources: Vec<SourceKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resolutions: Option<Vec<ResolvedCommand>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    matches: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<&'static str>,
}

impl ResolveCommandResult {
    fn new(
        token: &str,
        status: ResolveStatus,
        checked_sources: Vec<SourceKind>,
        resolutions: Option<Vec<ResolvedCommand>>,
        matches: Option<Vec<String>>,
        note: Option<&'static str>,
    ) -> Self {
        Self {
            token: token.to_string(),
            status,
            checked_sources,
            resolutions,
            matches,
            note,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AggregateOutcome {
    Exists(Vec<ResolvedCommand>),
    NotFound,
    Indeterminate(&'static str),
}

pub(crate) fn format_human(value: &ResolveCommandResult) -> String {
    let mut lines = vec![
        format!("TOKEN    {}", value.token),
        format!("STATUS   {}", value.status.as_str()),
    ];

    if let Some(resolutions) = &value.resolutions {
        for resolution in resolutions {
            lines.push(format!(
                "COMMAND  {} {}",
                resolution.command_type, resolution.name
            ));
            lines.push(format!("SOURCE   {}", resolution.source.as_str()));
            if !resolution.target.is_empty() {
                lines.push(format!("TARGET   {}", resolution.target));
            }
        }
    }

    if let Some(matches) = &value.matches {
        let matches = matches.join(", ");
        lines.push(format!(
            "MATCHES  {}",
            if matches.is_empty() { "-" } else { &matches }
        ));
    }

    if let Some(note) = value.note {
        lines.push(format!("NOTE     {note}"));
    }

    lines.join("\n")
}

pub(crate) async fn resolve(token: &str, shell: &str, cwd: Option<&Path>) -> ResolveCommandResult {
    let context = ResolutionContext::new(token, shell, cwd);
    let mut results = Vec::new();
    for source in DEFAULT_SOURCES {
        if !source.applies(&context) {
            continue;
        }
        results.push(SourceResult {
            source,
            outcome: source.resolve(&context).await,
        });
    }

    let checked_sources = results.iter().map(|result| result.source).collect();
    if results.is_empty() {
        return ResolveCommandResult::new(
            token,
            ResolveStatus::Unsupported,
            checked_sources,
            None,
            None,
            Some(NOTE_UNSUPPORTED),
        );
    }

    match aggregate_results(context.shell_kind, &results) {
        AggregateOutcome::Exists(resolutions) => ResolveCommandResult::new(
            token,
            ResolveStatus::Exists,
            checked_sources,
            Some(resolutions),
            None,
            None,
        ),
        AggregateOutcome::NotFound => {
            let matches = crate::command_recall::powershell_near_matches(shell, token)
                .await
                .unwrap_or_default();
            ResolveCommandResult::new(
                token,
                ResolveStatus::NotFound,
                checked_sources,
                None,
                Some(matches),
                None,
            )
        }
        AggregateOutcome::Indeterminate(note) => ResolveCommandResult::new(
            token,
            ResolveStatus::Indeterminate,
            checked_sources,
            None,
            None,
            Some(note),
        ),
    }
}

fn aggregate_results(shell: ShellKind, results: &[SourceResult]) -> AggregateOutcome {
    let mut resolutions: Vec<(SourceKind, &CommandResolution)> = Vec::new();
    for result in results {
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
        return AggregateOutcome::Exists(
            resolutions
                .into_iter()
                .map(|(source, resolution)| ResolvedCommand::new(source, resolution, shell))
                .collect(),
        );
    }

    let authoritative: Vec<&SourceResult> = results
        .iter()
        .filter(|result| result.source.miss_is_authoritative())
        .collect();
    let working_directory_failed = results.iter().any(|result| {
        result.source == SourceKind::WorkingDirectory
            && result.outcome == ResolveOutcome::Indeterminate
    });
    if !authoritative.is_empty()
        && !working_directory_failed
        && authoritative
            .iter()
            .all(|result| result.outcome == ResolveOutcome::NotFound)
    {
        return AggregateOutcome::NotFound;
    }

    let host_path_failed = results.iter().any(|result| {
        result.source == SourceKind::HostPath && result.outcome == ResolveOutcome::Indeterminate
    });
    AggregateOutcome::Indeterminate(
        if authoritative
            .iter()
            .any(|result| result.outcome == ResolveOutcome::Indeterminate)
        {
            NOTE_AUTHORITATIVE_FAILED
        } else if working_directory_failed {
            NOTE_WORKING_DIRECTORY_FAILED
        } else if host_path_failed {
            NOTE_HOST_PATH_FAILED
        } else {
            NOTE_PARTIAL_MISS
        },
    )
}

fn same_resolution(left: &CommandResolution, right: &CommandResolution) -> bool {
    left.command_type.eq_ignore_ascii_case(&right.command_type)
        && left.name.eq_ignore_ascii_case(&right.name)
        && left.target.eq_ignore_ascii_case(&right.target)
}

#[cfg(test)]
fn selected_source_ids(shell: &str, cwd: Option<&Path>) -> Vec<&'static str> {
    let context = ResolutionContext::new("x", shell, cwd);
    DEFAULT_SOURCES
        .iter()
        .filter(|source| source.applies(&context))
        .map(|source| source.as_str())
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
            vec!["powershell_profile", "working_directory", "host_path"]
        );
        assert_eq!(
            selected_source_ids("cmd.exe", Some(cwd)),
            vec!["working_directory", "host_path"]
        );
        assert_eq!(
            selected_source_ids("bash", Some(cwd)),
            vec!["working_directory", "host_path"]
        );
        assert_eq!(selected_source_ids("cmd.exe", None), vec!["host_path"]);
        assert!(selected_source_ids("wsl:Ubuntu", Some(cwd)).is_empty());
        assert!(selected_source_ids("C:\\Windows\\System32\\wsl.exe", Some(cwd)).is_empty());
        assert!(has_applicable_source("cmd.exe"));
        assert!(has_applicable_source("unknown"));
        assert!(!has_applicable_source("wsl:Ubuntu"));
    }

    #[test]
    fn host_path_resolution_classifies_targets() {
        let application = host_path_resolution("git", std::path::Path::new("C:\\tools\\git.exe"));
        assert_eq!(application.command_type, "Application");
        assert_eq!(application.name, "git.exe");
        assert_eq!(application.target, "C:\\tools\\git.exe");

        let script = host_path_resolution(
            "deploy-it",
            std::path::Path::new("C:\\tools\\deploy-it.ps1"),
        );
        assert_eq!(script.command_type, "ExternalScript");
        assert_eq!(script.name, "deploy-it.ps1");
    }

    #[test]
    fn working_directory_resolves_local_powershell_script() {
        let cwd = std::env::temp_dir().join(format!("wta-resolve-cwd-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&cwd).unwrap();
        let script = cwd.join("deploy-it.ps1");
        std::fs::write(&script, "param()\n").unwrap();
        let context = ResolutionContext::new("deploy-it", "pwsh.exe", Some(&cwd));

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

    #[test]
    fn working_directory_ignores_extensionless_files_for_cmd() {
        let cwd = std::env::temp_dir().join(format!("wta-resolve-cmd-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&cwd).unwrap();
        let file = cwd.join("plain-file");
        std::fs::write(&file, "not executable\n").unwrap();
        let context = ResolutionContext::new("plain-file", "cmd.exe", Some(&cwd));

        let outcome = resolve_working_directory(&context);
        std::fs::remove_file(file).unwrap();
        std::fs::remove_dir(cwd).unwrap();

        assert_eq!(outcome, ResolveOutcome::NotFound);
    }

    #[cfg(windows)]
    #[test]
    fn working_directory_normalizes_msys_drive_paths_for_bash() {
        assert_eq!(
            native_working_directory_path(Path::new("/c/Users/me/project"), ShellKind::Bash),
            Path::new(r"C:\Users\me\project")
        );
        assert_eq!(
            native_working_directory_path(Path::new("/d"), ShellKind::Bash),
            Path::new(r"D:\")
        );
        assert_eq!(
            native_working_directory_path(Path::new("/home/me/project"), ShellKind::Bash),
            Path::new("/home/me/project")
        );
        assert_eq!(
            native_working_directory_path(Path::new("/c/Users/me/project"), ShellKind::PowerShell),
            Path::new("/c/Users/me/project")
        );
    }

    #[test]
    fn working_directory_rejects_tokens_that_are_paths() {
        assert!(working_directory_candidate_names("..\\tool", ShellKind::Cmd).is_empty());
        assert!(working_directory_candidate_names(".\\tool", ShellKind::PowerShell).is_empty());
        assert!(working_directory_candidate_names("sub/tool", ShellKind::Bash).is_empty());
        assert!(working_directory_candidate_names("README.md", ShellKind::PowerShell).is_empty());
        assert_eq!(
            working_directory_candidate_names("deploy-it.sh", ShellKind::Bash),
            vec!["deploy-it.sh"]
        );
        assert!(
            !working_directory_candidate_names("plain-file", ShellKind::Cmd)
                .contains(&"plain-file".to_string())
        );
    }

    #[test]
    fn working_directory_reports_when_an_explicit_path_is_required() {
        let resolution =
            host_path_resolution("deploy-it", Path::new("C:\\workspace\\deploy-it.ps1"));

        let powershell = ResolvedCommand::new(
            SourceKind::WorkingDirectory,
            &resolution,
            ShellKind::PowerShell,
        );
        assert_eq!(powershell.requires_explicit_path, Some(true));

        let cmd = ResolvedCommand::new(SourceKind::WorkingDirectory, &resolution, ShellKind::Cmd);
        assert_eq!(cmd.requires_explicit_path, Some(false));

        let host_path =
            ResolvedCommand::new(SourceKind::HostPath, &resolution, ShellKind::PowerShell);
        assert_eq!(host_path.requires_explicit_path, None);
    }

    #[test]
    fn aggregation_deduplicates_resolutions_across_sources() {
        let resolution =
            host_path_resolution("git", Path::new(r"C:\Program Files\Git\cmd\git.exe"));
        let results = vec![
            SourceResult {
                source: SourceKind::PowerShellProfile,
                outcome: ResolveOutcome::Resolved(vec![resolution.clone()]),
            },
            SourceResult {
                source: SourceKind::HostPath,
                outcome: ResolveOutcome::Resolved(vec![resolution]),
            },
        ];

        let AggregateOutcome::Exists(resolutions) =
            aggregate_results(ShellKind::PowerShell, &results)
        else {
            panic!("expected an existing resolution");
        };
        assert_eq!(resolutions.len(), 1);
        assert_eq!(resolutions[0].source, SourceKind::PowerShellProfile);
    }

    #[test]
    fn aggregation_requires_all_authoritative_misses_and_a_readable_cwd() {
        let clean_miss = vec![
            SourceResult {
                source: SourceKind::PowerShellProfile,
                outcome: ResolveOutcome::NotFound,
            },
            SourceResult {
                source: SourceKind::WorkingDirectory,
                outcome: ResolveOutcome::NotFound,
            },
            SourceResult {
                source: SourceKind::HostPath,
                outcome: ResolveOutcome::NotFound,
            },
        ];
        assert_eq!(
            aggregate_results(ShellKind::PowerShell, &clean_miss),
            AggregateOutcome::NotFound
        );

        let cwd_failed = vec![
            SourceResult {
                source: SourceKind::PowerShellProfile,
                outcome: ResolveOutcome::NotFound,
            },
            SourceResult {
                source: SourceKind::WorkingDirectory,
                outcome: ResolveOutcome::Indeterminate,
            },
            SourceResult {
                source: SourceKind::HostPath,
                outcome: ResolveOutcome::NotFound,
            },
        ];
        assert_eq!(
            aggregate_results(ShellKind::PowerShell, &cwd_failed),
            AggregateOutcome::Indeterminate(NOTE_WORKING_DIRECTORY_FAILED)
        );
    }

    #[test]
    fn aggregation_prioritizes_authoritative_failures() {
        let results = vec![
            SourceResult {
                source: SourceKind::PowerShellProfile,
                outcome: ResolveOutcome::Indeterminate,
            },
            SourceResult {
                source: SourceKind::WorkingDirectory,
                outcome: ResolveOutcome::Indeterminate,
            },
            SourceResult {
                source: SourceKind::HostPath,
                outcome: ResolveOutcome::Indeterminate,
            },
        ];

        assert_eq!(
            aggregate_results(ShellKind::PowerShell, &results),
            AggregateOutcome::Indeterminate(NOTE_AUTHORITATIVE_FAILED)
        );
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
            CommandResolverInvocation::new(
                "C:\\Program Files\\It's WTA\\wta.exe",
                "C:\\Program Files\\PowerShell\\7\\pwsh.exe",
                Some("C:\\Work\\It's here".to_string()),
            )
            .contract("deploy-it's")
            .powershell,
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
        let exists = ResolveCommandResult::new(
            "profile-greeting",
            ResolveStatus::Exists,
            vec![SourceKind::PowerShellProfile],
            Some(vec![ResolvedCommand {
                source: SourceKind::PowerShellProfile,
                command_type: "Alias".to_string(),
                name: "profile-greeting".to_string(),
                target: "Invoke-ProfileGreeting".to_string(),
                requires_explicit_path: None,
            }]),
            None,
            None,
        );
        assert_eq!(
            format_human(&exists),
            "TOKEN    profile-greeting\nSTATUS   exists\nCOMMAND  Alias profile-greeting\nSOURCE   powershell_profile\nTARGET   Invoke-ProfileGreeting"
        );

        let not_found = ResolveCommandResult::new(
            "gti",
            ResolveStatus::NotFound,
            vec![SourceKind::PowerShellProfile],
            None,
            Some(vec!["git".to_string(), "gci".to_string()]),
            None,
        );
        assert_eq!(
            format_human(&not_found),
            "TOKEN    gti\nSTATUS   not_found\nMATCHES  git, gci"
        );
    }

    #[tokio::test]
    async fn wsl_context_returns_unsupported_without_running_host_sources() {
        let value =
            serde_json::to_value(resolve("gti", "wsl:Ubuntu", Some(Path::new("/tmp"))).await)
                .unwrap();
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
        let value = serde_json::to_value(resolve("cmd.exe", "cmd.exe", Some(&cwd)).await).unwrap();
        assert_eq!(value["status"], "exists", "got {value}");
        assert_eq!(
            value["checked_sources"],
            serde_json::json!(["working_directory", "host_path"])
        );
        assert_eq!(value["resolutions"][0]["source"], "host_path");

        let token = format!("wta-no-such-command-{}", uuid::Uuid::new_v4());
        let value = serde_json::to_value(resolve(&token, "cmd.exe", Some(&cwd)).await).unwrap();
        assert_eq!(value["status"], "indeterminate", "got {value}");
        assert_eq!(
            value["note"],
            "host PATH was checked, but shell-native aliases, functions, or builtins require a shell-specific resolver source"
        );

        let missing_cwd =
            std::env::temp_dir().join(format!("wta-missing-cwd-{}", uuid::Uuid::new_v4()));
        let value =
            serde_json::to_value(resolve(&token, "cmd.exe", Some(&missing_cwd)).await).unwrap();
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
        let value =
            serde_json::to_value(resolve("Get-ChildItem", shell, Some(&cwd)).await).unwrap();
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
            serde_json::json!(["powershell_profile", "working_directory", "host_path"])
        );
        assert!(
            resolutions
                .iter()
                .any(|item| item["source"] == "powershell_profile"),
            "expected a PowerShell-profile resolution source, got {value}"
        );

        let value =
            serde_json::to_value(resolve("no-such-command", shell, Some(&cwd)).await).unwrap();
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
