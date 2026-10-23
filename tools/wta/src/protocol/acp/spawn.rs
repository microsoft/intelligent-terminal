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

#[derive(Clone, Copy)]
pub(crate) enum SharedProviderSelection<'a> {
    /// Backward-compatible behavior for callers that do not declare whether
    /// they follow Terminal's current provider selection.
    Inherit,
    /// The helper authoritatively selected the agent's native provider.
    Disabled,
    /// Apply the master-resolved Terminal custom-provider configuration.
    Custom(&'a crate::custom_model_provider::Config),
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
    auth_browser: Option<(uuid::Uuid, tokio::sync::mpsc::Sender<String>)>,
}

/// Keeps routine agent stderr at debug level while preserving startup failures
/// in release logs. Only the bounded pre-initialize buffer is promoted.
#[derive(Clone)]
pub(crate) struct AgentStderrLog {
    agent: Arc<str>,
    inner: Arc<Mutex<AgentStderrLogInner>>,
}

pub(crate) struct AuthBrowserProgressGuard {
    log: AgentStderrLog,
    subscription_id: uuid::Uuid,
}

impl Drop for AuthBrowserProgressGuard {
    fn drop(&mut self) {
        let mut inner = self
            .log
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if inner
            .auth_browser
            .as_ref()
            .is_some_and(|(id, _)| *id == self.subscription_id)
        {
            inner.auth_browser = None;
        }
    }
}

impl AgentStderrLog {
    pub(crate) fn new(agent: impl Into<Arc<str>>) -> Self {
        Self {
            agent: agent.into(),
            inner: Arc::new(Mutex::new(AgentStderrLogInner {
                phase: StderrPhase::Startup,
                startup_lines: VecDeque::with_capacity(STARTUP_STDERR_MAX_LINES),
                auth_browser: None,
            })),
        }
    }

    pub(crate) fn subscribe_auth_browser(
        &self,
    ) -> (
        AuthBrowserProgressGuard,
        tokio::sync::mpsc::Receiver<String>,
    ) {
        let subscription_id = uuid::Uuid::new_v4();
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .auth_browser = Some((subscription_id, tx));
        (
            AuthBrowserProgressGuard {
                log: self.clone(),
                subscription_id,
            },
            rx,
        )
    }

    pub(crate) fn drain(&self, stderr: tokio::process::ChildStderr) -> tokio::task::JoinHandle<()> {
        let log = self.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr);
            loop {
                let mut line = Vec::new();
                match reader.read_until(b'\n', &mut line).await {
                    Ok(0) => break,
                    Ok(_) => log.log_line(
                        String::from_utf8_lossy(&line).trim_end_matches(['\r', '\n']),
                    ),
                    Err(error) => {
                        log.log_line(&format!("failed to read agent stderr: {error}"));
                        break;
                    }
                }
            }
        })
    }

    /// Kill the child, drain its stderr, and return the captured startup lines
    /// so the caller can surface them.
    ///
    /// The lines are the only description of *why* the agent died. Without
    /// them the user sees the transport symptom — "response to `initialize`
    /// never received: oneshot canceled" — while the actual cause (a missing
    /// CLI, a broken sandbox, an auth prompt) reaches the log only.
    pub(crate) async fn finish_failed_startup(
        &self,
        child: &mut super::process::AgentProcess,
        stderr_task: Option<tokio::task::JoinHandle<()>>,
    ) -> Result<Vec<String>> {
        let cleanup = child.terminate().await;
        let lines = self.finish_failed_stderr(stderr_task).await;
        cleanup
            .context("failed to reclaim the ACP process tree")
            .map_err(|error| crate::agent_lifecycle::StartupDetails::attach(error, &lines))?;
        Ok(lines)
    }

    pub(crate) async fn finish_failed_stderr(
        &self,
        stderr_task: Option<tokio::task::JoinHandle<()>>,
    ) -> Vec<String> {
        if let Some(mut stderr_task) = stderr_task {
            if tokio::time::timeout(STARTUP_STDERR_DRAIN_TIMEOUT, &mut stderr_task)
                .await
                .is_err()
            {
                stderr_task.abort();
                let _ = stderr_task.await;
            }
        }
        self.mark_failed()
    }

    pub(crate) fn mark_initialized(&self) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        inner.phase = StderrPhase::Running;
        inner.startup_lines.clear();
    }

    /// Promote the captured startup stderr to the log and return it.
    ///
    /// Returns empty when the phase already advanced (a second call, or the
    /// agent had already initialized), so a caller can't double-report.
    pub(crate) fn mark_failed(&self) -> Vec<String> {
        let startup_lines = {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if inner.phase != StderrPhase::Startup {
                return Vec::new();
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
        for line in &startup_lines {
            tracing::warn!(
                target: "agent_stderr",
                agent = %self.agent,
                phase = "startup_failure",
                "{line}"
            );
        }
        startup_lines
    }

    pub(crate) fn log_line(&self, line: &str) {
        // The WSL host-lifetime guardian uses blank stderr heartbeats.
        if line.trim().is_empty() {
            return;
        }
        let redacted = super::authentication::redact_auth_stderr(line);
        {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some((_, sender)) = &inner.auth_browser {
                if sender.is_closed() {
                    inner.auth_browser = None;
                } else if let Some(url) = super::authentication::browser_url_from_stderr(line) {
                    // A single bounded ephemeral URL; never back-pressure stderr.
                    let _ = sender.try_send(url);
                }
            }
            match inner.phase {
                StderrPhase::Startup => {
                    if inner.startup_lines.len() == STARTUP_STDERR_MAX_LINES {
                        inner.startup_lines.pop_front();
                    }
                    inner
                        .startup_lines
                        .push_back(truncate_stderr_line(redacted));
                }
                StderrPhase::Running | StderrPhase::Failed => {}
            }
        }

        tracing::debug!(target: "agent_stderr", agent = %self.agent, "{redacted}");
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
    pub child: super::process::AgentProcess,
    /// Original first token of `agent_cmd`, before path resolution.
    pub raw_program: String,
    /// Resolved program path (post `resolve_bare_agent_name`).
    pub resolved_program: String,
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
pub(crate) fn spawn_agent_process_with_provider(
    agent_cmd: &str,
    cwd: Option<&Path>,
    agent_id: Option<&str>,
    environment_policy: ChildEnvironmentPolicy,
    provider_selection: SharedProviderSelection<'_>,
) -> Result<AgentSpawn> {
    spawn_host_agent_process(
        agent_cmd,
        cwd,
        agent_id,
        environment_policy,
        provider_selection,
    )
}

fn split_agent_commandline(commandline: &str) -> Result<Vec<String>> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::UI::Shell::CommandLineToArgvW;

    anyhow::ensure!(!commandline.contains('\0'), "agent command contains NUL");
    // A dummy argv[0] makes every configured token use argument escaping,
    // matching join_windows_commandline, rather than argv[0]'s special rules.
    let commandline: Vec<u16> = format!("wta {commandline}")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut count = 0;
    // SAFETY: commandline is NUL-terminated and count is a valid output pointer.
    let argv = unsafe { CommandLineToArgvW(commandline.as_ptr(), &mut count) };
    anyhow::ensure!(
        !argv.is_null(),
        "failed to parse agent command: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: Windows returns count NUL-terminated strings in one allocation.
    // Copy them before releasing that allocation with LocalFree.
    let args = unsafe {
        let args = std::slice::from_raw_parts(argv, count as usize)
            .iter()
            .skip(1)
            .map(|&arg| {
                let mut len = 0;
                while *arg.add(len) != 0 {
                    len += 1;
                }
                String::from_utf16_lossy(std::slice::from_raw_parts(arg, len))
            })
            .collect();
        LocalFree(argv.cast());
        args
    };
    Ok(args)
}

fn spawn_host_agent_process(
    agent_cmd: &str,
    cwd: Option<&Path>,
    agent_id: Option<&str>,
    environment_policy: ChildEnvironmentPolicy,
    provider_selection: SharedProviderSelection<'_>,
) -> Result<AgentSpawn> {
    let parts = split_agent_commandline(agent_cmd)?;
    let raw_program = parts
        .first()
        .map(String::as_str)
        .ok_or_else(|| anyhow!("empty agent command"))?;
    let args = &parts[1..];
    let resolved_program = if resolve_spawn_profile(agent_cmd, agent_id).id
        == crate::agent_registry::ANTIGRAVITY_AGENT_ID
        && raw_program.eq_ignore_ascii_case("agy_acp_server.exe")
    {
        crate::agent_check::find_acp_exe(crate::agent_registry::ANTIGRAVITY_AGENT_ID)
            .context("Antigravity ACP is not ready after dependency preparation")?
    } else {
        crate::agent_registry::resolve_bare_agent_name(raw_program)
    };
    let needs_cmd = crate::coordinator::needs_shell_launch(
        &crate::coordinator::quote_windows_commandline_arg(&resolved_program),
    );

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

    let mut launch_args = Vec::new();
    if is_npx && should_omit_optional_dependencies(agent_cmd, agent_id) {
        launch_args.push("--omit=optional".to_string());
    }
    launch_args.extend_from_slice(args);
    let mut cmd = if needs_cmd {
        batch_companion_command(&resolved_program, &launch_args, cwd)?
    } else {
        let mut cmd = tokio::process::Command::new(&resolved_program);
        cmd.args(&launch_args);
        cmd
    };

    // The claude-code-acp adapter refuses to start when its recursion-
    // guard env var is set — that guard exists to block recursive
    // `claude` shells from sharing runtime, but doesn't apply to an
    // ACP host. Scrub unconditionally; other agents don't care.
    cmd.env_remove("CLAUDECODE");
    configure_child_environment(
        &mut cmd,
        agent_cmd,
        agent_id,
        environment_policy,
        provider_selection,
    )?;

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

    // Keep the log path available to pre-0.1.5 hook bundles while startup
    // auto-upgrade replaces their PowerShell bridge with `wtcli agent-hook`.
    // The only reader is the deleted `send-event.ps1`, which can still be
    // cached on disk for one agent lifetime after an IT upgrade.
    //
    // RETIREMENT (#620): delete once no supported upgrade path can leave a
    // pre-0.1.5 `wt-agent-hooks` bundle installed.
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
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let child = super::process::AgentProcess::spawn(&mut cmd, None)
        .with_context(|| format!("failed to spawn agent '{agent_cmd}'"))?;

    Ok(AgentSpawn {
        child,
        raw_program: raw_program.to_string(),
        resolved_program,
        adapter_package,
    })
}

fn batch_companion_command(
    program: &str,
    args: &[String],
    cwd: Option<&Path>,
) -> Result<tokio::process::Command> {
    // PS 5.1's native binder cannot round-trip these shapes through npm's
    // .ps1 -> node.exe hop. Reject them before starting any process.
    anyhow::ensure!(
        args.iter().all(|arg| !arg.is_empty()
            && !arg.contains(['"', '\0'])
            && !(arg.ends_with('\\') && arg.chars().any(char::is_whitespace))),
        "ACP batch launch is unsupported for arguments that Windows PowerShell cannot preserve"
    );
    let path = Path::new(program);
    anyhow::ensure!(
        path.extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("cmd") || ext.eq_ignore_ascii_case("bat")),
        "ACP batch launch is unsupported: expected a batch shim"
    );
    let base = std::env::current_dir()?.join(cwd.unwrap_or_else(|| Path::new("")));
    let shim = if path.is_absolute() {
        path.to_path_buf()
    } else if program.contains(['\\', '/']) || base.join(path).is_file() {
        base.join(path)
    } else {
        crate::agent_check::spawn_path()
            .map(std::ffi::OsString::from)
            .or_else(|| std::env::var_os("PATH"))
            .and_then(|paths| {
                std::env::split_paths(&paths)
                    .map(|directory| base.join(directory).join(path))
                    .find(|candidate| candidate.is_file())
            })
            .context("ACP batch launch is unsupported: shim not found")?
    };
    let companion = shim.with_extension("ps1");
    anyhow::ensure!(
        shim.is_file() && companion.is_file(),
        "ACP batch launch is unsupported without an adjacent PowerShell companion"
    );
    let quote = |value: &str| format!("'{}'", value.replace('\'', "''"));
    let call = std::iter::once(quote(&companion.to_string_lossy()))
        .chain(args.iter().map(|arg| quote(arg)))
        .collect::<Vec<_>>()
        .join(" ");
    let script = format!("$ErrorActionPreference='Stop'; & {call}; exit $LASTEXITCODE");
    let encoded: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let powershell = PathBuf::from(
        std::env::var_os("SystemRoot").context("SystemRoot is required for ACP batch launch")?,
    )
    .join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    let mut command = tokio::process::Command::new(powershell);
    command.args([
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        // Process-only policy; MachinePolicy and UserPolicy still take precedence.
        "-ExecutionPolicy",
        "Bypass",
        "-EncodedCommand",
        &crate::osc52::base64_encode(&encoded),
    ]);
    Ok(command)
}

pub(crate) fn wta_cli_directory() -> Result<PathBuf> {
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
    provider_selection: SharedProviderSelection<'_>,
) -> Result<Option<crate::agent_registry::ByokMode>> {
    let profile = resolve_spawn_profile(agent_cmd, agent_id);
    let configured_provider = match environment_policy {
        ChildEnvironmentPolicy::ApplySharedProvider => match provider_selection {
            SharedProviderSelection::Inherit => {
                crate::custom_model_provider::configure_child(command, profile.byok_mode)
            }
            SharedProviderSelection::Disabled => {
                crate::custom_model_provider::scrub_child_for_cloud_discovery(command);
                Ok(None)
            }
            SharedProviderSelection::Custom(config) => {
                crate::custom_model_provider::configure_child_with_config(
                    command,
                    profile.byok_mode,
                    config,
                )
            }
        },
        ChildEnvironmentPolicy::CleanCloudDiscovery => {
            crate::custom_model_provider::scrub_child_for_cloud_discovery(command);
            Ok(None)
        }
    }?;
    configure_adapter_executable_environment(command, profile, crate::agent_check::find_exe)?;
    Ok(configured_provider)
}

fn configure_adapter_executable_environment(
    command: &mut tokio::process::Command,
    profile: &crate::agent_registry::AgentProfile,
    find_executable: impl FnOnce(&str) -> Option<String>,
) -> Result<()> {
    let (variable, missing_message) = match profile.id {
        crate::agent_registry::CLAUDE_AGENT_ID => (
            "CLAUDE_CODE_EXECUTABLE",
            "Claude native binary not found. Reinstall Claude Code or set \
             CLAUDE_CODE_EXECUTABLE to the full path of claude.exe",
        ),
        crate::agent_registry::CODEX_AGENT_ID => (
            "CODEX_PATH",
            "Codex executable not found. Reinstall Codex or set CODEX_PATH to its full path",
        ),
        _ => return Ok(()),
    };
    let executable = find_executable(profile.id).ok_or_else(|| anyhow!(missing_message))?;
    command.env(variable, executable);
    Ok(())
}

fn should_omit_optional_dependencies(agent_cmd: &str, agent_id: Option<&str>) -> bool {
    matches!(
        resolve_spawn_profile(agent_cmd, agent_id).id,
        crate::agent_registry::CLAUDE_AGENT_ID | crate::agent_registry::CODEX_AGENT_ID
    )
}

/// Spawn an ACP agent in the selected per-tab execution source.
pub(crate) fn spawn_agent_process_for_source_with_provider(
    agent_cmd: &str,
    cwd: Option<&Path>,
    agent_id: Option<&str>,
    source: &crate::agent_source::AgentSource,
    environment_policy: ChildEnvironmentPolicy,
    provider_selection: SharedProviderSelection<'_>,
) -> Result<AgentSpawn> {
    match source {
        crate::agent_source::AgentSource::Host => spawn_agent_process_with_provider(
            agent_cmd,
            cwd,
            agent_id,
            environment_policy,
            provider_selection,
        ),
        crate::agent_source::AgentSource::Wsl { distro } => {
            spawn_wsl_agent_process(agent_cmd, distro, agent_id, environment_policy)
        }
    }
}

pub(crate) fn spawn_history_agent_process(
    agent_cmd: &str,
    agent_id: &str,
    source: &crate::agent_source::AgentSource,
    provider_selection: SharedProviderSelection<'_>,
) -> Result<AgentSpawn> {
    spawn_agent_process_for_source_with_provider(
        agent_cmd,
        None,
        Some(agent_id),
        source,
        ChildEnvironmentPolicy::ApplySharedProvider,
        provider_selection,
    )
}

fn spawn_wsl_agent_process(
    agent_cmd: &str,
    distro: &str,
    agent_id: Option<&str>,
    environment_policy: ChildEnvironmentPolicy,
) -> Result<AgentSpawn> {
    let parts = split_agent_commandline(agent_cmd)?;
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
    let mut script = wsl_agent_launch_script(&parts, environment_policy);
    if resolve_spawn_profile(agent_cmd, agent_id).id == crate::agent_registry::ANTIGRAVITY_AGENT_ID
        && raw_program == "agy_acp_server.par"
    {
        script = antigravity_wsl_launch_script(&parts, environment_policy);
    }
    let marker = format!("wta-acp-{}", uuid::Uuid::new_v4());
    script = super::process::wsl_process_script(&script, &marker);

    let mut command = wsl_agent_command(distro, &script);
    command
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

    let child = super::process::AgentProcess::spawn(
        &mut command,
        Some((distro.to_string(), marker)),
    )
    .with_context(|| format!("failed to spawn agent '{agent_cmd}' in WSL distro '{distro}'"))?;

    Ok(AgentSpawn {
        child,
        raw_program,
        resolved_program: format!("wsl.exe -d {distro}"),
        adapter_package,
    })
}

fn wsl_agent_command(distro: &str, script: &str) -> tokio::process::Command {
    let mut command = tokio::process::Command::new("wsl.exe");
    command
        .arg("-d")
        .arg(distro)
        .arg("--exec")
        .arg("bash")
        .arg("--noprofile")
        .arg("--norc")
        .arg("-c")
        .arg(script);
    command
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
    wsl_login_script(&format!("exec {invocation}"), environment_policy)
}

fn antigravity_wsl_launch_script(
    parts: &[String],
    environment_policy: ChildEnvironmentPolicy,
) -> String {
    let args = parts[1..]
        .iter()
        .map(|part| crate::coordinator::sh_quote(part))
        .collect::<Vec<_>>()
        .join(" ");
    let invocation = format!(
        "{}; if [ -z \"$resolved\" ]; then \
         printf '%s\\n' 'Antigravity ACP installation is incomplete.' >&2; exit 127; fi; \
         exec \"$resolved\" {args}",
        crate::antigravity_acp::wsl_server_resolution_script()
    );
    wsl_login_script(&invocation, environment_policy)
}

fn wsl_login_script(invocation: &str, environment_policy: ChildEnvironmentPolicy) -> String {
    let mut unset_names = vec!["CLAUDECODE"];
    if environment_policy == ChildEnvironmentPolicy::CleanCloudDiscovery {
        unset_names
            .extend_from_slice(crate::custom_model_provider::cloud_discovery_environment_keys());
    }
    let inner = format!(
        "exec 1>&3 3>&-; unset {}; {invocation}",
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
    fn agent_commandline_round_trips_windows_arguments() {
        let args = [
            r"C:\Program Files\Custom Agent\agent.exe",
            "--config",
            "",
            "--stdio",
            r"C:\directory with spaces\",
            r#"embedded "quotes" and \"escaped quotes\""#,
            "tab\tseparated",
            r"\\server\share\",
        ];
        assert_eq!(
            split_agent_commandline(&crate::coordinator::join_windows_commandline(&args)).unwrap(),
            args
        );
        assert!(split_agent_commandline(" \t ").unwrap().is_empty());
        assert!(split_agent_commandline("agent\0ignored").is_err());
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn host_batch_and_native_spawns_preserve_native_argv() {
        use tokio::io::AsyncReadExt;

        // Restrict only an isolated test process, not parallel tests in this process.
        const ISOLATED_TEST: &str = "WTA_TEST_RESTRICTED_BATCH_ARGV";
        let test_name = concat!(
            module_path!(),
            "::host_batch_and_native_spawns_preserve_native_argv"
        )
        .split_once("::")
        .unwrap()
        .1;
        if std::env::var(ISOLATED_TEST).as_deref() != Ok(test_name) {
            let output = tokio::time::timeout(
                Duration::from_secs(60),
                tokio::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", test_name, "--nocapture", "--test-threads=1"])
                    .env(ISOLATED_TEST, test_name)
                    .env("PSExecutionPolicyPreference", "Restricted")
                    // Restricted must probe Windows PowerShell's own policy module, not PS7's.
                    .env_remove("PSModulePath")
                    .kill_on_drop(true)
                    .output(),
            )
            .await
            .unwrap()
            .unwrap();
            assert!(
                output.status.success(),
                "isolated Restricted test failed: {output:?}"
            );
            assert!(
                String::from_utf8_lossy(&output.stdout).contains("1 passed;"),
                "the isolated subprocess must run the selected test: {output:?}"
            );
            return;
        }

        let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("batch argv {}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let source = directory.join("argv.cs");
        let executable = directory.join("native argv.exe");
        std::fs::write(
            &source,
            r#"using System; using System.Text;
class Probe { static void Main(string[] args) {
foreach (string arg in args) Console.WriteLine(Convert.ToBase64String(Encoding.UTF8.GetBytes(arg)));
} }"#,
        )
        .unwrap();
        let compiler = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
            .join(r"Microsoft.NET\Framework64\v4.0.30319\csc.exe");
        let output = std::process::Command::new(compiler)
            .arg("/nologo")
            .arg(format!("/out:{}", executable.display()))
            .arg(&source)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let shim = directory.join("npm agent.cmd");
        let marker = directory.join("batch must not run");
        std::fs::write(&shim, format!("@echo bad > \"{}\"\r\n", marker.display())).unwrap();
        std::fs::write(
            shim.with_extension("ps1"),
            "& \"$PSScriptRoot\\native argv.exe\" @args\r\nexit $LASTEXITCODE\r\n",
        )
        .unwrap();
        let launcher =
            batch_companion_command(shim.to_str().unwrap(), &[], Some(&directory)).unwrap();
        let launcher = launcher.as_std();
        let policy = std::process::Command::new(launcher.get_program())
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Get-ExecutionPolicy; Get-ExecutionPolicy -Scope Process",
            ])
            .output()
            .unwrap();
        assert!(policy.status.success(), "{policy:?}");
        assert_eq!(
            String::from_utf8(policy.stdout)
                .unwrap()
                .lines()
                .map(str::trim)
                .collect::<Vec<_>>(),
            ["Restricted", "Restricted"],
            "the isolated child must actually enforce Restricted"
        );
        // Reproduce the old launcher, without its new process-scoped override.
        let old_launcher = std::process::Command::new(launcher.get_program())
            .args(launcher.get_args().filter(|arg| {
                *arg != std::ffi::OsStr::new("-ExecutionPolicy")
                    && *arg != std::ffi::OsStr::new("Bypass")
            }))
            .output()
            .unwrap();
        assert!(!old_launcher.status.success(), "{old_launcher:?}");
        assert!(
            String::from_utf8_lossy(&old_launcher.stderr).contains("UnauthorizedAccess"),
            "old launcher must fail because script execution is restricted: {old_launcher:?}"
        );
        assert!(old_launcher.stdout.is_empty());
        assert!(!marker.exists(), "batch shell must never run");
        let safe = [
            "argument with spaces",
            "a&b",
            "%SystemRoot%",
            "it's literal",
        ];
        let native_only = ["", r#"embedded "quotes""#, r"C:\space here\"];
        for program in [&shim, &executable] {
            for history_only in [false, true] {
                let mut values = safe.to_vec();
                if program == &executable {
                    values.extend(native_only);
                }
                let mut args = vec![program.to_str().unwrap()];
                args.extend(values.iter().copied());
                let command = crate::coordinator::join_windows_commandline(&args);
                let mut spawned = if history_only {
                    spawn_history_agent_process(
                        &command,
                        "custom:batch-test",
                        &crate::agent_source::AgentSource::Host,
                        SharedProviderSelection::Disabled,
                    )
                } else {
                    spawn_agent_process_with_provider(
                        &command,
                        Some(&directory),
                        Some("custom:batch-test"),
                        ChildEnvironmentPolicy::CleanCloudDiscovery,
                        SharedProviderSelection::Disabled,
                    )
                }
                .unwrap();
                let mut stdout = String::new();
                tokio::time::timeout(
                    Duration::from_secs(10),
                    spawned
                        .child
                        .take_stdout()
                        .await
                        .unwrap()
                        .read_to_string(&mut stdout),
                )
                .await
                .unwrap()
                .unwrap();
                assert!(spawned.child.wait().await.unwrap().success());
                let actual: Vec<&str> = stdout.lines().collect();
                let expected: Vec<String> = values
                    .iter()
                    .map(|arg| crate::osc52::base64_encode(arg.as_bytes()))
                    .collect();
                assert_eq!(actual, expected, "history_only={history_only}, {program:?}");
                assert!(!marker.exists(), "batch shell must never run");
            }
        }
        for arg in native_only {
            for history_only in [false, true] {
                let command =
                    crate::coordinator::join_windows_commandline(&[shim.to_str().unwrap(), arg]);
                let result = if history_only {
                    spawn_history_agent_process(
                        &command,
                        "custom:batch-test",
                        &crate::agent_source::AgentSource::Host,
                        SharedProviderSelection::Disabled,
                    )
                } else {
                    spawn_agent_process_with_provider(
                        &command,
                        Some(&directory),
                        Some("custom:batch-test"),
                        ChildEnvironmentPolicy::CleanCloudDiscovery,
                        SharedProviderSelection::Disabled,
                    )
                };
                assert!(result.err().unwrap().to_string().contains("unsupported"));
                assert!(!marker.exists());
            }
        }
        std::fs::remove_file(shim.with_extension("ps1")).unwrap();
        let command = crate::coordinator::join_windows_commandline(&[shim.to_str().unwrap()]);
        for history_only in [false, true] {
            let result = if history_only {
                spawn_history_agent_process(
                    &command,
                    "custom:batch-test",
                    &crate::agent_source::AgentSource::Host,
                    SharedProviderSelection::Disabled,
                )
            } else {
                spawn_agent_process_with_provider(
                    &command,
                    Some(&directory),
                    Some("custom:batch-test"),
                    ChildEnvironmentPolicy::CleanCloudDiscovery,
                    SharedProviderSelection::Disabled,
                )
            };
            assert!(result.err().unwrap().to_string().contains("companion"));
        }
        assert!(!marker.exists());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn host_spawns_round_trip_windows_arguments() {
        use tokio::io::AsyncReadExt;

        let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("Generated Files")
            .join(format!("agent argv {}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let script = directory.join("echo arguments.ps1");
        std::fs::write(
            &script,
            "[Console]::WriteLine((ConvertTo-Json -Compress -InputObject @($args)))",
        )
        .unwrap();
        let expected = [
            "--config",
            "",
            "--stdio",
            r"C:\directory with spaces\",
            r#"embedded "quotes" and \"escaped quotes\""#,
            "argument with spaces",
        ];
        let mut args = vec![
            "pwsh.exe",
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-File",
            script.to_str().unwrap(),
        ];
        args.extend(expected);
        let command = crate::coordinator::join_windows_commandline(&args);
        for history_only in [false, true] {
            let mut spawned = if history_only {
                spawn_history_agent_process(
                    &command,
                    "custom:argv-test",
                    &crate::agent_source::AgentSource::Host,
                    SharedProviderSelection::Disabled,
                )
            } else {
                spawn_agent_process_with_provider(
                    &command,
                    Some(&directory),
                    Some("custom:argv-test"),
                    ChildEnvironmentPolicy::CleanCloudDiscovery,
                    SharedProviderSelection::Disabled,
                )
            }
            .unwrap();
            let mut stdout = String::new();
            tokio::time::timeout(
                Duration::from_secs(10),
                spawned
                    .child
                    .take_stdout()
                    .await
                    .unwrap()
                    .read_to_string(&mut stdout),
            )
            .await
            .unwrap()
            .unwrap();
            assert!(spawned.child.wait().await.unwrap().success());
            let actual: Vec<String> = serde_json::from_str(stdout.trim()).unwrap();
            assert_eq!(actual, expected, "history_only={history_only}");
        }
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn host_spawns_preserve_quoted_executable_and_argument_paths() {
        use tokio::io::AsyncReadExt;

        let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("Generated Files")
            .join(format!("agent with spaces {}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let executable = directory.join("custom agent.exe");
        let marker = directory.join("argument with spaces.txt");
        let system_root = std::env::var_os("SystemRoot").unwrap();
        std::fs::copy(
            PathBuf::from(system_root)
                .join("System32")
                .join("where.exe"),
            &executable,
        )
        .unwrap();
        std::fs::write(&marker, "").unwrap();
        let pattern = format!("{}:argument with spaces.txt", directory.display());
        let command = crate::coordinator::join_windows_commandline(&[
            executable.to_str().unwrap(),
            "/F",
            &pattern,
        ]);

        for history_only in [false, true] {
            let mut spawned = if history_only {
                spawn_history_agent_process(
                    &command,
                    "custom:spawn-test",
                    &crate::agent_source::AgentSource::Host,
                    SharedProviderSelection::Disabled,
                )
            } else {
                spawn_agent_process_with_provider(
                    &command,
                    Some(&directory),
                    Some("custom:spawn-test"),
                    ChildEnvironmentPolicy::CleanCloudDiscovery,
                    SharedProviderSelection::Disabled,
                )
            }
            .unwrap();
            assert_eq!(spawned.raw_program, executable.to_str().unwrap());
            assert_eq!(spawned.resolved_program, executable.to_str().unwrap());
            let mut stdout = String::new();
            tokio::time::timeout(
                Duration::from_secs(10),
                spawned
                    .child
                    .take_stdout()
                    .await
                    .unwrap()
                    .read_to_string(&mut stdout),
            )
            .await
            .unwrap()
            .unwrap();
            let status = spawned.child.wait().await.unwrap();
            assert!(
                status.success(),
                "history_only={history_only}, status={status}, stdout={stdout}",
            );
            assert_eq!(stdout.trim().trim_matches('"'), marker.to_str().unwrap());
        }
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn failed_acp_startup_reclaims_the_owned_descendant_before_returning() {
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        use windows_sys::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob,
            JobObjectExtendedLimitInformation, SetInformationJobObject, TerminateJobObject,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };
        use windows_sys::Win32::System::Threading::{
            OpenProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION,
            PROCESS_SYNCHRONIZE,
        };

        let command = r#"pwsh.exe -NoLogo -NoProfile -NonInteractive -Command $null=[Console]::ReadLine();$child=Start-Process pwsh.exe -ArgumentList '-NoProfile','-NonInteractive','-Command','Start-Sleep -Seconds 120' -WindowStyle Hidden -PassThru;[Console]::Error.WriteLine($child.Id);Start-Sleep -Seconds 120"#;
        let mut spawned = spawn_agent_process_with_provider(
            command,
            None,
            Some("custom:startup-cleanup-regression"),
            ChildEnvironmentPolicy::CleanCloudDiscovery,
            SharedProviderSelection::Inherit,
        )
        .expect("spawn the real ACP launcher fixture");

        // The fixture waits for stdin before spawning, so this independent
        // test job contains every descendant even when the RED assertion fails.
        let safety_job = unsafe {
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            assert!(!handle.is_null());
            let job = OwnedHandle::from_raw_handle(handle);
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            assert_ne!(
                SetInformationJobObject(
                    job.as_raw_handle(),
                    JobObjectExtendedLimitInformation,
                    std::ptr::addr_of!(limits).cast(),
                    std::mem::size_of_val(&limits) as u32,
                ),
                0
            );
            assert_ne!(
                AssignProcessToJobObject(job.as_raw_handle(), spawned.child.raw_handle().unwrap()),
                0
            );
            job
        };
        spawned
            .child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"start\n")
            .await
            .unwrap();
        let mut stderr = tokio::io::BufReader::new(spawned.child.stderr.take().unwrap());
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(30), stderr.read_line(&mut line))
            .await
            .expect("fixture must report its descendant")
            .unwrap();
        let descendant_pid = line
            .trim()
            .parse::<u32>()
            .expect("fixture reports a native descendant PID");
        // A live handle and job-membership proof prevent PID reuse or another
        // process from being mistaken for the fixture's descendant.
        let descendant = unsafe {
            let handle = OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0,
                descendant_pid,
            );
            assert!(!handle.is_null());
            let process = OwnedHandle::from_raw_handle(handle);
            let mut in_job = 0;
            assert_ne!(
                IsProcessInJob(process.as_raw_handle(), safety_job.as_raw_handle(), &mut in_job),
                0
            );
            assert_ne!(in_job, 0);
            assert_eq!(WaitForSingleObject(process.as_raw_handle(), 0), WAIT_TIMEOUT);
            process
        };

        AgentStderrLog::new("startup-cleanup-regression")
            .finish_failed_startup(&mut spawned.child, None)
            .await
            .expect("failed startup must confirm descendant cleanup");
        let gone_when_cleanup_returned =
            unsafe { WaitForSingleObject(descendant.as_raw_handle(), 0) == WAIT_OBJECT_0 };

        unsafe {
            assert_ne!(TerminateJobObject(safety_job.as_raw_handle(), 1), 0);
            assert_eq!(
                WaitForSingleObject(descendant.as_raw_handle(), 5_000),
                WAIT_OBJECT_0,
                "the test safety job must reclaim its fixture even on RED"
            );
        }
        tokio::time::timeout(Duration::from_secs(5), spawned.child.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(
            gone_when_cleanup_returned,
            "failed ACP startup returned while its owned descendant was still running"
        );
    }

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
    fn wsl_acp_command_executes_bash_directly_without_default_shell_expansion() {
        let script = antigravity_wsl_launch_script(
            &["agy_acp_server.par".into(), "--uid=".into()],
            ChildEnvironmentPolicy::ApplySharedProvider,
        );
        for history_only in [false, true] {
            let script = if history_only {
                super::super::process::wsl_process_script(&script, "wta-history-test")
            } else {
                script.clone()
            };
            let command = wsl_agent_command("Debian", &script);
            assert_eq!(command.as_std().get_program(), "wsl.exe");
            assert_eq!(
                command
                    .as_std()
                    .get_args()
                    .map(|arg| arg.to_str().unwrap())
                    .collect::<Vec<_>>(),
                [
                    "-d",
                    "Debian",
                    "--exec",
                    "bash",
                    "--noprofile",
                    "--norc",
                    "-c",
                    &script,
                ],
                "an extra default shell must not expand ACP cache variables or quotes"
            );
        }
    }

    #[test]
    fn antigravity_wsl_managed_launch_preserves_uid_and_cloud_environment_policy() {
        let parts = vec!["agy_acp_server.par".into(), "--uid=".into()];
        for policy in [
            ChildEnvironmentPolicy::ApplySharedProvider,
            ChildEnvironmentPolicy::CleanCloudDiscovery,
        ] {
            let script = antigravity_wsl_launch_script(&parts, policy);
            assert!(script.contains("--uid="));
            assert!(script.contains("localharness_external"));
            assert!(script.contains("readlink -f"));
            assert!(script.contains(".intelligent-terminal-acp.json"));
            assert!(script.contains("unset CLAUDECODE"));
            for key in crate::custom_model_provider::cloud_discovery_environment_keys() {
                assert_eq!(
                    script.contains(key),
                    policy == ChildEnvironmentPolicy::CleanCloudDiscovery,
                    "wrong WSL provider environment policy for {key}"
                );
            }
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
            SharedProviderSelection::Inherit,
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
    fn native_provider_selection_scrubs_inherited_byok_environment() {
        let mut command = tokio::process::Command::new("copilot");
        for key in crate::custom_model_provider::cloud_discovery_environment_keys() {
            command.env(key, "stale-provider-value");
        }

        let applied = configure_child_environment(
            &mut command,
            "copilot --acp --stdio",
            Some("copilot"),
            ChildEnvironmentPolicy::ApplySharedProvider,
            SharedProviderSelection::Disabled,
        )
        .expect("native provider selection should not require BYOK configuration");

        assert_eq!(applied, None);
        let configured_env: std::collections::HashMap<_, _> = command.as_std().get_envs().collect();
        for key in crate::custom_model_provider::cloud_discovery_environment_keys() {
            assert_eq!(
                configured_env.get(std::ffi::OsStr::new(key)),
                Some(&None),
                "native provider selection must scrub stale {key}"
            );
        }
    }

    #[test]
    fn adapter_spawns_use_the_same_executables_as_preflight() {
        for (agent_id, variable, executable) in [
            ("claude", "CLAUDE_CODE_EXECUTABLE", r"C:\Claude\claude.exe"),
            ("codex", "CODEX_PATH", r"C:\Codex\codex.exe"),
        ] {
            let mut command = tokio::process::Command::new("npx");
            let profile = crate::agent_registry::lookup_profile_by_id(agent_id);
            configure_adapter_executable_environment(&mut command, profile, |requested_id| {
                assert_eq!(requested_id, agent_id);
                Some(executable.to_string())
            })
            .unwrap();
            let configured_env: std::collections::HashMap<_, _> =
                command.as_std().get_envs().collect();
            assert_eq!(
                configured_env.get(std::ffi::OsStr::new(variable)),
                Some(&Some(std::ffi::OsStr::new(executable)))
            );
        }
    }

    #[test]
    fn adapter_spawns_fail_before_launch_when_preflight_executable_is_missing() {
        for agent_id in ["claude", "codex"] {
            let mut command = tokio::process::Command::new("npx");
            let profile = crate::agent_registry::lookup_profile_by_id(agent_id);
            let error = configure_adapter_executable_environment(&mut command, profile, |_| None)
                .unwrap_err();
            assert!(error.to_string().contains("not found"));
        }
    }

    #[test]
    fn only_builtin_host_adapters_omit_optional_npm_dependencies() {
        assert!(should_omit_optional_dependencies(
            "npx -y @agentclientprotocol/claude-agent-acp@0.65.0",
            Some("claude")
        ));
        assert!(should_omit_optional_dependencies(
            "npx -y @agentclientprotocol/codex-acp@1.1.13",
            Some("codex")
        ));
        assert!(!should_omit_optional_dependencies(
            "npx -y @example/custom-agent",
            Some("custom:test")
        ));
        assert!(!should_omit_optional_dependencies(
            "copilot --acp --stdio",
            Some("copilot")
        ));
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
    fn auth_bridge_stderr_progress_is_transient_and_guard_owned() {
        const URL: &str = "https://accounts.google.com/o/oauth2/v2/auth?redirect_uri=http%3A%2F%2Flocalhost%3A8181%2Fcallback&code=fixture";
        for phase in [
            StderrPhase::Startup,
            StderrPhase::Running,
            StderrPhase::Failed,
        ] {
            let log = AgentStderrLog::new("test-agent");
            log.inner.lock().unwrap().phase = phase;
            let (guard, mut receiver) = log.subscribe_auth_browser();
            log.log_line(&format!("Open {URL}"));
            assert_eq!(receiver.try_recv().unwrap(), URL);
            assert!(log
                .inner
                .lock()
                .unwrap()
                .startup_lines
                .iter()
                .all(|line| !line.contains(URL)));
            drop(guard);
            assert!(log.inner.lock().unwrap().auth_browser.is_none());
            log.log_line(&format!("Open {URL}"));
            assert!(receiver.try_recv().is_err());
        }
    }

    #[test]
    fn auth_bridge_old_guard_cannot_clear_new_subscriber_and_closed_receiver_retires() {
        let log = AgentStderrLog::new("test-agent");
        let (old_guard, _) = log.subscribe_auth_browser();
        let (new_guard, receiver) = log.subscribe_auth_browser();
        drop(old_guard);
        assert!(log.inner.lock().unwrap().auth_browser.is_some());
        drop(receiver);
        log.log_line("ordinary stderr");
        assert!(log.inner.lock().unwrap().auth_browser.is_none());
        drop(new_guard);
    }

    #[test]
    fn auth_bridge_stderr_never_retains_oauth_urls() {
        let log = AgentStderrLog::new("test-agent");
        log.log_line("Open https://accounts.google.com/o/oauth2/v2/auth?state=private-marker&redirect_uri=http%3A%2F%2Flocalhost%3A8181%2Fcallback");
        let captured = log.mark_failed().join("\n");
        assert!(!captured.contains("accounts.google.com"));
        assert!(!captured.contains("private-marker"));
        assert!(captured.contains("redacted"));
    }

    #[test]
    fn stderr_heartbeats_do_not_displace_startup_diagnostics() {
        let log = AgentStderrLog::new("heartbeat-fixture");
        log.log_line("native failure detail");
        for _ in 0..STARTUP_STDERR_MAX_LINES + 1 {
            log.log_line("");
            log.log_line("\r");
        }
        assert_eq!(log.mark_failed(), vec!["native failure detail"]);
    }

    #[tokio::test]
    async fn stderr_reader_keeps_draining_after_non_utf8_bytes() {
        let mut command = tokio::process::Command::new("pwsh.exe");
        command
            .args([
                "-NoProfile",
                "-Command",
                "$e=[Console]::OpenStandardError();$e.Write([byte[]](255,10,111,107,10),0,5);$e.Flush()",
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let mut child = super::super::process::AgentProcess::spawn(&mut command, None).unwrap();
        let log = AgentStderrLog::new("lossy-stderr-fixture");
        let drain = log.drain(child.stderr.take().unwrap());
        tokio::time::timeout(Duration::from_secs(20), drain)
            .await
            .unwrap()
            .unwrap();
        child.terminate().await.unwrap();
        assert_eq!(
            log.mark_failed(),
            vec![char::REPLACEMENT_CHARACTER.to_string(), "ok".to_string()]
        );
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

    /// `mark_failed` must hand the captured lines back, not just log them —
    /// they are what turns a pane's "oneshot canceled" into an actionable
    /// message. The second call returns empty so a caller can't double-report.
    #[test]
    fn mark_failed_returns_the_captured_lines_once() {
        let log = AgentStderrLog::new("test-agent");
        log.log_line("cannot preserve mount namespace");
        log.log_line("unexpected eof from helper process");

        let promoted = log.mark_failed();
        assert_eq!(
            promoted,
            vec![
                "cannot preserve mount namespace".to_string(),
                "unexpected eof from helper process".to_string(),
            ]
        );
        assert!(log.mark_failed().is_empty(), "second call must be empty");
    }

    /// An agent that initialized successfully has nothing to promote, so a
    /// later failure path can't resurrect stale startup noise.
    #[test]
    fn mark_failed_returns_nothing_after_initialize() {
        let log = AgentStderrLog::new("test-agent");
        log.log_line("startup detail");
        log.mark_initialized();
        assert!(log.mark_failed().is_empty());
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
