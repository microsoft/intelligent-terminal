//! Pluggable prompt-context injection for ACP planner / autofix prompts.
//!
//! Prompts shipped to the agent CLI carry a set of `### …` runtime context
//! sections (delegate agents, terminal layout, shell info, the failing
//! command's output, did-you-mean near-matches, …). These used to be
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
//! The command-not-found "did you mean" feature (issue #287) is one such
//! provider, [`CommandNotFoundProvider`]; it is the *local context injection*
//! implementation of this abstraction, not a special case bolted into the
//! assembler.

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
async fn read_pane_last_message(
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

struct PlannerTerminalContext {
    json: String,
    target_pane_id: String,
    resolver_invocation: Option<crate::agent_tools::command_resolution::CommandResolverInvocation>,
}

async fn build_terminal_context(
    shell_mgr: &ShellManager,
    pane_context: Option<&PaneContext>,
) -> Option<PlannerTerminalContext> {
    // WT's GetActivePane already resolves the agent pane to the user's working
    // pane (the "source"), so a single active-pane query gives us the right
    // target. Pane IDs are process-globally unique, so we only need the pane
    // id itself — tab/window aren't needed for addressing.
    let active = match pane_context.and_then(|context| context.source_pane_id.as_deref()) {
        Some(source) => resolve_pane_by_session_id(shell_mgr, source).await?,
        None => shell_mgr.wt_get_active_pane().await.ok()?,
    };

    let is_agent = active
        .get("is_agent_pane")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if is_agent {
        return None;
    }

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
    let resolver_invocation =
        command_resolver_invocation(false, target_shell.as_deref(), Some(&active));

    tracing::debug!(
        target: "acp.terminal_context",
        target_pane_id = %target_pane_id,
        shell = ?target_shell,
        "terminal_context_target_resolved"
    );

    let buffer = read_pane_last_message(
        shell_mgr,
        &target_pane_id,
        24,
        ACTIVE_PANE_CONTEXT_MAX_CHARS,
    )
    .await;

    let json = serde_json::to_string(&serde_json::json!({
        "activeTarget": target_pane_id,
        "window_title": target_window_title,
        "cwd": target_cwd,
        "shell": target_shell,
        "locale": user_locale_tag(),
        "buffer": buffer,
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
        command_resolver_invocation: command_resolver_invocation(is_autofix, None, None),
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

    let active = shell_mgr.wt_get_active_pane().await.ok();

    // Explicit source pane (error-triggered autofix) wins; otherwise fall
    // back to the resolved active working pane (`/fix`). An active pane that
    // is itself an agent pane is skipped — there's no terminal output there.
    let explicit_source = pane_context.and_then(|ctx| ctx.source_pane_id.clone());
    let source_pane_id = explicit_source.clone().or_else(|| {
        active.as_ref().and_then(|a| {
            let is_agent = a
                .get("is_agent_pane")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if is_agent {
                None
            } else {
                json_str_or_num(a.get("session_id"))
            }
        })
    });
    // When we resolved the pane ourselves (manual `/fix`, no explicit
    // source), remember it so the App can fill `target_pane_id` — that is
    // the pane the eventual fix command is sent to.
    if explicit_source.is_none() {
        resolved.resolved_fix_pane = source_pane_id.clone();
    }

    // The pane whose shell/cwd describe the FAILING command — drives the
    // `### Shell Context` header and the command-not-found near-match gate.
    // For a manual `/fix` the active pane IS the source. But error-triggered
    // autofix can fire for a pane in a *non-focused* tab, so deriving the
    // shell from `wt_get_active_pane()` would describe the wrong pane (e.g.
    // a failing pwsh pane while bash is active) and mis-gate the near-match.
    // Resolve the explicit source pane's JSON by *session id* (not by
    // `PaneContext.tab_id`, which in autofix is a StableId `list_panes`
    // won't accept — see `resolve_pane_by_session_id`). If that lookup fails,
    // omit shell context rather than borrowing an unrelated active pane.
    resolved.context_pane = match explicit_source.as_deref() {
        Some(src) => resolve_pane_by_session_id(shell_mgr, src).await,
        None => active,
    };
    // Canonical shell exe (pwsh.exe / cmd.exe / wsl.exe …) of the failing
    // pane — load-bearing for both the shell-context header and the
    // command-not-found near-match gate.
    resolved.shell_exe = resolved.context_pane.as_ref().and_then(shell_from_active);

    if let Some(source_pane_id) = source_pane_id {
        tracing::debug!(
            target: "acp.terminal_context",
            source_pane_id = %source_pane_id,
            shell = ?resolved.shell_exe,
            mode = "autofix",
            "terminal_context_target_resolved"
        );
        resolved.terminal_output = read_pane_last_message(
            shell_mgr,
            &source_pane_id,
            30,
            ACTIVE_PANE_CONTEXT_MAX_CHARS,
        )
        .await;
    }

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
    /// Shell manager for providers that query WT directly (planner terminal
    /// context).
    pub(super) shell_mgr: &'a ShellManager,
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
    /// Planner only: resolver contract derived from the same authoritative pane.
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
/// has nothing to add this turn (e.g. the failing command actually exists).
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
        // Planner turns.
        &CommandResolverProvider,
        &DelegateAgentsProvider,
        &TerminalContextProvider,
        // Autofix turns.
        &ShellContextProvider,
        &TerminalOutputProvider,
        &CommandNotFoundProvider,
    ]
}

/// Planner: a deterministic invocation of this WTA installation's local
/// command resolver through the package execution alias injected into the
/// agent CLI's PATH.
struct CommandResolverProvider;

pub(super) fn command_resolver_invocation(
    is_autofix: bool,
    planner_shell: Option<&str>,
    planner_pane: Option<&serde_json::Value>,
) -> Option<crate::agent_tools::command_resolution::CommandResolverInvocation> {
    if is_autofix
        || planner_shell.is_some_and(|shell| {
            !crate::agent_tools::command_resolution::has_applicable_source(shell)
        })
    {
        return None;
    }

    let executable = "wta.exe".to_string();
    let cwd = planner_pane
        .and_then(|pane| pane.get("cwd"))
        .and_then(serde_json::Value::as_str)
        .filter(|cwd| !cwd.is_empty())
        .map(str::to_string);

    let mut shell = planner_shell.unwrap_or("unknown").to_string();
    if crate::command_recall::is_powershell(&shell) && !std::path::Path::new(&shell).is_absolute() {
        if let Some(path) = planner_pane
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
             active pane's working directory. "
        } else {
            ""
        };
        Some(ContextSection {
            heading: "Command Resolver Invocation",
            body: format!(
                "Replace `<name>` with the command name as one argument. Prefer \
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

/// Autofix: local "did you mean" near-matches when the failing command does not
/// resolve on this machine (issue #287). PowerShell-only in v1; the matching
/// logic lives in [`crate::command_recall`], this provider just gates and
/// formats it into a section.
struct CommandNotFoundProvider;

#[async_trait]
impl ContextProvider for CommandNotFoundProvider {
    fn id(&self) -> &'static str {
        "command_not_found"
    }

    fn applies(&self, req: &ContextRequest<'_>) -> bool {
        req.is_autofix
            && req.terminal_output.is_some()
            && req
                .shell_exe
                .is_some_and(crate::command_recall::is_powershell)
    }

    async fn provide(&self, req: &ContextRequest<'_>) -> Option<ContextSection> {
        let shell_exe = req.shell_exe?;
        let content = req.terminal_output?;
        let token = crate::command_recall::extract_command_token(content)?;
        let matches = crate::command_recall::powershell_near_matches(shell_exe, &token).await?;
        tracing::debug!(
            target: "acp.terminal_context",
            token = %token,
            matches = ?matches,
            mode = "autofix",
            "near_matches_resolved"
        );
        Some(ContextSection {
            heading: "Near Matches",
            body: format!(
                "`{}` was not found as a command in this shell. Closest commands \
                 that DO exist on this machine: {}",
                token,
                near_match_list(&matches)
            ),
        })
    }
}

/// Render near-match command names as a comma-separated, back-ticked list.
fn near_match_list(matches: &[String]) -> String {
    matches
        .iter()
        .map(|m| format!("`{m}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::ShellManager;
    use std::sync::Arc;

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
    }

    #[async_trait::async_trait]
    impl crate::shell::wt_channel::WtChannel for MockWtChannel {
        async fn request(
            &self,
            method: &str,
            _params: serde_json::Value,
        ) -> anyhow::Result<serde_json::Value> {
            match method {
                "get_active_pane" => Ok(self.active_pane.clone()),
                other => Err(anyhow::anyhow!("MockWtChannel: unhandled method {other}")),
            }
        }

        fn is_available(&self) -> bool {
            true
        }
    }

    fn shell_mgr_with_pane(active_pane: serde_json::Value) -> ShellManager {
        ShellManager::new().with_wt_channel(Arc::new(MockWtChannel { active_pane }))
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
        // The mock errors the buffer reads, so `buffer` is null.
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

    fn req_planner(mgr: &ShellManager, wt_connected: bool) -> ContextRequest<'_> {
        ContextRequest {
            is_autofix: false,
            wt_connected,
            shell_mgr: mgr,
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
            heading: "Near Matches",
            body: "body text".to_string(),
        };
        assert_eq!(section.render(), "### Near Matches\nbody text");
    }

    #[test]
    fn near_match_list_backticks_and_joins() {
        assert_eq!(
            near_match_list(&["git".to_string(), "gci".to_string()]),
            "`git`, `gci`"
        );
        assert_eq!(near_match_list(&[]), "");
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
    fn command_resolver_applies_to_supported_planner_shells() {
        let mgr = ShellManager::new();
        let pane = serde_json::json!({ "session_id": "pane-1" });
        for shell in ["pwsh", "powershell.exe", "cmd.exe"] {
            let invocation = command_resolver_invocation(false, Some(shell), Some(&pane));
            let req = ContextRequest {
                command_resolver_invocation: invocation.as_ref(),
                ..req_planner(&mgr, true)
            };
            assert!(CommandResolverProvider.applies(&req), "shell={shell}");
        }

        let unknown_invocation = command_resolver_invocation(false, None, None);
        let unknown = ContextRequest {
            command_resolver_invocation: unknown_invocation.as_ref(),
            ..req_planner(&mgr, false)
        };
        assert!(CommandResolverProvider.applies(&unknown));
        let wsl_invocation = command_resolver_invocation(false, Some("wsl:Ubuntu"), Some(&pane));
        let wsl = ContextRequest {
            command_resolver_invocation: wsl_invocation.as_ref(),
            ..req_planner(&mgr, true)
        };
        assert!(!CommandResolverProvider.applies(&wsl));
        let autofix_invocation = command_resolver_invocation(true, Some("pwsh"), Some(&pane));
        let autofix = ContextRequest {
            is_autofix: true,
            command_resolver_invocation: autofix_invocation.as_ref(),
            ..req_planner(&mgr, true)
        };
        assert!(!CommandResolverProvider.applies(&autofix));
    }

    #[test]
    fn command_resolver_uses_short_wta_execution_alias() {
        let invocation = command_resolver_invocation(false, Some("cmd.exe"), None).unwrap();
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
        let invocation = command_resolver_invocation(false, Some("pwsh.exe"), Some(&pane)).unwrap();
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

    #[test]
    fn command_not_found_gates_on_powershell_and_output() {
        let mgr = ShellManager::new();
        let base = ContextRequest {
            is_autofix: true,
            shell_exe: Some("pwsh.exe"),
            terminal_output: Some("gti status\n..."),
            ..req_planner(&mgr, true)
        };
        assert!(CommandNotFoundProvider.applies(&base));

        // Non-PowerShell shell: feature is PowerShell-only in v1.
        let bash = ContextRequest {
            is_autofix: true,
            shell_exe: Some("bash"),
            terminal_output: Some("gti status"),
            ..req_planner(&mgr, true)
        };
        assert!(!CommandNotFoundProvider.applies(&bash));

        // No captured output: nothing to extract a token from.
        let no_output = ContextRequest {
            is_autofix: true,
            shell_exe: Some("pwsh.exe"),
            terminal_output: None,
            ..req_planner(&mgr, true)
        };
        assert!(!CommandNotFoundProvider.applies(&no_output));

        // Planner turn: never runs the autofix-only provider.
        let planner = ContextRequest {
            shell_exe: Some("pwsh.exe"),
            terminal_output: Some("gti status"),
            ..req_planner(&mgr, true)
        };
        assert!(!CommandNotFoundProvider.applies(&planner));
    }
}
