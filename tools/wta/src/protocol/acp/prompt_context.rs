//! Pluggable prompt-context injection for ACP planner / autofix prompts.
//!
//! Prompts shipped to the agent CLI carry a set of `### …` runtime context
//! sections (delegate agents, terminal layout, shell info, the failing
//! command's output, on-demand command resolver invocation, …). These used to be
//! assembled by inline `runtime_sections.push(format!("### X\n…"))` calls
//! scattered across two mutually-exclusive branches of `build_prompt_text`;
//! adding a source meant another nested `if let … push(…)` block.
//!
//! This module turns each source into a [`ContextProvider`]: it declares when
//! it [`applies`](ContextProvider::applies) and asynchronously
//! [`provide`](ContextProvider::provide)s at most one [`ContextSection`].
//! `build_prompt_text` resolves the shared inputs once into a
//! [`ContextRequest`], then runs [`default_providers`] in order — no source is
//! hand-stuffed.
//!
//! Command resolution is agent-initiated through [`CommandResolverProvider`]'s
//! invocation contract. Prompt assembly never enumerates shell commands.

use async_trait::async_trait;

use crate::coordinator::default_supported_delegate_agents;
use crate::pane_context::PaneContext;
use crate::shell::ShellManager;

const ACTIVE_PANE_CONTEXT_MAX_CHARS: usize = 4000;

fn truncate_for_prompt(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let truncated: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_none() {
        text.to_string()
    } else {
        format!("{truncated}\n...<truncated>")
    }
}

fn preserve_protocol_truncation(text: &str, max_chars: usize, protocol_truncated: bool) -> String {
    let bounded = truncate_for_prompt(text, max_chars);
    if protocol_truncated && !bounded.ends_with("...<truncated>") {
        format!("{bounded}\n...<truncated>")
    } else {
        bounded
    }
}

fn json_str_or_num(value: Option<&serde_json::Value>) -> Option<String> {
    match value {
        Some(serde_json::Value::String(s)) => Some(s.clone()),
        Some(serde_json::Value::Number(n)) => Some(n.to_string()),
        _ => None,
    }
}

/// Read the most recent shell-integration command (prompt + command + output)
/// for `pane_id`. Falls back to a line-count read when shell integration is
/// not active (e.g. CMD, plain bash without OSC 133 support).
///
/// Returns the (possibly truncated) content as a string. `None` on failure.
///
/// Emits structured tracing under target `acp.last_message` so the call chain
/// is visible in `wta-{process}.log`:
///   * `last_message_request`  — start, with pane_id and budgets
///   * `last_message_result`   — outcome: marks_hit | fallback_used | empty
async fn read_pane_last_message_legacy(
    shell_mgr: &ShellManager,
    pane_id: &str,
    fallback_lines: u32,
    max_chars: usize,
) -> Option<String> {
    let started = std::time::Instant::now();
    tracing::debug!(
        target: "acp.last_message",
        pane_id,
        fallback_lines,
        max_chars,
        "last_message_request"
    );

    let mark_call_started = std::time::Instant::now();
    let mark_result = shell_mgr.wt_read_last_prompt(pane_id).await;
    let mark_call_ms = mark_call_started.elapsed().as_millis() as u64;

    match &mark_result {
        Ok(value) => {
            let has_marks = value
                .get("has_marks")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let raw_len = value
                .get("content")
                .and_then(|c| c.as_str())
                .map(str::len)
                .unwrap_or(0);
            tracing::debug!(
                target: "acp.last_message",
                pane_id,
                has_marks,
                raw_len,
                rpc_ms = mark_call_ms,
                "last_message_rpc_ok"
            );
            if has_marks {
                if let Some(content) = value.get("content").and_then(|c| c.as_str()) {
                    if !content.is_empty() {
                        let truncated = truncate_for_prompt(content, max_chars);
                        tracing::debug!(
                            target: "acp.last_message",
                            pane_id,
                            path = "marks_hit",
                            out_len = truncated.len(),
                            total_ms = started.elapsed().as_millis() as u64,
                            "last_message_result"
                        );
                        return Some(truncated);
                    }
                }
            }
        }
        Err(err) => {
            tracing::debug!(
                target: "acp.last_message",
                pane_id,
                rpc_ms = mark_call_ms,
                error = %err,
                "last_message_rpc_err"
            );
        }
    }

    // Fallback: shell integration absent or call failed — use line-count read.
    let fb_started = std::time::Instant::now();
    let result = shell_mgr
        .wt_read_pane_output(pane_id, Some(fallback_lines))
        .await
        .ok()
        .and_then(|value| {
            value
                .get("content")
                .and_then(|content| content.as_str())
                .map(|content| truncate_for_prompt(content, max_chars))
        });
    let fb_ms = fb_started.elapsed().as_millis() as u64;

    match &result {
        Some(text) => tracing::debug!(
            target: "acp.last_message",
            pane_id,
            path = "fallback_used",
            fallback_lines,
            out_len = text.len(),
            fallback_ms = fb_ms,
            total_ms = started.elapsed().as_millis() as u64,
            "last_message_result"
        ),
        None => tracing::debug!(
            target: "acp.last_message",
            pane_id,
            path = "empty",
            fallback_lines,
            fallback_ms = fb_ms,
            total_ms = started.elapsed().as_millis() as u64,
            "last_message_result"
        ),
    }

    result
}

/// Best-effort absolute process image path for a pid.
#[cfg(windows)]
fn process_image_path(pid: u32) -> Option<String> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    if pid == 0 {
        return None;
    }
    // SAFETY: a standard Win32 handle dance. The handle from OpenProcess is
    // closed on every return path; the buffer is sized up front and the
    // written length comes back in `size`.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        // Not MAX_PATH: QueryFullProcessImageNameW can return paths longer than
        // 260 for processes under long roots (WindowsApps installs, `\\?\`
        // extended paths). Use the extended-length max so a valid pid never
        // silently drops the `shell` field. Heap-allocated to keep it off the
        // (smaller) task stack.
        let mut size: u32 = 32768;
        let mut buf = vec![0u16; size as usize];
        let ok =
            QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut size);
        CloseHandle(handle);
        if ok == 0 || size == 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..size as usize]))
    }
}

#[cfg(not(windows))]
fn process_image_path(_pid: u32) -> Option<String> {
    None
}

/// Best-effort canonical shell executable for a pid — e.g. `pwsh.exe`,
/// `powershell.exe`, `cmd.exe`, `bash.exe`, `wsl.exe`. Unlike the WT profile
/// *name* (which the user can rename), this is the actual running process, so
/// the agent can reliably pick shell syntax. Returns the file name only;
/// `None` on any failure (or off Windows).
fn process_image_name(pid: u32) -> Option<String> {
    process_image_path(pid).and_then(|full| {
        full.rsplit(['\\', '/'])
            .next()
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    })
}

/// Resolve the shell identity for an active-pane JSON object. The agent gets
/// this as the `shell` field — the shell-type signal that drives PowerShell vs
/// bash vs cmd syntax in any fix command it suggests.
///
/// Resolution order:
///   1. The `shell` field reported by shell integration via `OSC 9001;ShellType`
///      (e.g. `pwsh`, `powershell`, `bash`, `wsl:Ubuntu`). This is the only
///      signal that survives a nested shell — `pwsh` → `wsl` → `exit` reports
///      `wsl:<distro>` while inside WSL and `pwsh` again after exit, because the
///      shell re-emits it on every prompt. The pid-based fallback below can't
///      see this: the pane's host process stays `wsl.exe`/`pwsh.exe` regardless
///      of which shell is actually drawing the prompt.
///   2. Otherwise, the canonical shell exe from the pane's `pid` (covers panes
///      without shell integration installed, or before the first prompt).
pub(super) fn shell_from_active(active: &serde_json::Value) -> Option<String> {
    if let Some(shell) = active
        .get("shell")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Some(shell.to_string());
    }
    active
        .get("pid")
        .and_then(|v| v.as_u64())
        .and_then(|pid| process_image_name(pid as u32))
}

/// Resolve a pane's full JSON (`shell`, `cwd`, `session_id`, `pid`, …) by its
/// **session id**, enumerating windows → tabs → panes via the protocol. Used by
/// error-triggered autofix, where the failing pane can live in a non-focused
/// tab and so is **not** the active pane returned by `get_active_pane`.
///
/// We deliberately resolve by session id rather than scoping `list_panes` to a
/// tab: in autofix `PaneContext.tab_id` is the WT tab *StableId* (see
/// `WtNotification.tab_id`), not the numeric protocol tab index that
/// `list_panes` expects, so scoping by it would never match and would silently
/// fall back to the wrong (active) pane. Enumerating by session id — using each
/// tab's protocol `tab_id` from `list_tabs` for the inner `list_panes` call —
/// sidesteps the id-space mismatch entirely. Returns `None` when no pane
/// matches (channel error, pane closed).
async fn resolve_pane_by_session_id(
    shell_mgr: &ShellManager,
    session_id: &str,
) -> Option<serde_json::Value> {
    let windows = shell_mgr.wt_list_windows().await.ok()?;
    for win in windows.get("windows")?.as_array()? {
        let Some(window_id) = json_str_or_num(win.get("window_id")) else {
            continue;
        };
        let Ok(tabs) = shell_mgr.wt_list_tabs(&window_id).await else {
            continue;
        };
        let Some(tabs_arr) = tabs.get("tabs").and_then(|v| v.as_array()) else {
            continue;
        };
        for tab in tabs_arr {
            // Protocol tab index (from `list_tabs`), which `list_panes` accepts
            // — NOT the autofix StableId.
            let Some(tab_id) = json_str_or_num(tab.get("tab_id")) else {
                continue;
            };
            let Ok(panes) = shell_mgr
                .wt_list_panes(&tab_id, Some(window_id.as_str()))
                .await
            else {
                continue;
            };
            let Some(panes_arr) = panes.get("panes").and_then(|v| v.as_array()) else {
                continue;
            };
            if let Some(pane) = panes_arr
                .iter()
                .find(|p| json_str_or_num(p.get("session_id")).as_deref() == Some(session_id))
            {
                return Some(pane.clone());
            }
        }
    }
    None
}

struct CapturedPaneContext {
    pane: serde_json::Value,
    output: Option<String>,
}

fn validate_pane_context(value: &serde_json::Value) -> Result<&serde_json::Value, &'static str> {
    if !value.is_object() {
        return Err("response must be an object");
    }
    let pane = value.get("pane").ok_or("missing required pane")?;
    if !pane.is_object() {
        return Err("pane must be an object");
    }
    if pane
        .get("session_id")
        .and_then(serde_json::Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .is_none()
    {
        return Err("pane.session_id must be a nonempty string");
    }
    if pane.get("is_agent_pane").is_none_or(|v| !v.is_boolean()) {
        return Err("pane.is_agent_pane must be a boolean");
    }
    if value.get("content").is_none_or(|v| !v.is_string()) {
        return Err("content must be a string");
    }
    for field in ["truncated", "has_marks"] {
        if value.get(field).is_none_or(|v| !v.is_boolean()) {
            return Err(match field {
                "truncated" => "truncated must be a boolean",
                _ => "has_marks must be a boolean",
            });
        }
    }
    for field in ["output_source", "fallback_reason"] {
        if value.get(field).is_none_or(|v| !v.is_string()) {
            return Err(match field {
                "output_source" => "output_source must be a string",
                _ => "fallback_reason must be a string",
            });
        }
    }
    if value.get("line_count").is_none_or(|v| v.as_u64().is_none()) {
        return Err("line_count must be a nonnegative integer");
    }
    Ok(pane)
}

async fn capture_pane_context(
    shell_mgr: &ShellManager,
    explicit_source: Option<&str>,
    max_lines: u32,
    max_chars: usize,
) -> Option<CapturedPaneContext> {
    let started = std::time::Instant::now();
    let (pane, response) = match shell_mgr
        .wt_get_pane_context(explicit_source, max_lines, max_chars)
        .await
    {
        Ok(value) => {
            let pane = match validate_pane_context(&value) {
                Ok(pane) => pane.clone(),
                Err(error) => {
                    tracing::debug!(
                        target: "acp.terminal_context",
                        explicit_source = explicit_source.is_some(),
                        rpc_ms = started.elapsed().as_millis() as u64,
                        error,
                        "pane_context_response_contract_error"
                    );
                    return None;
                }
            };
            (pane, Some(value))
        }
        Err(error) if format!("{error:#}").contains("WT_PROTOCOL_UNSUPPORTED_PANE_CONTEXT") => {
            tracing::warn!(
                target: "acp.terminal_context",
                explicit_source = explicit_source.is_some(),
                "pane_context_legacy_fallback"
            );
            let pane = match explicit_source {
                Some(source) => resolve_pane_by_session_id(shell_mgr, source).await?,
                None => shell_mgr.wt_get_active_pane().await.ok()?,
            };
            (pane, None)
        }
        Err(error) => {
            tracing::debug!(
                target: "acp.terminal_context",
                explicit_source = explicit_source.is_some(),
                rpc_ms = started.elapsed().as_millis() as u64,
                error = %error,
                "pane_context_request_failed"
            );
            return None;
        }
    };
    if pane
        .get("is_agent_pane")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        return None;
    }

    let output = if let Some(value) = response {
        let protocol_truncated = value
            .get("truncated")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let output = value
            .get("content")
            .and_then(serde_json::Value::as_str)
            .filter(|content| !content.is_empty())
            .map(|content| preserve_protocol_truncation(content, max_chars, protocol_truncated));
        tracing::debug!(
            target: "acp.terminal_context",
            explicit_source = explicit_source.is_some(),
            rpc_ms = started.elapsed().as_millis() as u64,
            output_source = value
                .get("output_source")
                .and_then(serde_json::Value::as_str),
            fallback_reason = value
                .get("fallback_reason")
                .and_then(serde_json::Value::as_str),
            truncated = value.get("truncated").and_then(serde_json::Value::as_bool),
            "pane_context_request_complete"
        );
        output
    } else {
        let pane_id = json_str_or_num(pane.get("session_id"))?;
        read_pane_last_message_legacy(shell_mgr, &pane_id, max_lines, max_chars).await
    };
    Some(CapturedPaneContext { pane, output })
}

struct PlannerTerminalContext {
    json: String,
    target_pane_id: String,
    resolver_invocation: Option<crate::agent_tools::command_resolution::CommandResolverInvocation>,
}

async fn build_terminal_context(
    shell_mgr: &ShellManager,
    pane_context: Option<&PaneContext>,
) -> Option<PlannerTerminalContext> {
    let captured = capture_pane_context(
        shell_mgr,
        pane_context.and_then(|context| context.source_pane_id.as_deref()),
        24,
        ACTIVE_PANE_CONTEXT_MAX_CHARS,
    )
    .await?;
    let active = captured.pane;

    let target_pane_id = json_str_or_num(active.get("session_id"))?;
    let target_window_title = active
        .get("title")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let target_cwd = active
        .get("cwd")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    // Canonical shell exe (pwsh.exe / cmd.exe / wsl.exe …) from the pane's pid.
    // Load-bearing for the planner: any `send` action it emits has to match the
    // active pane's shell syntax (`Get-ChildItem` vs `ls`, `Set-Location` vs
    // `cd`, etc.). We use the real process rather than the WT profile name,
    // which the user can rename.
    let target_shell = shell_from_active(&active);
    let resolver_invocation = command_resolver_invocation(target_shell.as_deref(), Some(&active));

    tracing::debug!(
        target: "acp.terminal_context",
        target_pane_id = %target_pane_id,
        shell = ?target_shell,
        "terminal_context_target_resolved"
    );

    let json = serde_json::to_string(&serde_json::json!({
        "activeTarget": target_pane_id,
        "window_title": target_window_title,
        "cwd": target_cwd,
        "shell": target_shell,
        "locale": user_locale_tag(),
        "buffer": captured.output,
    }))
    .ok()?;

    Some(PlannerTerminalContext {
        json,
        target_pane_id,
        resolver_invocation,
    })
}

/// User's UI locale as a BCP-47 tag, suitable for embedding in
/// runtime context JSON shipped to the agent.
///
/// Pseudo-locales (`qps-ploc*`) are passed through verbatim. Unlike
/// `LANG`/`LC_ALL` in `spawn.rs` — which feed libc and have to be real
/// POSIX locales — this field is just metadata for an LLM, which will
/// either recognise the tag or treat it as opaque text. Either way it's
/// honest: it reflects exactly what the user picked in the UI.
fn user_locale_tag() -> String {
    rust_i18n::locale().to_string()
}

pub(super) struct ResolvedProviderContext {
    pub(super) context_pane: Option<serde_json::Value>,
    pub(super) shell_exe: Option<String>,
    pub(super) terminal_output: Option<String>,
    pub(super) resolved_fix_pane: Option<String>,
    pub(super) planner_terminal_context: Option<String>,
    pub(super) resolved_planner_pane: Option<String>,
    pub(super) command_resolver_invocation:
        Option<crate::agent_tools::command_resolution::CommandResolverInvocation>,
}

pub(super) async fn resolve_provider_context(
    is_autofix: bool,
    wt_connected: bool,
    shell_mgr: &ShellManager,
    pane_context: Option<&PaneContext>,
) -> ResolvedProviderContext {
    let mut resolved = ResolvedProviderContext {
        context_pane: None,
        shell_exe: None,
        terminal_output: None,
        resolved_fix_pane: None,
        planner_terminal_context: None,
        resolved_planner_pane: None,
        command_resolver_invocation: if is_autofix {
            None
        } else {
            command_resolver_invocation(None, None)
        },
    };
    if !wt_connected {
        return resolved;
    }
    if !is_autofix {
        if let Some(context) = build_terminal_context(shell_mgr, pane_context).await {
            resolved.planner_terminal_context = Some(context.json);
            resolved.resolved_planner_pane = Some(context.target_pane_id);
            resolved.command_resolver_invocation = context.resolver_invocation;
        }
        return resolved;
    }

    let explicit_source = pane_context.and_then(|ctx| ctx.source_pane_id.as_deref());
    let Some(captured) = capture_pane_context(
        shell_mgr,
        explicit_source,
        30,
        ACTIVE_PANE_CONTEXT_MAX_CHARS,
    )
    .await
    else {
        return resolved;
    };

    let source_pane_id = json_str_or_num(captured.pane.get("session_id"));
    if explicit_source.is_none() {
        resolved.resolved_fix_pane = source_pane_id.clone();
    }
    resolved.shell_exe = shell_from_active(&captured.pane);
    resolved.command_resolver_invocation =
        command_resolver_invocation(resolved.shell_exe.as_deref(), Some(&captured.pane));
    resolved.context_pane = Some(captured.pane);
    resolved.terminal_output = captured.output;

    tracing::debug!(
        target: "acp.terminal_context",
        source_pane_id = ?source_pane_id,
        shell = ?resolved.shell_exe,
        mode = "autofix",
        "terminal_context_target_resolved"
    );

    resolved
}

/// Read-only inputs a [`ContextProvider`] may consult when deciding whether it
/// applies and what section to emit.
///
/// [`resolve_provider_context`] resolves the expensive shared bits (the active
/// pane, its canonical shell, the failing pane's last output) **once** and
/// lends them here, so providers never re-query WT. Autofix-only fields are
/// `None` for planner turns and vice-versa; providers gate on them in
/// [`applies`](ContextProvider::applies).
pub(super) struct ContextRequest<'a> {
    /// True for an auto-fix / `/fix` turn; false for a planner turn.
    pub(super) is_autofix: bool,
    /// Whether the WT protocol channel is live (pane queries are meaningful).
    pub(super) wt_connected: bool,
    /// Autofix only: the JSON of the pane whose shell/cwd describe the failing
    /// command (the source pane — for error-triggered autofix this can be a
    /// pane in a non-focused tab, not the active pane). `None` when WT is not
    /// connected / no pane resolved.
    pub(super) context_pane: Option<&'a serde_json::Value>,
    /// Autofix only: the canonical shell exe of the failing pane
    /// (`pwsh.exe` / `cmd.exe` / `wsl.exe` …), from its pid.
    pub(super) shell_exe: Option<&'a str>,
    /// Autofix only: the failing pane's last `[command + output]` buffer.
    pub(super) terminal_output: Option<&'a str>,
    /// Planner only: terminal context assembled with its authoritative target.
    pub(super) planner_terminal_context: Option<&'a str>,
    /// Resolver contract derived from the same authoritative pane.
    pub(super) command_resolver_invocation:
        Option<&'a crate::agent_tools::command_resolution::CommandResolverInvocation>,
}

/// One `### {heading}\n{body}` block to inject into the prompt. `heading` is
/// fixed per provider; `body` is the provider's already-formatted content
/// (including any code fences). The leading `### ` and the heading/body
/// newline are added by [`ContextSection::render`], so every provider produces
/// a uniformly-shaped section.
pub(super) struct ContextSection {
    heading: &'static str,
    body: String,
}

impl ContextSection {
    /// Render to the exact `### {heading}\n{body}` text appended to the prompt.
    pub(super) fn render(&self) -> String {
        format!("### {}\n{}", self.heading, self.body)
    }
}

/// A single, self-contained source of prompt context.
///
/// Implementors decide *when* they run ([`applies`](Self::applies)) and *what*
/// they emit ([`provide`](Self::provide)). Keeping the two split lets the
/// assembler skip the (possibly expensive) `provide` for a provider that does
/// not apply, and lets `provide` return `None` when it applies in principle but
/// has nothing to add this turn (e.g. the pane context is unavailable).
#[async_trait]
pub(super) trait ContextProvider: Send + Sync {
    /// Stable identifier, used for per-provider timing logs.
    fn id(&self) -> &'static str;

    /// Cheap, synchronous gate: does this provider run for `req` at all?
    fn applies(&self, req: &ContextRequest<'_>) -> bool;

    /// Produce the section, or `None` when there is nothing to inject.
    async fn provide(&self, req: &ContextRequest<'_>) -> Option<ContextSection>;
}

/// The ordered provider chain `build_prompt_text` runs. Order is the order
/// sections appear in the prompt; mutually-exclusive planner / autofix
/// providers self-gate via [`ContextProvider::applies`], so the same chain
/// serves both turn kinds.
///
/// Every provider is a zero-sized, stateless unit struct, so the chain is a
/// `&'static` slice of const-promoted instances — no per-prompt allocation.
pub(super) fn default_providers() -> &'static [&'static dyn ContextProvider] {
    &[
        // Both turn kinds.
        &CommandResolverProvider,
        // Planner turns.
        &DelegateAgentsProvider,
        &TerminalContextProvider,
        // Autofix turns.
        &ShellContextProvider,
        &TerminalOutputProvider,
    ]
}

/// A deterministic invocation of this WTA installation's local
/// command resolver through the package execution alias injected into the
/// agent CLI's PATH.
struct CommandResolverProvider;

pub(super) fn command_resolver_invocation(
    pane_shell: Option<&str>,
    pane: Option<&serde_json::Value>,
) -> Option<crate::agent_tools::command_resolution::CommandResolverInvocation> {
    if pane_shell
        .is_some_and(|shell| !crate::agent_tools::command_resolution::has_applicable_source(shell))
    {
        return None;
    }

    let executable = "wta.exe".to_string();
    let cwd = pane
        .and_then(|pane| pane.get("cwd"))
        .and_then(serde_json::Value::as_str)
        .filter(|cwd| !cwd.is_empty())
        .map(str::to_string);

    let mut shell = pane_shell.unwrap_or("unknown").to_string();
    if crate::command_recall::is_powershell(&shell) && !std::path::Path::new(&shell).is_absolute() {
        if let Some(path) = pane
            .and_then(|pane| pane.get("pid"))
            .and_then(serde_json::Value::as_u64)
            .and_then(|pid| u32::try_from(pid).ok())
            .and_then(process_image_path)
            .filter(|path| crate::command_recall::is_powershell(path))
        {
            shell = path;
        }
    }

    Some(
        crate::agent_tools::command_resolution::CommandResolverInvocation::new(
            executable, shell, cwd,
        ),
    )
}

#[async_trait]
impl ContextProvider for CommandResolverProvider {
    fn id(&self) -> &'static str {
        "command_resolver"
    }

    fn applies(&self, req: &ContextRequest<'_>) -> bool {
        req.command_resolver_invocation.is_some()
    }

    async fn provide(&self, req: &ContextRequest<'_>) -> Option<ContextSection> {
        let invocation = req.command_resolver_invocation?;
        let contract = serde_json::to_string_pretty(&invocation.contract("<name>")).ok()?;
        let cwd_instruction = if invocation.cwd().is_some() {
            "Keep the injected `--cwd` value unchanged so resolution uses the \
             target pane's working directory. "
        } else {
            ""
        };
        Some(ContextSection {
            heading: "Command Resolver Invocation",
            body: format!(
                "This optional local CLI queries command existence and, for missing \
                 PowerShell commands, similar installed names. Invoke it only when \
                 diagnosis needs command resolution, not routinely on every failure. \
                 Propose an obvious typo correction in a familiar command directly, \
                 without querying merely to verify it. Query when an unfamiliar local \
                 command or genuine ambiguity requires local evidence; do not invent \
                 local command names. A command-not-found error alone does not require \
                 a query. \
                 `exists` identifies a resolved command; `not_found` reports no \
                 resolution from an authoritative source; `indeterminate` and \
                 `unsupported` do not prove absence. It cannot observe aliases or \
                 functions defined only in the running pane's memory. \
                 Keep the injected `--shell` value unchanged. \
                 Replace `<name>` with the command name as one argument. Prefer \
                 invoking `executable` with each `arguments` entry as a separate \
                 argv. {}Use `powershell` only when the tool executes a PowerShell \
                 command string; replace `<name>` inside its existing \
                 single quotes and double every embedded `'` in the command name.\n\
                 ```json\n{}\n```",
                cwd_instruction, contract
            ),
        })
    }
}

/// Planner: the agents this build can delegate to (`?<prompt>` etc.).
struct DelegateAgentsProvider;

#[async_trait]
impl ContextProvider for DelegateAgentsProvider {
    fn id(&self) -> &'static str {
        "delegate_agents"
    }

    fn applies(&self, req: &ContextRequest<'_>) -> bool {
        !req.is_autofix
    }

    async fn provide(&self, _req: &ContextRequest<'_>) -> Option<ContextSection> {
        let json = serde_json::to_string(&default_supported_delegate_agents())
            .unwrap_or_else(|_| "[]".to_string());
        Some(ContextSection {
            heading: "Supported Delegate Agents",
            body: format!("```json\n{}\n```", json),
        })
    }
}

/// Planner: the full terminal layout / active-target context JSON.
struct TerminalContextProvider;

#[async_trait]
impl ContextProvider for TerminalContextProvider {
    fn id(&self) -> &'static str {
        "terminal_context"
    }

    fn applies(&self, req: &ContextRequest<'_>) -> bool {
        !req.is_autofix && req.wt_connected && req.planner_terminal_context.is_some()
    }

    async fn provide(&self, req: &ContextRequest<'_>) -> Option<ContextSection> {
        let json = req.planner_terminal_context?;
        Some(ContextSection {
            heading: "Terminal Context JSON",
            body: format!("```json\n{}\n```", json),
        })
    }
}

/// Autofix: a small `{shell, cwd, locale}` header so the agent picks the right
/// shell syntax for any file-edit fix it suggests.
struct ShellContextProvider;

#[async_trait]
impl ContextProvider for ShellContextProvider {
    fn id(&self) -> &'static str {
        "shell_context"
    }

    fn applies(&self, req: &ContextRequest<'_>) -> bool {
        req.is_autofix && req.context_pane.is_some()
    }

    async fn provide(&self, req: &ContextRequest<'_>) -> Option<ContextSection> {
        let pane = req.context_pane?;
        let cwd = pane
            .get("cwd")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let json = serde_json::to_string(&serde_json::json!({
            "shell": req.shell_exe,
            "cwd": cwd,
            "locale": user_locale_tag(),
        }))
        .unwrap_or_else(|_| "{}".to_string());
        Some(ContextSection {
            heading: "Shell Context",
            body: format!("```json\n{}\n```", json),
        })
    }
}

/// Autofix: the failing pane's last `[command + output]` buffer.
struct TerminalOutputProvider;

#[async_trait]
impl ContextProvider for TerminalOutputProvider {
    fn id(&self) -> &'static str {
        "terminal_output"
    }

    fn applies(&self, req: &ContextRequest<'_>) -> bool {
        req.is_autofix && req.terminal_output.is_some()
    }

    async fn provide(&self, req: &ContextRequest<'_>) -> Option<ContextSection> {
        let content = req.terminal_output?;
        Some(ContextSection {
            heading: "Terminal Output",
            body: format!("```\n{}\n```", content),
        })
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::shell::ShellManager;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    };

    /// `shell_from_active` resolves our own pid to a real exe name (the test
    /// binary). Proves the pid → image-name path works end to end on Windows;
    /// a missing/zero pid yields `None`.
    #[cfg(windows)]
    #[test]
    fn shell_from_active_resolves_pid() {
        let me = serde_json::json!({ "pid": std::process::id() });
        let name = shell_from_active(&me).expect("own pid should resolve");
        assert!(
            name.to_ascii_lowercase().ends_with(".exe"),
            "expected an .exe image name, got {name:?}"
        );

        assert_eq!(shell_from_active(&serde_json::json!({ "pid": 0 })), None);
        assert_eq!(shell_from_active(&serde_json::json!({})), None);
    }

    /// The `shell` field reported via `OSC 9001;ShellType` wins over the
    /// pid-based fallback — even when a real pid is present. This is the
    /// nested-shell case (`pwsh` → `wsl` → bash): the pane's host process is
    /// still pwsh/wsl.exe, but the prompt is drawn by bash, so the OSC-reported
    /// `wsl:Ubuntu` must reach the agent. Platform-independent (no pid lookup).
    #[test]
    fn shell_from_active_prefers_osc_reported_shell() {
        // Reported shell wins over a live pid.
        let pane = serde_json::json!({ "pid": std::process::id(), "shell": "wsl:Ubuntu" });
        assert_eq!(shell_from_active(&pane), Some("wsl:Ubuntu".to_string()));

        // Empty/whitespace reported shell is ignored; falls back to pid (or None).
        assert_eq!(
            shell_from_active(&serde_json::json!({ "shell": "  ", "pid": 0 })),
            None
        );
        assert_eq!(shell_from_active(&serde_json::json!({ "shell": "" })), None);
    }

    #[test]
    fn user_locale_tag_returns_current_locale_verbatim() {
        let _g = crate::test_support::lock_locale();
        // Real locales pass through unchanged.
        rust_i18n::set_locale("zh-CN");
        assert_eq!(user_locale_tag(), "zh-CN");
        rust_i18n::set_locale("en-US");
        assert_eq!(user_locale_tag(), "en-US");
        // Pseudo-locales are passed through too — agents treat unknown
        // BCP-47 tags as opaque metadata, so there's no need to remap.
        rust_i18n::set_locale("qps-ploca");
        assert_eq!(user_locale_tag(), "qps-ploca");
    }

    struct MockWtChannel {
        active_pane: serde_json::Value,
        source_pane: Option<serde_json::Value>,
        source_output: String,
    }

    #[async_trait::async_trait]
    impl crate::shell::wt_channel::WtChannel for MockWtChannel {
        async fn request(
            &self,
            method: &str,
            params: serde_json::Value,
        ) -> anyhow::Result<serde_json::Value> {
            match method {
                "get_pane_context" => {
                    let (pane, output) = if let Some(session_id) = params.get("session_id") {
                        let pane = self
                            .source_pane
                            .as_ref()
                            .filter(|pane| pane.get("session_id") == Some(session_id))
                            .ok_or_else(|| anyhow::anyhow!("MockWtChannel: source pane missing"))?;
                        (pane, self.source_output.as_str())
                    } else {
                        (&self.active_pane, "")
                    };
                    Ok(serde_json::json!({
                        "pane": pane,
                        "content": output,
                        "output_source": if output.is_empty() { "metadata_only" } else { "last_command" },
                        "fallback_reason": "",
                        "line_count": output.lines().count(),
                        "truncated": false,
                        "has_marks": !output.is_empty(),
                    }))
                }
                other => Err(anyhow::anyhow!("MockWtChannel: unhandled method {other}")),
            }
        }

        fn is_available(&self) -> bool {
            true
        }
    }

    fn shell_mgr_with_pane(active_pane: serde_json::Value) -> ShellManager {
        shell_mgr_with_source_pane(active_pane, None, "")
    }

    pub(crate) fn shell_mgr_with_source_pane(
        active_pane: serde_json::Value,
        source_pane: Option<serde_json::Value>,
        source_output: &str,
    ) -> ShellManager {
        ShellManager::new().with_wt_channel(Arc::new(MockWtChannel {
            active_pane,
            source_pane,
            source_output: source_output.to_string(),
        }))
    }

    fn pane_context_response() -> serde_json::Value {
        serde_json::json!({
            "pane": {
                "session_id": "pane-explicit",
                "is_agent_pane": false,
            },
            "content": "command output",
            "output_source": "last_command",
            "fallback_reason": "",
            "line_count": 1,
            "truncated": false,
            "has_marks": true,
        })
    }

    struct RecordingPaneContextChannel {
        requests: AtomicUsize,
        params: Mutex<Option<serde_json::Value>>,
        error: Option<&'static str>,
        response: Option<serde_json::Value>,
    }

    #[async_trait::async_trait]
    impl crate::shell::wt_channel::WtChannel for RecordingPaneContextChannel {
        async fn request(
            &self,
            method: &str,
            params: serde_json::Value,
        ) -> anyhow::Result<serde_json::Value> {
            assert_eq!(method, "get_pane_context");
            self.requests.fetch_add(1, Ordering::Relaxed);
            *self.params.lock().unwrap() = Some(params);
            if let Some(error) = self.error {
                anyhow::bail!("{error}");
            }
            if let Some(response) = &self.response {
                return Ok(response.clone());
            }
            Ok(pane_context_response())
        }

        fn is_available(&self) -> bool {
            true
        }
    }

    #[tokio::test]
    async fn consolidated_context_uses_one_request_with_explicit_source() {
        let channel = Arc::new(RecordingPaneContextChannel {
            requests: AtomicUsize::new(0),
            params: Mutex::new(None),
            error: None,
            response: None,
        });
        let mgr = ShellManager::new().with_wt_channel(channel.clone());

        let captured = capture_pane_context(&mgr, Some("pane-explicit"), 30, 4000)
            .await
            .expect("consolidated pane context should resolve");

        assert_eq!(captured.pane["session_id"], "pane-explicit");
        assert_eq!(captured.output.as_deref(), Some("command output"));
        assert_eq!(channel.requests.load(Ordering::Relaxed), 1);
        let params = channel.params.lock().unwrap().clone().unwrap();
        assert_eq!(params["session_id"], "pane-explicit");
        assert_eq!(params["max_lines"], 30);
        assert_eq!(params["max_chars"], 4000);
    }

    #[tokio::test]
    async fn metadata_only_context_accepts_empty_content_and_additional_fields() {
        let mut response = pane_context_response();
        response["content"] = serde_json::json!("");
        response["output_source"] = serde_json::json!("metadata_only");
        response["line_count"] = serde_json::json!(0);
        response["has_marks"] = serde_json::json!(false);
        response["extension"] = serde_json::json!({"future": true});
        let channel = Arc::new(RecordingPaneContextChannel {
            requests: AtomicUsize::new(0),
            params: Mutex::new(None),
            error: None,
            response: Some(response),
        });
        let mgr = ShellManager::new().with_wt_channel(channel.clone());
        let captured = capture_pane_context(&mgr, None, 0, 4000)
            .await
            .expect("complete metadata-only response must remain valid");
        assert_eq!(captured.pane["session_id"], "pane-explicit");
        assert!(captured.output.is_none());
        assert_eq!(channel.requests.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn planner_and_autofix_resolve_context_with_one_request() {
        for is_autofix in [false, true] {
            for explicit_source in [None, Some("pane-explicit")] {
                let channel = Arc::new(RecordingPaneContextChannel {
                    requests: AtomicUsize::new(0),
                    params: Mutex::new(None),
                    error: None,
                    response: None,
                });
                let mgr = ShellManager::new().with_wt_channel(channel.clone());
                let pane_context = PaneContext {
                    source_pane_id: explicit_source.map(str::to_string),
                    ..Default::default()
                };

                let resolved =
                    resolve_provider_context(is_autofix, true, &mgr, Some(&pane_context)).await;

                assert_eq!(channel.requests.load(Ordering::Relaxed), 1);
                let params = channel.params.lock().unwrap().clone().unwrap();
                assert_eq!(
                    params.get("session_id").and_then(|id| id.as_str()),
                    explicit_source
                );
                assert_eq!(params["max_lines"], if is_autofix { 30 } else { 24 });
                assert_eq!(params["max_chars"], 4000);
                if is_autofix {
                    assert_eq!(
                        resolved.context_pane.unwrap()["session_id"],
                        "pane-explicit"
                    );
                    assert_eq!(resolved.terminal_output.as_deref(), Some("command output"));
                    assert_eq!(
                        resolved.resolved_fix_pane.as_deref(),
                        explicit_source.is_none().then_some("pane-explicit")
                    );
                } else {
                    assert_eq!(
                        resolved.resolved_planner_pane.as_deref(),
                        Some("pane-explicit")
                    );
                    let context: serde_json::Value =
                        serde_json::from_str(&resolved.planner_terminal_context.unwrap()).unwrap();
                    assert_eq!(context["activeTarget"], "pane-explicit");
                    assert_eq!(context["buffer"], "command output");
                }
            }
        }
    }

    #[tokio::test]
    async fn pane_context_failure_does_not_retry_against_another_pane() {
        for is_autofix in [false, true] {
            for explicit_source in [None, Some("pane-missing")] {
                let channel = Arc::new(RecordingPaneContextChannel {
                    requests: AtomicUsize::new(0),
                    params: Mutex::new(None),
                    error: Some("GetPaneContext failed: 0x80070490"),
                    response: None,
                });
                let mgr = ShellManager::new().with_wt_channel(channel.clone());
                let pane_context = PaneContext {
                    source_pane_id: explicit_source.map(str::to_string),
                    ..Default::default()
                };

                let resolved =
                    resolve_provider_context(is_autofix, true, &mgr, Some(&pane_context)).await;

                assert_eq!(channel.requests.load(Ordering::Relaxed), 1);
                assert!(resolved.context_pane.is_none());
                assert!(resolved.terminal_output.is_none());
                assert!(resolved.resolved_fix_pane.is_none());
                assert!(resolved.planner_terminal_context.is_none());
                assert!(resolved.resolved_planner_pane.is_none());
            }
        }
    }

    #[tokio::test]
    async fn malformed_pane_context_logs_contract_error_without_fallback_or_content() {
        use tracing::instrument::WithSubscriber;

        struct SharedWriter(Arc<Mutex<Vec<u8>>>);

        impl std::io::Write for SharedWriter {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let mut cases = vec![
            (serde_json::json!(null), "response must be an object"),
            (serde_json::json!([]), "response must be an object"),
            (serde_json::json!({}), "missing required pane"),
            (serde_json::json!({"pane": null}), "pane must be an object"),
            (serde_json::json!({"pane": []}), "pane must be an object"),
            (
                serde_json::json!({"pane": {}}),
                "pane.session_id must be a nonempty string",
            ),
        ];
        for id in [
            serde_json::json!(""),
            serde_json::json!(" "),
            serde_json::json!({}),
            serde_json::json!(null),
            serde_json::json!(0),
            serde_json::json!(42),
            serde_json::json!(1.5),
            serde_json::json!(true),
            serde_json::json!(false),
            serde_json::json!([]),
        ] {
            cases.push((
                serde_json::json!({"pane": {"session_id": id}}),
                "pane.session_id must be a nonempty string",
            ));
        }
        cases.push((
            serde_json::json!({"pane": {"session_id": "pane-explicit", "is_agent_pane": "false"}}),
            "pane.is_agent_pane must be a boolean",
        ));
        for (field, error) in [
            ("content", "content must be a string"),
            ("truncated", "truncated must be a boolean"),
            ("has_marks", "has_marks must be a boolean"),
            ("output_source", "output_source must be a string"),
            ("fallback_reason", "fallback_reason must be a string"),
            ("line_count", "line_count must be a nonnegative integer"),
        ] {
            let mut response = pane_context_response();
            response.as_object_mut().unwrap().remove(field);
            cases.push((response, error));
        }
        let mut response = pane_context_response();
        response["pane"]
            .as_object_mut()
            .unwrap()
            .remove("is_agent_pane");
        cases.push((response, "pane.is_agent_pane must be a boolean"));
        for (field, invalid, error) in [
            ("content", serde_json::json!({}), "content must be a string"),
            (
                "content",
                serde_json::Value::Null,
                "content must be a string",
            ),
            (
                "truncated",
                serde_json::json!("false"),
                "truncated must be a boolean",
            ),
            (
                "has_marks",
                serde_json::json!(1),
                "has_marks must be a boolean",
            ),
            (
                "output_source",
                serde_json::json!([]),
                "output_source must be a string",
            ),
            (
                "fallback_reason",
                serde_json::json!(false),
                "fallback_reason must be a string",
            ),
            (
                "line_count",
                serde_json::json!(-1),
                "line_count must be a nonnegative integer",
            ),
        ] {
            let mut value = pane_context_response();
            value[field] = invalid;
            cases.push((value, error));
        }

        for (mut response, error) in cases {
            if response.is_object() {
                response["private_terminal_content"] =
                    serde_json::json!("DO_NOT_LOG_TERMINAL_CONTENT");
            }
            for explicit_source in [None, Some("pane-explicit")] {
                let channel = Arc::new(RecordingPaneContextChannel {
                    requests: AtomicUsize::new(0),
                    params: Mutex::new(None),
                    error: None,
                    response: Some(response.clone()),
                });
                let mgr = ShellManager::new().with_wt_channel(channel.clone());
                let logs = Arc::new(Mutex::new(Vec::new()));
                let writer = logs.clone();
                let subscriber = tracing_subscriber::fmt()
                    .without_time()
                    .with_ansi(false)
                    .with_max_level(tracing::Level::DEBUG)
                    .with_writer(move || SharedWriter(writer.clone()))
                    .finish();
                let result = capture_pane_context(&mgr, explicit_source, 30, 4000)
                    .with_subscriber(subscriber)
                    .await;
                assert!(result.is_none(), "{response:?}");
                assert_eq!(channel.requests.load(Ordering::Relaxed), 1);
                let log = String::from_utf8(logs.lock().unwrap().clone()).unwrap();
                assert!(
                    log.contains("pane_context_response_contract_error"),
                    "{log}"
                );
                assert!(log.contains(error), "{log}");
                assert!(!log.contains("pane_context_legacy_fallback"), "{log}");
                assert!(!log.contains("pane_context_request_complete"), "{log}");
                assert!(!log.contains("DO_NOT_LOG_TERMINAL_CONTENT"), "{log}");
            }
        }
    }

    struct LegacyPaneContextChannel {
        methods: Mutex<Vec<String>>,
        pane: serde_json::Value,
    }

    #[async_trait::async_trait]
    impl crate::shell::wt_channel::WtChannel for LegacyPaneContextChannel {
        async fn request(
            &self,
            method: &str,
            params: serde_json::Value,
        ) -> anyhow::Result<serde_json::Value> {
            self.methods.lock().unwrap().push(method.to_string());
            match method {
                "get_pane_context" => Err(anyhow::anyhow!(
                    "wtcli failed: WT_PROTOCOL_UNSUPPORTED_PANE_CONTEXT"
                )),
                "list_windows" => Ok(serde_json::json!({
                    "windows": [{ "window_id": 1 }]
                })),
                "list_tabs" => Ok(serde_json::json!({
                    "tabs": [{ "tab_id": 2 }]
                })),
                "get_active_pane" => Ok(self.pane.clone()),
                "list_panes" => Ok(serde_json::json!({ "panes": [self.pane] })),
                "read_pane_output" => {
                    assert_eq!(
                        params["session_id"].as_str(),
                        json_str_or_num(self.pane.get("session_id")).as_deref()
                    );
                    Ok(serde_json::json!({
                        "content": "legacy output",
                        "has_marks": true,
                    }))
                }
                other => Err(anyhow::anyhow!("unexpected legacy method {other}")),
            }
        }

        fn is_available(&self) -> bool {
            true
        }
    }

    #[tokio::test]
    async fn unsupported_server_uses_observable_legacy_path() {
        let channel = Arc::new(LegacyPaneContextChannel {
            methods: Mutex::new(Vec::new()),
            pane: serde_json::json!({
                "session_id": "pane-legacy",
                "is_agent_pane": false,
            }),
        });
        let mgr = ShellManager::new().with_wt_channel(channel.clone());

        let captured = capture_pane_context(&mgr, Some("pane-legacy"), 30, 4000)
            .await
            .expect("legacy pane context should resolve");

        assert_eq!(captured.pane["session_id"], "pane-legacy");
        assert_eq!(captured.output.as_deref(), Some("legacy output"));
        assert_eq!(
            *channel.methods.lock().unwrap(),
            vec![
                "get_pane_context".to_string(),
                "list_windows".to_string(),
                "list_tabs".to_string(),
                "list_panes".to_string(),
                "read_pane_output".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn legacy_capture_preserves_numeric_ids_and_skips_agent_output() {
        for is_agent in [false, true] {
            for source in [None, Some("42")] {
                let channel = Arc::new(LegacyPaneContextChannel {
                    methods: Mutex::new(Vec::new()),
                    pane: serde_json::json!({
                        "session_id": 42,
                        "is_agent_pane": is_agent,
                    }),
                });
                let mgr = ShellManager::new().with_wt_channel(channel.clone());
                let captured = capture_pane_context(&mgr, source, 30, 4000).await;
                if is_agent {
                    assert!(captured.is_none());
                } else {
                    let captured = captured.expect("legacy numeric pane IDs remain supported");
                    assert_eq!(captured.pane["session_id"], 42);
                    assert_eq!(captured.output.as_deref(), Some("legacy output"));
                }
                assert_eq!(
                    channel
                        .methods
                        .lock()
                        .unwrap()
                        .iter()
                        .any(|method| method == "read_pane_output"),
                    !is_agent,
                );
            }
        }
    }

    #[tokio::test]
    async fn consolidated_capture_skips_agent_panes_before_providers() {
        let mut response = pane_context_response();
        response["pane"]["is_agent_pane"] = serde_json::json!(true);
        let channel = Arc::new(RecordingPaneContextChannel {
            requests: AtomicUsize::new(0),
            params: Mutex::new(None),
            error: None,
            response: Some(response),
        });
        let mgr = ShellManager::new().with_wt_channel(channel.clone());
        for source in [None, Some("pane-explicit")] {
            assert!(capture_pane_context(&mgr, source, 30, 4000).await.is_none());
        }
        assert_eq!(channel.requests.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn build_terminal_context_none_without_wt_channel() {
        let mgr = ShellManager::new();
        assert!(build_terminal_context(&mgr, None).await.is_none());
    }

    #[tokio::test]
    async fn build_terminal_context_skips_agent_pane() {
        let mgr = shell_mgr_with_pane(serde_json::json!({
            "session_id": "p1",
            "is_agent_pane": true,
        }));
        assert!(
            build_terminal_context(&mgr, None).await.is_none(),
            "an active agent pane has no terminal output to ship"
        );
    }

    #[tokio::test]
    async fn build_terminal_context_assembles_fields_for_real_pane() {
        let mgr = shell_mgr_with_pane(serde_json::json!({
            "session_id": "pane-9",
            "title": "My Tab",
            "cwd": "C:\\workspace",
            "pid": std::process::id(),
            "is_agent_pane": false,
        }));
        let context = build_terminal_context(&mgr, None)
            .await
            .expect("a non-agent active pane must yield context json");
        let v: serde_json::Value = serde_json::from_str(&context.json).unwrap();
        assert_eq!(v["activeTarget"], "pane-9");
        assert_eq!(context.target_pane_id, "pane-9");
        assert_eq!(v["window_title"], "My Tab");
        assert_eq!(v["cwd"], "C:\\workspace");
        // The mock returns metadata-only context, so `buffer` is null.
        assert!(v["buffer"].is_null());
        // pid is our own test process → shell resolves to the test binary exe.
        if cfg!(windows) {
            assert!(
                v["shell"]
                    .as_str()
                    .unwrap_or_default()
                    .to_ascii_lowercase()
                    .ends_with(".exe"),
                "shell should resolve from pid; got {:?}",
                v["shell"]
            );
        }
    }

    #[test]
    fn truncate_for_prompt_appends_marker_only_when_over_budget() {
        assert_eq!(truncate_for_prompt("hello", 10), "hello");
        assert_eq!(truncate_for_prompt("hello", 5), "hello");
        assert_eq!(truncate_for_prompt("hello", 3), "hel\n...<truncated>");
    }

    #[test]
    fn protocol_truncation_remains_visible_at_the_prompt_boundary() {
        assert_eq!(
            preserve_protocol_truncation("bounded output", 4000, true),
            "bounded output\n...<truncated>"
        );
        assert_eq!(
            preserve_protocol_truncation("complete output", 4000, false),
            "complete output"
        );
    }

    #[test]
    fn truncate_for_prompt_is_char_safe() {
        let s: String = std::iter::repeat('é').take(10).collect();
        // 5-char budget must cut on a char boundary, no panic.
        let out = truncate_for_prompt(&s, 5);
        assert!(out.starts_with("ééééé"));
        assert!(out.ends_with("...<truncated>"));
    }

    #[test]
    fn json_str_or_num_accepts_strings_and_numbers_only() {
        use serde_json::json;
        let s = json!("hello");
        let n = json!(42);
        let f = json!(1.5);
        let b = json!(true);
        let null = json!(null);
        let arr = json!([1, 2]);
        assert_eq!(json_str_or_num(Some(&s)).as_deref(), Some("hello"));
        assert_eq!(json_str_or_num(Some(&n)).as_deref(), Some("42"));
        assert_eq!(json_str_or_num(Some(&f)).as_deref(), Some("1.5"));
        assert_eq!(json_str_or_num(Some(&b)), None);
        assert_eq!(json_str_or_num(Some(&null)), None);
        assert_eq!(json_str_or_num(Some(&arr)), None);
        assert_eq!(json_str_or_num(None), None);
    }

    fn req_planner(_mgr: &ShellManager, wt_connected: bool) -> ContextRequest<'_> {
        ContextRequest {
            is_autofix: false,
            wt_connected,
            context_pane: None,
            shell_exe: None,
            terminal_output: None,
            planner_terminal_context: None,
            command_resolver_invocation: None,
        }
    }

    #[test]
    fn render_prefixes_heading_marker() {
        let section = ContextSection {
            heading: "Terminal Output",
            body: "body text".to_string(),
        };
        assert_eq!(section.render(), "### Terminal Output\nbody text");
    }

    #[test]
    fn delegate_agents_applies_only_to_planner() {
        let mgr = ShellManager::new();
        assert!(DelegateAgentsProvider.applies(&req_planner(&mgr, true)));
        let autofix = ContextRequest {
            is_autofix: true,
            ..req_planner(&mgr, true)
        };
        assert!(!DelegateAgentsProvider.applies(&autofix));
    }

    #[test]
    fn command_resolver_applies_to_supported_shells_in_both_turn_kinds() {
        let mgr = ShellManager::new();
        let pane = serde_json::json!({ "session_id": "pane-1" });
        for is_autofix in [false, true] {
            for shell in ["pwsh", "powershell.exe", "cmd.exe"] {
                let invocation = command_resolver_invocation(Some(shell), Some(&pane));
                let req = ContextRequest {
                    is_autofix,
                    command_resolver_invocation: invocation.as_ref(),
                    ..req_planner(&mgr, true)
                };
                assert!(CommandResolverProvider.applies(&req), "shell={shell}");
            }
            let invocation = command_resolver_invocation(Some("wsl:Ubuntu"), Some(&pane));
            let req = ContextRequest {
                is_autofix,
                command_resolver_invocation: invocation.as_ref(),
                ..req_planner(&mgr, true)
            };
            assert!(!CommandResolverProvider.applies(&req));
        }

        let unknown_invocation = command_resolver_invocation(None, None);
        let unknown = ContextRequest {
            command_resolver_invocation: unknown_invocation.as_ref(),
            ..req_planner(&mgr, false)
        };
        assert!(CommandResolverProvider.applies(&unknown));
    }

    #[test]
    fn command_resolver_uses_short_wta_execution_alias() {
        let invocation = command_resolver_invocation(Some("cmd.exe"), None).unwrap();
        let contract = serde_json::to_value(invocation.contract("git")).unwrap();

        assert_eq!(contract["executable"], "wta.exe");
        assert_eq!(invocation.shell(), "cmd.exe");
        assert!(invocation.cwd().is_none());
        assert_eq!(
            contract["arguments"],
            serde_json::json!(["resolve-command", "git", "--shell", "cmd.exe", "--json"])
        );
        assert_eq!(
            contract["powershell"],
            "& 'wta.exe' resolve-command 'git' --shell 'cmd.exe' --json"
        );
    }

    #[test]
    fn command_resolver_binds_active_pane_working_directory() {
        let pane = serde_json::json!({ "cwd": "C:\\workspace" });
        let invocation = command_resolver_invocation(Some("pwsh.exe"), Some(&pane)).unwrap();
        let contract = serde_json::to_value(invocation.contract("deploy-it")).unwrap();

        assert_eq!(invocation.cwd(), Some("C:\\workspace"));
        assert_eq!(
            contract["arguments"],
            serde_json::json!([
                "resolve-command",
                "deploy-it",
                "--shell",
                "pwsh.exe",
                "--cwd",
                "C:\\workspace",
                "--json"
            ])
        );
        assert_eq!(
            contract["powershell"],
            "& 'wta.exe' resolve-command 'deploy-it' --shell 'pwsh.exe' \
             --cwd 'C:\\workspace' --json"
        );
    }

    #[test]
    fn terminal_context_requires_planner_and_wt_connection() {
        let mgr = ShellManager::new();
        let connected = ContextRequest {
            planner_terminal_context: Some("{}"),
            ..req_planner(&mgr, true)
        };
        assert!(TerminalContextProvider.applies(&connected));
        assert!(!TerminalContextProvider.applies(&req_planner(&mgr, false)));
    }

    #[test]
    fn shell_context_requires_autofix_with_context_pane() {
        let mgr = ShellManager::new();
        let pane = serde_json::json!({ "cwd": "C:\\proj" });
        let with_pane = ContextRequest {
            is_autofix: true,
            context_pane: Some(&pane),
            ..req_planner(&mgr, true)
        };
        assert!(ShellContextProvider.applies(&with_pane));
        // Planner turn never ships the autofix shell header.
        let planner = ContextRequest {
            context_pane: Some(&pane),
            ..req_planner(&mgr, true)
        };
        assert!(!ShellContextProvider.applies(&planner));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn autofix_providers_return_immediately_without_command_lookup() {
        use futures::FutureExt;

        let probes = crate::command_recall::probe_observer::ProbeObserver::start();
        let mgr = ShellManager::new();
        let pane = serde_json::json!({ "cwd": "C:\\failing-pane" });
        let invocation = command_resolver_invocation(Some("pwsh.exe"), Some(&pane)).unwrap();
        for output in [
            "wta-missing-command-844\nThe term is not recognized",
            "Get-Item missing.txt\nPath not found",
            "gci missing.txt\nPath not found",
            "MyProfileFunction\nCustom failure",
        ] {
            let req = ContextRequest {
                is_autofix: true,
                context_pane: Some(&pane),
                shell_exe: Some("pwsh.exe"),
                terminal_output: Some(output),
                command_resolver_invocation: Some(&invocation),
                ..req_planner(&mgr, true)
            };
            for _ in 0..2 {
                let mut sections = Vec::new();
                for provider in default_providers() {
                    if provider.applies(&req) {
                        if let Some(section) =
                            provider.provide(&req).now_or_never().unwrap_or_else(|| {
                                panic!("{} waited for asynchronous work", provider.id())
                            })
                        {
                            sections.push(section);
                        }
                    }
                }
                assert_eq!(
                    sections
                        .iter()
                        .map(|section| section.heading)
                        .collect::<Vec<_>>(),
                    [
                        "Command Resolver Invocation",
                        "Shell Context",
                        "Terminal Output"
                    ]
                );
                assert!(sections[0].body.contains("not routinely on every failure"));
                assert!(sections[0]
                    .body
                    .contains("without querying merely to verify it"));
                assert!(sections[0]
                    .body
                    .contains("unfamiliar local command or genuine ambiguity"));
                assert!(sections[0].body.contains("indeterminate"));
                assert!(sections[0].body.contains("unsupported"));
                assert!(sections[0].body.contains(r"C:\\failing-pane"));
                assert_eq!(sections[2].body, format!("```\n{output}\n```"));
                assert!(
                    probes.attempts().is_empty(),
                    "complete Autofix provider processing must not attempt command queries"
                );
            }
        }
    }
}
