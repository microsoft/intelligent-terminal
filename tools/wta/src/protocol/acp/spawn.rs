//! Shared agent-process spawn logic for the ACP layer.
//!
//! Both [`crate::master`] (spawning the shared agent CLI) and
//! [`super::probe::probe_models`] need to spawn an ACP agent the same
//! way: parse the user-facing cmdline, resolve bare names via
//! [`crate::agent_registry`], optionally wrap in `cmd /c`, scrub the
//! claude-code-acp guard env var, and pipe stdio with `kill_on_drop`.
//! They diverge only after `spawn()` — master drives a full prompt loop
//! over the helper pipes; the probe attaches raw stdio, runs `initialize`
//! + `new_session`, and exits.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Result};
use tokio::io::{AsyncBufReadExt, BufReader};

const STARTUP_STDERR_MAX_LINES: usize = 32;
const STARTUP_STDERR_MAX_CHARS_PER_LINE: usize = 1024;
const STARTUP_STDERR_DRAIN_TIMEOUT: Duration = Duration::from_millis(250);

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
        self.mark_failed();
        let _ = child.start_kill();
        if let Some(stderr_task) = stderr_task {
            let _ = tokio::time::timeout(STARTUP_STDERR_DRAIN_TIMEOUT, stderr_task).await;
        }
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
        let warn = {
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
                    false
                }
                StderrPhase::Running => false,
                StderrPhase::Failed => true,
            }
        };

        if warn {
            let line = truncate_stderr_line(line);
            tracing::warn!(
                target: "agent_stderr",
                agent = %self.agent,
                phase = "startup_failure",
                "{line}"
            );
        } else {
            tracing::debug!(target: "agent_stderr", agent = %self.agent, "{line}");
        }
    }
}

fn truncate_stderr_line(line: &str) -> String {
    let mut chars = line.chars();
    let mut truncated = chars
        .by_ref()
        .take(STARTUP_STDERR_MAX_CHARS_PER_LINE)
        .collect::<String>();
    if chars.next().is_some() {
        truncated.push_str("...");
    }
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
        self.adapter_package
            .as_deref()
            .unwrap_or(&self.raw_program)
    }
}

/// Spawn the ACP agent process.
///
/// `cwd` pins the child's working directory to the user's active pane cwd so
/// the agent's `execute_command` tool — which inherits the agent process cwd
/// when its shell wrapper doesn't explicitly set one — starts in the user's
/// project. None preserves the parent's cwd (probe path, where it doesn't
/// matter).
pub(crate) fn spawn_agent_process(agent_cmd: &str, cwd: Option<&Path>) -> Result<AgentSpawn> {
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

    let program = if needs_cmd { "cmd" } else { resolved_program.as_str() };
    let mut cmd = tokio::process::Command::new(program);
    if needs_cmd {
        cmd.arg("/c").arg(&resolved_program);
    }
    // The claude-code-acp adapter refuses to start when its recursion-
    // guard env var is set — that guard exists to block recursive
    // `claude` shells from sharing runtime, but doesn't apply to an
    // ACP host. Scrub unconditionally; other agents don't care.
    cmd.env_remove("CLAUDECODE");

    // Give the agent CLI a PATH rebuilt from the Windows registry. Windows
    // Terminal — and thus this wta-master / wta child — snapshots its
    // environment at start, so an agent CLI installed mid-session (e.g. the
    // FRE winget-installing `copilot` while WT is already running) is invisible
    // to our inherited PATH. That makes `cmd /c copilot` (or a bare spawn) fail
    // with "is not recognized", which the master reports as an immediate
    // ACP-initialize failure. Setting the child's PATH here fixes resolution
    // for both the `cmd /c` and direct-spawn cases without requiring a full WT
    // restart. (Recent Rust resolves the program name against the child env's
    // PATH when one is provided.)
    if let Some(path) = crate::agent_check::spawn_path() {
        cmd.env("PATH", path);
    }

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
