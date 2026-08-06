//! Shared agent-process spawn logic for the ACP layer.
//!
//! Both [`crate::master`] (spawning the shared agent CLI) and
//! [`super::probe::probe_models`] share command parsing, executable
//! resolution, stdio, and lifecycle setup. They choose different explicit
//! child-environment policies: normal master startup may apply the shared
//! provider, while cloud model discovery removes every provider override.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use tokio::io::{AsyncBufReadExt, BufReader};

const STARTUP_STDERR_MAX_LINES: usize = 32;
const STARTUP_STDERR_MAX_CHARS_PER_LINE: usize = 1024;
const STARTUP_STDERR_DRAIN_TIMEOUT: Duration = Duration::from_millis(250);
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Provider configuration inherited by an ACP child.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChildEnvironmentPolicy {
    /// Normal agent startup: scrub WTA-only metadata, then inject a complete
    /// shared provider configuration when the authoritative agent supports it.
    ApplySharedProvider,
    /// Cloud catalog discovery: remove both shared and agent-native provider
    /// overrides so the probe cannot rediscover the selected shared provider.
    CleanCloudDiscovery,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StderrPhase {
    Startup,
    Running,
    Failed,
}

#[derive(Debug)]
struct AgentStderrLogInner {
    phase: StderrPhase,
    startup_lines: VecDeque<String>,
}

/// Keeps routine agent stderr at debug level while preserving startup failures
/// in release logs. Only the bounded pre-initialize buffer is promoted.
#[derive(Clone)]
pub(crate) struct AgentStderrLog {
    agent: Arc<str>,
    inner: Arc<Mutex<AgentStderrLogInner>>,
}

impl AgentStderrLog {
    pub(crate) fn new(agent: impl Into<Arc<str>>) -> Self {
        Self {
            agent: agent.into(),
            inner: Arc::new(Mutex::new(AgentStderrLogInner {
                phase: StderrPhase::Startup,
                startup_lines: VecDeque::with_capacity(STARTUP_STDERR_MAX_LINES),
            })),
        }
    }

    pub(crate) fn drain(&self, stderr: tokio::process::ChildStderr) -> tokio::task::JoinHandle<()> {
        let log = self.clone();
        tokio::task::spawn_local(async move {
            let mut lines = BufReader::new(stderr).lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => log.log_line(&line),
                    Ok(None) => break,
                    Err(error) => {
                        log.log_line(&format!("failed to read agent stderr: {error}"));
                        break;
                    }
                }
            }
        })
    }

    pub(crate) async fn finish_failed_startup(
        &self,
        child: &mut tokio::process::Child,
        stderr_task: Option<tokio::task::JoinHandle<()>>,
    ) {
        let _ = child.start_kill();
        if let Some(mut stderr_task) = stderr_task {
            if tokio::time::timeout(STARTUP_STDERR_DRAIN_TIMEOUT, &mut stderr_task)
                .await
                .is_err()
            {
                stderr_task.abort();
                let _ = stderr_task.await;
            }
        }
        self.mark_failed();
    }

    pub(crate) fn mark_initialized(&self) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        inner.phase = StderrPhase::Running;
        inner.startup_lines.clear();
    }

    pub(crate) fn mark_failed(&self) {
        let startup_lines = {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if inner.phase != StderrPhase::Startup {
                return;
            }
            inner.phase = StderrPhase::Failed;
            inner.startup_lines.drain(..).collect::<Vec<_>>()
        };

        if !startup_lines.is_empty() {
            tracing::warn!(
                target: "agent_stderr",
                agent = %self.agent,
                phase = "startup_failure",
                captured_lines = startup_lines.len(),
                "agent startup failed; promoting captured stderr"
            );
        }
        for line in startup_lines {
            tracing::warn!(
                target: "agent_stderr",
                agent = %self.agent,
                phase = "startup_failure",
                "{line}"
            );
        }
    }

    fn log_line(&self, line: &str) {
        {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match inner.phase {
                StderrPhase::Startup => {
                    if inner.startup_lines.len() == STARTUP_STDERR_MAX_LINES {
                        inner.startup_lines.pop_front();
                    }
                    inner.startup_lines.push_back(truncate_stderr_line(line));
                }
                StderrPhase::Running | StderrPhase::Failed => {}
            }
        }

        tracing::debug!(target: "agent_stderr", agent = %self.agent, "{line}");
    }
}

fn truncate_stderr_line(line: &str) -> String {
    const ELLIPSIS: &str = "...";

    if line.chars().count() <= STARTUP_STDERR_MAX_CHARS_PER_LINE {
        return line.to_string();
    }

    let mut truncated = line
        .chars()
        .take(STARTUP_STDERR_MAX_CHARS_PER_LINE - ELLIPSIS.chars().count())
        .collect::<String>();
    truncated.push_str(ELLIPSIS);
    truncated
}

pub(crate) struct AgentSpawn {
    pub child: tokio::process::Child,
    /// Original first token of `agent_cmd`, before path resolution.
    pub raw_program: String,
    /// Resolved program path (post `resolve_bare_agent_name`).
    pub resolved_program: String,
    /// True when the resolved program is an `npx` launcher. Callers
    /// stretch their initialize timeout when this is set — first npx
    /// run downloads the adapter package.
    pub is_npx: bool,
    /// For npx launches, the first `@`-prefixed arg (the adapter
    /// package id, e.g. `@zed-industries/claude-code-acp`).
    pub adapter_package: Option<String>,
}

impl AgentSpawn {
    /// Human-readable agent label for error messages. Prefers the npx
    /// adapter package id when present.
    pub fn label(&self) -> &str {
        self.adapter_package.as_deref().unwrap_or(&self.raw_program)
    }
}

/// Spawn the ACP agent process.
///
/// `cwd` pins the child's working directory to the user's active pane cwd so
/// the agent's `execute_command` tool — which inherits the agent process cwd
/// when its shell wrapper doesn't explicitly set one — starts in the user's
/// project. None preserves the parent's cwd (probe path, where it doesn't
/// matter). `agent_id` is authoritative when supplied by the master, so a
/// custom command whose executable happens to match a built-in agent cannot
/// inherit that built-in's BYOK configuration.
pub(crate) fn spawn_agent_process(
    agent_cmd: &str,
    cwd: Option<&Path>,
    agent_id: Option<&str>,
    environment_policy: ChildEnvironmentPolicy,
) -> Result<AgentSpawn> {
    let parts: Vec<&str> = agent_cmd.split_whitespace().collect();
    let raw_program = parts
        .first()
        .copied()
        .ok_or_else(|| anyhow!("empty agent command"))?;
    let args: Vec<&str> = parts[1..].to_vec();
    let resolved_program = crate::agent_registry::resolve_bare_agent_name(raw_program);
    let needs_cmd = crate::coordinator::needs_shell_launch(&resolved_program);

    let is_npx = resolved_program.eq_ignore_ascii_case("npx")
        || resolved_program.eq_ignore_ascii_case("npx.cmd")
        || resolved_program.eq_ignore_ascii_case("npx.exe");
    let adapter_package = if is_npx {
        args.iter()
            .find(|a| a.starts_with('@'))
            .map(|s| s.to_string())
    } else {
        None
    };

    let program = if needs_cmd {
        "cmd"
    } else {
        resolved_program.as_str()
    };
    let mut cmd = tokio::process::Command::new(program);
    if needs_cmd {
        cmd.arg("/c").arg(&resolved_program);
    }

    // The claude-code-acp adapter refuses to start when its recursion-
    // guard env var is set — that guard exists to block recursive
    // `claude` shells from sharing runtime, but doesn't apply to an
    // ACP host. Scrub unconditionally; other agents don't care.
    cmd.env_remove("CLAUDECODE");
    configure_child_environment(&mut cmd, agent_cmd, agent_id, environment_policy)?;

    // Give the agent CLI a fresh PATH and make this package's `wta.exe`
    // App Execution Alias the first match. The package-specific alias directory
    // avoids collisions when Dev, Preview, and Store builds are installed
    // together. Unpackaged builds prepend the running binary's directory.
    //
    // The agent's tool shells inherit this environment, so prompt contracts can
    // invoke short `wta.exe` while still selecting this exact WTA installation.
    let base_path = crate::agent_check::spawn_path()
        .map(std::ffi::OsString::from)
        .or_else(|| std::env::var_os("PATH"));
    if let Some(base_path) = base_path.as_ref() {
        cmd.env("PATH", base_path);
    }
    match wta_cli_directory().and_then(|directory| {
        prepend_directory_to_path(&directory, base_path.as_deref().unwrap_or_default())
    }) {
        Ok(path) => {
            cmd.env("PATH", path);
        }
        Err(error) => {
            tracing::warn!(
                target: "acp.spawn",
                %error,
                "wta_cli_alias_path_unavailable"
            );
        }
    }
    cmd.env("WTA_CLI_PATH", wta_cli_directory()?.join("wta.exe"));

    // Tell the agent CLI's hook scripts (`send-event.ps1`, inherited via the
    // CLI → node → powershell process chain) where to write their diagnostic
    // trace. PowerShell can't resolve our package-private log dir on its own
    // (it only sees the un-redirected `%LOCALAPPDATA%`, and doesn't know the
    // package family name), so we hand it the already-resolved path. The hook
    // falls back to bare `%LOCALAPPDATA%\IntelligentTerminal\logs` when this
    // is unset (unpackaged dev runs, or an older wta that didn't set it).
    // Versioned dir (`logs\<pkgver>\`) via the shared resolver so the hooks'
    // `hook-trace.log` lands alongside this build's Rust + C++ logs.
    cmd.env("WTA_HOOK_LOG_DIR", crate::logging::log_dir());

    // Forward the user's locale to the agent process via standard POSIX
    // environment variables. Many agent CLIs (and the large language models
    // they speak to) honor `LANG` / `LC_ALL` to choose their response
    // language. We keep the ACP wire format itself untouched — this is
    // purely a hint for the agent's response language. Format:
    // `<lang>_<REGION>.UTF-8` (BCP-47 with dashes converted to underscores;
    // language lowercase, 2-letter region uppercase, plus UTF-8 codeset).
    //
    // Don't override the user's own locale env vars: if the user has
    // intentionally pinned `LANG` or `LC_ALL` in their shell (e.g. to
    // get an English-only Copilot response while running a zh-CN UI),
    // respect that. Only set what's missing.
    //
    // Pseudo-locales (`qps-ploc*`) are UI-only and not real BCP-47 tags;
    // forwarding them verbatim would make agent CLIs warn or fall back.
    // Map them to en_US.UTF-8 so the agent gets a real locale.
    {
        let current_locale = rust_i18n::locale().to_string();
        if !current_locale.is_empty() {
            let posix_locale = if current_locale.starts_with("qps-") {
                "en_US.UTF-8".to_string()
            } else {
                canonicalize_posix_locale(&current_locale)
            };
            if std::env::var_os("LANG").is_none() {
                cmd.env("LANG", &posix_locale);
            }
            if std::env::var_os("LC_ALL").is_none() {
                cmd.env("LC_ALL", &posix_locale);
            }
        }
    }
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    let child = cmd
        .args(&args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| anyhow!("failed to spawn agent '{}': {}", agent_cmd, e))?;

    Ok(AgentSpawn {
        child,
        raw_program: raw_program.to_string(),
        resolved_program,
        is_npx,
        adapter_package,
    })
}

fn wta_cli_directory() -> Result<PathBuf> {
    let package_family = crate::runtime_paths::current_package_family_name();
    let local_app_data = std::env::var_os("LOCALAPPDATA");
    let current_exe =
        std::env::current_exe().context("failed to resolve the running wta executable")?;
    wta_cli_directory_for(
        package_family.as_deref(),
        local_app_data.as_deref(),
        &current_exe,
    )
}

fn wta_cli_directory_for(
    package_family: Option<&std::ffi::OsStr>,
    local_app_data: Option<&std::ffi::OsStr>,
    current_exe: &Path,
) -> Result<PathBuf> {
    if let Some(package_family) = package_family {
        let local_app_data = local_app_data
            .context("LOCALAPPDATA is required to resolve the packaged wta execution alias")?;
        return Ok(PathBuf::from(local_app_data)
            .join("Microsoft")
            .join("WindowsApps")
            .join(package_family));
    }

    current_exe
        .parent()
        .map(Path::to_path_buf)
        .context("running wta executable has no parent directory")
}

fn prepend_directory_to_path(
    directory: &Path,
    base_path: &std::ffi::OsStr,
) -> Result<std::ffi::OsString> {
    std::env::join_paths(
        std::iter::once(directory.to_path_buf()).chain(std::env::split_paths(base_path)),
    )
    .context("failed to prepend the wta CLI directory to the agent PATH")
}

fn resolve_spawn_profile(
    agent_cmd: &str,
    agent_id: Option<&str>,
) -> &'static crate::agent_registry::AgentProfile {
    let agent_id =
        agent_id.unwrap_or_else(|| crate::agent_registry::resolve_agent_id_from_cmd(agent_cmd));
    crate::agent_registry::lookup_profile_by_id(agent_id)
}

fn configure_child_environment(
    command: &mut tokio::process::Command,
    agent_cmd: &str,
    agent_id: Option<&str>,
    environment_policy: ChildEnvironmentPolicy,
) -> Result<Option<crate::agent_registry::ByokMode>> {
    match environment_policy {
        ChildEnvironmentPolicy::ApplySharedProvider => {
            let profile = resolve_spawn_profile(agent_cmd, agent_id);
            crate::custom_model_provider::configure_child(command, profile.byok_mode)
        }
        ChildEnvironmentPolicy::CleanCloudDiscovery => {
            crate::custom_model_provider::scrub_child_for_cloud_discovery(command);
            Ok(None)
        }
    }
}

/// Spawn an ACP agent in the selected per-tab execution source.
pub(crate) fn spawn_agent_process_for_source(
    agent_cmd: &str,
    cwd: Option<&Path>,
    agent_id: Option<&str>,
    source: &crate::agent_source::AgentSource,
    environment_policy: ChildEnvironmentPolicy,
) -> Result<AgentSpawn> {
    match source {
        crate::agent_source::AgentSource::Host => {
            spawn_agent_process(agent_cmd, cwd, agent_id, environment_policy)
        }
        crate::agent_source::AgentSource::Wsl { distro } => {
            spawn_wsl_agent_process(agent_cmd, distro, agent_id, environment_policy)
        }
    }
}

fn spawn_wsl_agent_process(
    agent_cmd: &str,
    distro: &str,
    _agent_id: Option<&str>,
    environment_policy: ChildEnvironmentPolicy,
) -> Result<AgentSpawn> {
    let parts = crate::coordinator::split_windows_commandline(agent_cmd);
    let raw_program = parts
        .first()
        .cloned()
        .ok_or_else(|| anyhow!("empty agent command"))?;
    let is_npx = ["npx", "npx.cmd", "npx.exe"]
        .iter()
        .any(|name| raw_program.eq_ignore_ascii_case(name));
    let adapter_package = is_npx
        .then(|| parts.iter().find(|arg| arg.starts_with('@')).cloned())
        .flatten();
    let script = wsl_agent_launch_script(&parts, environment_policy);

    let mut command = tokio::process::Command::new("wsl.exe");
    command
        .arg("-d")
        .arg(distro)
        .arg("--")
        .arg("bash")
        .arg("--noprofile")
        .arg("--norc")
        .arg("-c")
        .arg(script)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    // Shared-provider configuration is Host-only. Remove any Windows-side
    // provider variables (including preexisting WSLENV forwarding) while
    // leaving the distro's own environment untouched for normal launches.
    // Clean probes additionally unset provider keys inside the Linux shell via
    // `wsl_agent_launch_script`.
    crate::custom_model_provider::scrub_child_for_cloud_discovery(&mut command);
    scrub_provider_environment_from_wslenv(&mut command);
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);

    let child = command.spawn().map_err(|error| {
        anyhow!(
            "failed to spawn agent '{}' in WSL distro '{}': {}",
            agent_cmd,
            distro,
            error
        )
    })?;

    Ok(AgentSpawn {
        child,
        raw_program,
        resolved_program: format!("wsl.exe -d {distro}"),
        is_npx,
        adapter_package,
    })
}

fn scrub_provider_environment_from_wslenv(command: &mut tokio::process::Command) {
    let wslenv = remove_wslenv_names(
        &std::env::var("WSLENV").unwrap_or_default(),
        crate::custom_model_provider::cloud_discovery_environment_keys(),
    );
    command.env("WSLENV", wslenv);
}

fn remove_wslenv_names(current: &str, names: &[&str]) -> String {
    wslenv_entries(current)
        .into_iter()
        .filter(|entry| {
            !names
                .iter()
                .any(|name| wslenv_name(entry).eq_ignore_ascii_case(name))
        })
        .collect::<Vec<_>>()
        .join(":")
}

fn wslenv_entries(current: &str) -> Vec<String> {
    current
        .split(':')
        .filter(|entry| !entry.is_empty())
        .map(str::to_string)
        .collect()
}

fn wslenv_name(entry: &str) -> &str {
    entry.split('/').next().unwrap_or(entry)
}

fn wsl_agent_launch_script(parts: &[String], environment_policy: ChildEnvironmentPolicy) -> String {
    let invocation = parts
        .iter()
        .map(|part| crate::coordinator::sh_quote(part))
        .collect::<Vec<_>>()
        .join(" ");
    let mut unset_names = vec!["CLAUDECODE"];
    if environment_policy == ChildEnvironmentPolicy::CleanCloudDiscovery {
        unset_names
            .extend_from_slice(crate::custom_model_provider::cloud_discovery_environment_keys());
    }
    let inner = format!(
        "exec 1>&3 3>&-; unset {}; exec {invocation}",
        unset_names.join(" ")
    );
    format!(
        "bash -lc {} 3>&1 >/dev/null",
        crate::coordinator::sh_quote(&inner)
    )
}

/// Convert a BCP-47 locale tag (e.g. `zh-CN`, `gd-gb`) to the POSIX
/// `LANG`/`LC_ALL` form (e.g. `zh_CN.UTF-8`, `gd_GB.UTF-8`).
///
/// POSIX runtimes expect language lowercase + region uppercase (e.g.
/// `gd_GB.UTF-8`, not `gd_gb.UTF-8`). Our locale folder names are
/// canonical BCP-47 except for a few inconsistencies like `gd-gb` —
/// canonicalize defensively rather than depending on every locale file
/// being named correctly.
///
/// Rules:
/// - Split on `-`. First segment is always the language, lowercased.
/// - Two-letter segments after the first are treated as ISO 3166-1
///   region codes and converted to uppercase.
/// - Four-letter segments are treated as ISO 15924 script codes and
///   title-cased (e.g. `Latn`, `Cyrl`).
/// - Anything else (numeric region, variant) is left as-is.
/// - Joined with `_` per POSIX convention; UTF-8 codeset appended.
fn canonicalize_posix_locale(tag: &str) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(3);
    for (i, seg) in tag.split('-').enumerate() {
        if i == 0 {
            parts.push(seg.to_lowercase());
        } else if seg.len() == 2 && seg.chars().all(|c| c.is_ascii_alphabetic()) {
            parts.push(seg.to_uppercase());
        } else if seg.len() == 4 && seg.chars().all(|c| c.is_ascii_alphabetic()) {
            let mut chars = seg.chars();
            let first = chars.next().unwrap().to_ascii_uppercase();
            let rest: String = chars.map(|c| c.to_ascii_lowercase()).collect();
            parts.push(format!("{}{}", first, rest));
        } else {
            parts.push(seg.to_string());
        }
    }
    format!("{}.UTF-8", parts.join("_"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_agent_id_overrides_command_inference() {
        assert_eq!(
            resolve_spawn_profile("copilot --acp --stdio", None).byok_mode,
            crate::agent_registry::ByokMode::CopilotProviderEnvironment
        );
        assert_eq!(
            resolve_spawn_profile("copilot --acp --stdio", Some("custom:test-agent")).byok_mode,
            crate::agent_registry::ByokMode::Unsupported
        );
    }

    #[test]
    fn normal_wsl_launch_preserves_only_the_distros_provider_environment() {
        let script = wsl_agent_launch_script(
            &["copilot".to_string(), "--acp".to_string()],
            ChildEnvironmentPolicy::ApplySharedProvider,
        );

        assert!(script.contains("unset CLAUDECODE"));
        for key in crate::custom_model_provider::cloud_discovery_environment_keys() {
            assert!(
                !script.contains(key),
                "normal WSL launch must preserve the distro's own {key}: {script}"
            );
        }
    }

    #[test]
    fn clean_cloud_policy_scrubs_builtin_provider_environment() {
        let mut command = tokio::process::Command::new("cloud-probe");
        for key in crate::custom_model_provider::cloud_discovery_environment_keys() {
            command.env(key, "must-not-leak");
        }

        let applied = configure_child_environment(
            &mut command,
            "copilot --acp --stdio",
            Some("copilot"),
            ChildEnvironmentPolicy::CleanCloudDiscovery,
        )
        .expect("clean cloud policy should not require shared configuration");

        assert_eq!(applied, None);
        let configured_env: std::collections::HashMap<_, _> = command.as_std().get_envs().collect();
        for key in crate::custom_model_provider::cloud_discovery_environment_keys() {
            assert_eq!(
                configured_env.get(std::ffi::OsStr::new(key)),
                Some(&None),
                "cloud policy must scrub {key}"
            );
        }
    }

    #[test]
    fn packaged_wta_cli_uses_package_specific_execution_alias() {
        let directory = wta_cli_directory_for(
            Some(std::ffi::OsStr::new("IntelligentTerminal_test")),
            Some(std::ffi::OsStr::new(r"C:\Users\test\AppData\Local")),
            Path::new(r"C:\Program Files\WindowsApps\package\wta.exe"),
        )
        .unwrap();

        assert_eq!(
            directory,
            PathBuf::from(
                r"C:\Users\test\AppData\Local\Microsoft\WindowsApps\IntelligentTerminal_test"
            )
        );
    }

    #[test]
    fn unpackaged_wta_cli_uses_running_executable_directory() {
        let current_exe = Path::new(r"C:\src\wta\target\debug\wta.exe");
        let directory = wta_cli_directory_for(None, None, current_exe).unwrap();

        assert_eq!(directory, PathBuf::from(r"C:\src\wta\target\debug"));
    }

    #[test]
    fn wta_cli_directory_is_first_on_agent_path() {
        let directory = Path::new(r"C:\package-alias");
        let path = prepend_directory_to_path(
            directory,
            std::ffi::OsStr::new(r"C:\Windows\System32;C:\Tools"),
        )
        .unwrap();
        let entries = std::env::split_paths(&path).collect::<Vec<_>>();

        assert_eq!(entries[0], directory);
        assert_eq!(entries[1], Path::new(r"C:\Windows\System32"));
        assert_eq!(entries[2], Path::new(r"C:\Tools"));
    }

    #[test]
    fn wsl_launch_script_quotes_every_agent_argument() {
        let parts = vec![
            "copilot".to_string(),
            "--model".to_string(),
            "model; touch /tmp/nope".to_string(),
        ];
        assert_eq!(
            wsl_agent_launch_script(&parts, ChildEnvironmentPolicy::ApplySharedProvider),
            "bash -lc 'exec 1>&3 3>&-; unset CLAUDECODE; exec '\\''copilot'\\'' \
             '\\''--model'\\'' '\\''model; touch /tmp/nope'\\''' 3>&1 >/dev/null"
        );
    }

    #[test]
    fn wsl_forwarding_names_match_each_shared_provider_mode() {
        use crate::agent_registry::ByokMode;

        let cases = [
            (
                ByokMode::CopilotProviderEnvironment,
                vec![
                    "COPILOT_PROVIDER_BASE_URL",
                    "COPILOT_PROVIDER_API_KEY",
                    "COPILOT_PROVIDER_TYPE",
                    "COPILOT_MODEL",
                    "COPILOT_OFFLINE",
                ],
            ),
            (
                ByokMode::OpenCodeConfigContent,
                vec![
                    "OPENCODE_CONFIG_CONTENT",
                    "INTELLIGENT_TERMINAL_MODEL_API_KEY",
                ],
            ),
            (ByokMode::Unsupported, Vec::new()),
        ];

        for (mode, expected) in cases {
            assert_eq!(
                crate::custom_model_provider::shared_provider_environment_keys(mode),
                expected
            );
        }
    }

    #[test]
    fn wslenv_host_isolation_removes_shared_provider_forwarding() {
        let current = "PATH/p:copilot_provider_base_url:\
                       INTELLIGENT_TERMINAL_MODEL_API_KEY:KEEP/u";
        let isolated = remove_wslenv_names(
            current,
            crate::custom_model_provider::cloud_discovery_environment_keys(),
        );

        assert_eq!(isolated, "PATH/p:KEEP/u");
    }

    #[test]
    fn wslenv_clean_cloud_discovery_removes_all_provider_forwarding() {
        let current = "PATH/p:WTA_CUSTOM_MODEL_BASE_URL:\
                       COPILOT_PROVIDER_API_KEY:OPENCODE_CONFIG_CONTENT:KEEP/u";
        let cleaned = remove_wslenv_names(
            current,
            crate::custom_model_provider::cloud_discovery_environment_keys(),
        );

        assert_eq!(cleaned, "PATH/p:KEEP/u");
    }

    #[test]
    fn clean_cloud_wsl_script_unsets_provider_configuration() {
        let script = wsl_agent_launch_script(
            &["copilot".to_string(), "--acp".to_string()],
            ChildEnvironmentPolicy::CleanCloudDiscovery,
        );

        for key in crate::custom_model_provider::cloud_discovery_environment_keys() {
            assert!(
                script.contains(key),
                "clean WSL probe must unset {key}: {script}"
            );
        }
        assert!(script.contains("unset CLAUDECODE"));
    }

    #[test]
    fn stderr_log_promotes_only_failed_startup_lines() {
        let log = AgentStderrLog::new("test-agent");
        let clone = log.clone();

        for index in 0..STARTUP_STDERR_MAX_LINES + 1 {
            log.log_line(&format!("startup line {index}"));
        }
        {
            let inner = log
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            assert_eq!(inner.phase, StderrPhase::Startup);
            assert_eq!(inner.startup_lines.len(), STARTUP_STDERR_MAX_LINES);
            assert_eq!(inner.startup_lines.front().unwrap(), "startup line 1");
        }

        clone.mark_failed();
        log.log_line("late failure detail");
        let inner = log
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(inner.phase, StderrPhase::Failed);
        assert!(inner.startup_lines.is_empty());
    }

    #[test]
    fn stderr_log_discards_buffer_after_initialize() {
        let log = AgentStderrLog::new("test-agent");
        log.log_line("startup detail");
        log.mark_initialized();

        let inner = log
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(inner.phase, StderrPhase::Running);
        assert!(inner.startup_lines.is_empty());
    }

    #[test]
    fn stderr_line_at_limit_is_unchanged() {
        let line = "a".repeat(STARTUP_STDERR_MAX_CHARS_PER_LINE);

        assert_eq!(truncate_stderr_line(&line), line);
    }

    #[test]
    fn stderr_line_over_limit_includes_ellipsis_within_limit() {
        let line = "a".repeat(STARTUP_STDERR_MAX_CHARS_PER_LINE + 1);
        let truncated = truncate_stderr_line(&line);

        assert_eq!(truncated.chars().count(), STARTUP_STDERR_MAX_CHARS_PER_LINE);
        assert!(truncated.ends_with("..."));
        assert_eq!(
            truncated,
            format!("{}...", "a".repeat(STARTUP_STDERR_MAX_CHARS_PER_LINE - 3))
        );
    }

    #[test]
    fn stderr_line_truncation_preserves_unicode_characters() {
        let line = "界".repeat(STARTUP_STDERR_MAX_CHARS_PER_LINE + 1);
        let truncated = truncate_stderr_line(&line);

        assert_eq!(truncated.chars().count(), STARTUP_STDERR_MAX_CHARS_PER_LINE);
        assert_eq!(
            truncated,
            format!("{}...", "界".repeat(STARTUP_STDERR_MAX_CHARS_PER_LINE - 3))
        );
    }

    #[test]
    fn canonicalizes_simple_lang_region() {
        assert_eq!(canonicalize_posix_locale("zh-CN"), "zh_CN.UTF-8");
        assert_eq!(canonicalize_posix_locale("en-US"), "en_US.UTF-8");
        // Lowercase region in source folder names gets normalized.
        assert_eq!(canonicalize_posix_locale("gd-gb"), "gd_GB.UTF-8");
        // Already-uppercase region preserved (idempotent).
        assert_eq!(canonicalize_posix_locale("de-DE"), "de_DE.UTF-8");
    }

    #[test]
    fn canonicalizes_script_code() {
        // 4-letter script code (ISO 15924) → title case, region preserved.
        assert_eq!(canonicalize_posix_locale("sr-Cyrl-RS"), "sr_Cyrl_RS.UTF-8");
        assert_eq!(canonicalize_posix_locale("zh-Hans-CN"), "zh_Hans_CN.UTF-8");
        assert_eq!(canonicalize_posix_locale("az-Latn-AZ"), "az_Latn_AZ.UTF-8");
    }

    #[test]
    fn canonicalizes_language_only() {
        assert_eq!(canonicalize_posix_locale("en"), "en.UTF-8");
    }
}
