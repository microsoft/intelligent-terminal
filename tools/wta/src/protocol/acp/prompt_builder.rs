use super::prompt;
use super::prompt_context::{self, ContextRequest};
use super::turn_metrics::prompt_timing_log;
use crate::pane_context::PaneContext;
use crate::shell::ShellManager;
use std::collections::HashMap;
use std::sync::Arc;

/// Which prompt template was last shipped on a given ACP session.
/// Used by [`TemplateMemo`] to decide whether the next turn needs to
/// re-include the (~10k char) template body or can ride on the
/// persona already installed in the session's conversation history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TemplateKind {
    Planner,
    Autofix,
}

impl std::fmt::Display for TemplateKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TemplateKind::Planner => f.write_str("planner"),
            TemplateKind::Autofix => f.write_str("autofix"),
        }
    }
}

/// Per-session memo of the last shipped template kind.
///
/// Each ACP session has its own conversation history with the agent.
/// We pay the ~10k-char template body once on the first turn of a
/// session; subsequent turns only carry runtime context + the user
/// request, because the planner persona is already in history. When
/// the kind changes (planner ↔ autofix) we re-ship so the model's
/// most-recent system instructions match the turn's intent.
///
/// Cleanup is driven by the session lifecycle: `forget()` runs
/// whenever a SessionId is dropped (via `/new` or `drop_session_rx`),
/// keeping the map bounded.
#[derive(Clone, Default)]
pub(crate) struct TemplateMemo(Arc<tokio::sync::Mutex<HashMap<String, TemplateKind>>>);

impl TemplateMemo {
    /// Records `kind` as the latest template for this session and
    /// returns whether the caller must ship the template body on this
    /// turn. Autofix always ships (its template *is* the prompt body);
    /// planner ships on the first turn or when the previous turn used
    /// the other kind.
    pub(crate) async fn should_ship(&self, session_id: &str, kind: TemplateKind) -> bool {
        let prev = self.0.lock().await.insert(session_id.to_string(), kind);
        kind == TemplateKind::Autofix || prev != Some(kind)
    }

    /// Drops the memo entry for a session that's going away.
    pub(crate) async fn forget(&self, session_id: &str) {
        self.0.lock().await.remove(session_id);
    }
}

fn format_pane_context_summary(pane_context: Option<&PaneContext>) -> String {
    match pane_context {
        Some(context) => format!(
            "pane_id={:?} tab_id={:?} window_id={:?} source_pane_id={:?} effective_source_pane_id={:?}",
            context.pane_id,
            context.tab_id,
            context.window_id,
            context.source_pane_id,
            context.effective_source_pane_id(),
        ),
        None => "none".to_string(),
    }
}

pub(crate) async fn build_prompt_text(
    prompt_id: u64,
    submitted_at_unix_s: f64,
    user_text: &str,
    is_autofix: bool,
    include_template: bool,
    shell_mgr: &ShellManager,
    wt_connected: bool,
    pane_context: Option<&PaneContext>,
) -> (String, String, String, Option<String>) {
    let total_started = std::time::Instant::now();
    let mut runtime_sections = Vec::new();
    let template_started = std::time::Instant::now();
    let planner_template = if is_autofix {
        prompt::load_autofix_prompt_template()
    } else {
        prompt::load_planner_prompt_template()
    };
    prompt_timing_log(
        prompt_id,
        submitted_at_unix_s,
        "planner_template_ready",
        &format!(
            "name={:?} source={} dt={:.3}s",
            planner_template.display_name,
            planner_template.source_label,
            template_started.elapsed().as_secs_f64()
        ),
    );

    // ── Shared context resolution ───────────────────────────────────────────
    // Autofix turns resolve the failing pane, its canonical shell, and its last
    // output once; the providers below borrow these from the `ContextRequest`.
    // Planner turns need none of it (their providers query the shell manager
    // directly). Resolving here also keeps the authoritative target pane
    // side-output out of the provider chain. The App binds it to the matching
    // turn context before any recommendation can execute.
    let resolved_context =
        prompt_context::resolve_provider_context(is_autofix, wt_connected, shell_mgr, pane_context)
            .await;

    // ── Provider-driven section assembly ────────────────────────────────────
    // Each `### …` context source is a `ContextProvider`; the chain self-gates
    // by turn kind, so adding a source means adding a provider, not editing
    // this loop. The command-not-found "did you mean" injection (issue #287) is
    // one such provider — see `prompt_context`.
    let context_request = ContextRequest {
        is_autofix,
        wt_connected,
        shell_mgr,
        context_pane: resolved_context.context_pane.as_ref(),
        shell_exe: resolved_context.shell_exe.as_deref(),
        terminal_output: resolved_context.terminal_output.as_deref(),
        planner_terminal_context: resolved_context.planner_terminal_context.as_deref(),
    };
    for provider in prompt_context::default_providers() {
        if !provider.applies(&context_request) {
            continue;
        }
        let provider_started = std::time::Instant::now();
        let section = provider.provide(&context_request).await;
        prompt_timing_log(
            prompt_id,
            submitted_at_unix_s,
            "context_provider",
            &format!(
                "id={} present={} dt={:.3}s",
                provider.id(),
                section.is_some(),
                provider_started.elapsed().as_secs_f64()
            ),
        );
        if let Some(section) = section {
            runtime_sections.push(section.render());
        }
    }

    let assemble_started = std::time::Instant::now();
    // First turn of a session (or kind change): ship the full template
    // body. Subsequent same-kind turns drop the template — the agent
    // already has the persona in its conversation history. Autofix
    // turns always carry the template because the template *is* the
    // prompt body (no user_text alongside it).
    let prompt_body = if include_template {
        prompt::merge_runtime_sections(&planner_template.content, &runtime_sections)
    } else {
        runtime_sections
            .iter()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n")
    };
    let prompt = if is_autofix {
        // Autofix prompts historically ignored `user_text` — the template +
        // terminal output was the whole prompt. Now a non-empty `user_text` is
        // appended as a `## User Request`: a manual `/fix <hint>` passes the
        // hint here, and the error-triggered path passes its failure summary.
        if user_text.trim().is_empty() {
            prompt_body
        } else {
            format!("{}\n\n## User Request\n{}", prompt_body, user_text)
        }
    } else if prompt_body.is_empty() {
        format!("## User Request\n{}", user_text)
    } else {
        format!("{}\n\n## User Request\n{}", prompt_body, user_text)
    };
    prompt_timing_log(
        prompt_id,
        submitted_at_unix_s,
        "prompt_assembled",
        &format!(
            "assemble_dt={:.3}s total_context_dt={:.3}s prompt_len={} include_template={}",
            assemble_started.elapsed().as_secs_f64(),
            total_started.elapsed().as_secs_f64(),
            prompt.len(),
            include_template
        ),
    );
    (
        prompt,
        planner_template.source_label,
        planner_template.display_name,
        resolved_context
            .resolved_fix_pane
            .or(resolved_context.resolved_planner_pane),
    )
}

pub(crate) fn acp_log_built_prompt(
    user_text: &str,
    pane_context: Option<&PaneContext>,
    prompt_source: &str,
    prompt_text: &str,
) {
    tracing::debug!(
        target: "acp",
        user_text_len = user_text.len(),
        pane_context = %format_pane_context_summary(pane_context),
        prompt_source,
        "planner_prompt_begin"
    );
    // Full assembled prompt = user text + captured terminal buffer + cwd.
    // Sensitive — trace only.
    acp_trace_content(&format!("planner_prompt_text:\n{}", prompt_text));
    tracing::debug!(target: "acp", "planner_prompt_end");
}

/// Per-turn audit log: one structured info-level line per round.
///
/// Use this to verify rounds 2+ on a session are "clean" — i.e. the
/// prompt body no longer carries the planner template. Look for
/// `include_template=false` paired with a `body_head` that does NOT
/// start with `# Terminal Agent`.
///
/// Snippets are short on purpose (newlines escaped) so each turn fits
/// on one log line and stays greppable.
pub(crate) fn log_turn_trace(
    prompt_id: u64,
    session_id: &str,
    kind: TemplateKind,
    include_template: bool,
    prompt_text: &str,
) {
    const HEAD_LEN: usize = 200;
    const TAIL_LEN: usize = 150;
    let head = snippet(prompt_text, HEAD_LEN, true);
    let tail = if prompt_text.chars().count() > HEAD_LEN + TAIL_LEN {
        snippet(prompt_text, TAIL_LEN, false)
    } else {
        String::new()
    };
    tracing::info!(
        target: "acp.turn_trace",
        turn = prompt_id,
        session = %session_short(session_id),
        kind = %kind,
        include_template,
        prompt_len = prompt_text.len(),
        "turn_sent"
    );
    // The prompt body snippets carry user text / template content — trace only.
    acp_trace_content(&format!(
        "turn {turn} body_head={head:?} body_tail={tail:?}",
        turn = prompt_id
    ));
}

/// Take `max_chars` from either end of `text` and inline newlines as
/// `\n` so the snippet fits on a single log line.
fn snippet(text: &str, max_chars: usize, from_start: bool) -> String {
    let slice: String = if from_start {
        text.chars().take(max_chars).collect()
    } else {
        let mut tail: Vec<char> = text.chars().rev().take(max_chars).collect();
        tail.reverse();
        tail.into_iter().collect()
    };
    slice.replace('\n', "\\n")
}

/// Last 8 chars of a SessionId for compact logging.
fn session_short(session_id: &str) -> String {
    let mut tail: Vec<char> = session_id.chars().rev().take(8).collect();
    tail.reverse();
    tail.into_iter().collect()
}

fn acp_trace_content(msg: &str) {
    tracing::trace!(target: "acp.content", "{}", msg);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_pane_context_summary_none_is_literal_none() {
        assert_eq!(format_pane_context_summary(None), "none");
    }

    /// The summary must surface `effective_source_pane_id`, which drives autofix
    /// routing: it prefers `source_pane_id` (the pane that produced the failing
    /// command) and only falls back to `pane_id` (the agent pane) when absent.
    #[test]
    fn format_pane_context_summary_reflects_effective_source_precedence() {
        let ctx = PaneContext {
            pane_id: Some("agent-pane".to_string()),
            tab_id: Some("tab-1".to_string()),
            window_id: Some("win-1".to_string()),
            cwd: Some("C:\\work".to_string()),
            source_pane_id: Some("src-pane".to_string()),
        };
        let s = format_pane_context_summary(Some(&ctx));
        assert!(s.contains("pane_id=Some(\"agent-pane\")"), "got: {s}");
        assert!(s.contains("source_pane_id=Some(\"src-pane\")"), "got: {s}");
        assert!(
            s.contains("effective_source_pane_id=Some(\"src-pane\")"),
            "effective must prefer source_pane_id; got: {s}"
        );

        let ctx2 = PaneContext {
            pane_id: Some("agent-pane".to_string()),
            source_pane_id: None,
            ..Default::default()
        };
        let s2 = format_pane_context_summary(Some(&ctx2));
        assert!(
            s2.contains("effective_source_pane_id=Some(\"agent-pane\")"),
            "effective must fall back to pane_id; got: {s2}"
        );
    }

    #[test]
    fn template_kind_display_matches_label() {
        assert_eq!(TemplateKind::Planner.to_string(), "planner");
        assert_eq!(TemplateKind::Autofix.to_string(), "autofix");
    }

    /// Minimal [`crate::shell::wt_channel::WtChannel`] that answers
    /// `get_active_pane` with a canned pane and the
    /// `list_windows`/`list_tabs`/`list_panes` enumeration with canned
    /// payloads; every other request errors. `read_pane_last_message` degrades
    /// to `None` on those errors, which is all the assembly tests need (no
    /// buffer content is asserted).
    struct MockWtChannel {
        active_pane: serde_json::Value,
        /// Optional enumeration topology for `resolve_pane_by_session_id`:
        /// `{ "windows": […] }`, `{ "tabs": […] }`, `{ "panes": […] }`.
        windows: Option<serde_json::Value>,
        tabs: Option<serde_json::Value>,
        panes: Option<serde_json::Value>,
    }

    #[async_trait::async_trait]
    impl crate::shell::wt_channel::WtChannel for MockWtChannel {
        async fn request(
            &self,
            method: &str,
            _params: serde_json::Value,
        ) -> anyhow::Result<serde_json::Value> {
            let scripted = |v: &Option<serde_json::Value>, what: &str| {
                v.clone()
                    .ok_or_else(|| anyhow::anyhow!("MockWtChannel: no {what} scripted"))
            };
            match method {
                "get_active_pane" => Ok(self.active_pane.clone()),
                "list_windows" => scripted(&self.windows, "list_windows"),
                "list_tabs" => scripted(&self.tabs, "list_tabs"),
                "list_panes" => scripted(&self.panes, "list_panes"),
                other => Err(anyhow::anyhow!("MockWtChannel: unhandled method {other}")),
            }
        }
        fn is_available(&self) -> bool {
            true
        }
    }

    fn shell_mgr_with_pane(active: serde_json::Value) -> ShellManager {
        ShellManager::new().with_wt_channel(Arc::new(MockWtChannel {
            active_pane: active,
            windows: None,
            tabs: None,
            panes: None,
        }))
    }

    /// Shell manager whose enumeration (`list_windows`→`list_tabs`→`list_panes`)
    /// resolves to a single window/tab containing `source_pane`, so
    /// `resolve_pane_by_session_id` can find the failing pane.
    fn shell_mgr_with_source_pane(
        active: serde_json::Value,
        source_pane: serde_json::Value,
    ) -> ShellManager {
        ShellManager::new().with_wt_channel(Arc::new(MockWtChannel {
            active_pane: active,
            windows: Some(serde_json::json!({ "windows": [{ "window_id": 1 }] })),
            tabs: Some(serde_json::json!({ "tabs": [{ "tab_id": 0 }] })),
            panes: Some(serde_json::json!({ "panes": [source_pane] })),
        }))
    }

    /// A planner turn with `include_template=true` ships the persona template,
    /// the delegate-agents section, and appends the user request.
    #[tokio::test]
    async fn build_prompt_text_planner_includes_template_and_user_request() {
        let mgr = ShellManager::new();
        let expected = prompt::load_planner_prompt_template();
        let (built_prompt, _source, display_name, target_pane) =
            build_prompt_text(1, 0.0, "list files", false, true, &mgr, false, None).await;
        assert_eq!(display_name, expected.display_name);
        assert!(
            built_prompt.contains("### Supported Delegate Agents"),
            "planner must ship the delegate-agents section"
        );
        assert!(
            built_prompt.contains("## User Request\nlist files"),
            "planner must append the user text"
        );
        assert!(target_pane.is_none(), "no WT channel means no target pane");
    }

    #[tokio::test]
    async fn build_prompt_text_planner_returns_the_injected_active_target() {
        let mgr = shell_mgr_with_pane(serde_json::json!({
            "session_id": "real-pane-guid",
            "cwd": "C:\\repo",
            "pid": std::process::id(),
            "is_agent_pane": false,
        }));

        let (built_prompt, _source, _display_name, target_pane) =
            build_prompt_text(8, 0.0, "check port 8000", false, true, &mgr, true, None).await;

        assert!(built_prompt.contains("\"activeTarget\":\"real-pane-guid\""));
        assert_eq!(target_pane.as_deref(), Some("real-pane-guid"));
    }

    #[tokio::test]
    async fn build_prompt_text_planner_uses_submitted_source_after_focus_changes() {
        let active = serde_json::json!({
            "session_id": "newly-focused-pane",
            "cwd": "C:\\other",
            "pid": std::process::id(),
            "is_agent_pane": false,
        });
        let source = serde_json::json!({
            "session_id": "submitted-source-pane",
            "cwd": "C:\\repo",
            "pid": std::process::id(),
            "is_agent_pane": false,
        });
        let mgr = shell_mgr_with_source_pane(active, source);
        let pane_context = PaneContext {
            source_pane_id: Some("submitted-source-pane".to_string()),
            ..Default::default()
        };

        let (built_prompt, _source, _display_name, target_pane) = build_prompt_text(
            9,
            0.0,
            "check port 8000",
            false,
            true,
            &mgr,
            true,
            Some(&pane_context),
        )
        .await;

        assert!(built_prompt.contains("\"activeTarget\":\"submitted-source-pane\""));
        assert_eq!(target_pane.as_deref(), Some("submitted-source-pane"));
    }

    /// An autofix turn loads the *autofix* persona (not the planner), appends a
    /// non-empty hint as a User Request, and omits planner-only sections.
    #[tokio::test]
    async fn build_prompt_text_autofix_appends_hint_and_omits_planner_sections() {
        let mgr = ShellManager::new();
        let planner = prompt::load_planner_prompt_template();
        let autofix = prompt::load_autofix_prompt_template();
        let (built_prompt, _s, display_name, fix_pane) =
            build_prompt_text(2, 0.0, "fix the build", true, true, &mgr, false, None).await;
        assert_eq!(display_name, autofix.display_name);
        assert_ne!(
            display_name, planner.display_name,
            "autofix must not reuse the planner persona"
        );
        assert!(
            !built_prompt.contains("### Supported Delegate Agents"),
            "autofix prompt is not the planner prompt"
        );
        let user_request = format!("## User Request\n{}", "fix the build");
        assert!(
            built_prompt.contains(&user_request),
            "a non-empty autofix hint is appended"
        );
        assert!(fix_pane.is_none(), "no wt channel → nothing to resolve");
    }

    /// A blank autofix hint must not produce an empty `## User Request` section.
    #[tokio::test]
    async fn build_prompt_text_autofix_blank_hint_has_no_user_request() {
        let mgr = ShellManager::new();
        let (built_prompt, _s, _d, _f) =
            build_prompt_text(3, 0.0, "   ", true, true, &mgr, false, None).await;
        assert!(
            !built_prompt.contains("## User Request"),
            "blank autofix hint must not add a User Request section"
        );
    }

    /// With `include_template=false` the (large) persona body is dropped — only
    /// runtime sections and the user request remain. This is the per-session
    /// "template already in history" optimization.
    #[tokio::test]
    async fn build_prompt_text_without_template_drops_persona_body() {
        let mgr = ShellManager::new();
        let planner = prompt::load_planner_prompt_template();
        assert!(
            !planner.content.trim().is_empty(),
            "test precondition: planner template body is non-empty"
        );
        let (built_prompt, _s, _d, _f) =
            build_prompt_text(4, 0.0, "hi", false, false, &mgr, false, None).await;
        assert!(
            !built_prompt.contains(planner.content.trim()),
            "include_template=false must omit the template body"
        );
        let user_request = format!("## User Request\n{}", "hi");
        assert!(built_prompt.contains(&user_request));
    }

    /// A manual `/fix` (autofix, no explicit `source_pane_id`) resolves the
    /// active working pane from WT and reports it as the fix target so the App
    /// can address the eventual fix command.
    #[tokio::test]
    async fn build_prompt_text_autofix_fix_resolves_active_pane() {
        let mgr = shell_mgr_with_pane(serde_json::json!({
            "session_id": "work-pane",
            "cwd": "C:\\proj",
            "pid": std::process::id(),
            "is_agent_pane": false,
        }));
        let (built_prompt, _s, _d, fix_pane) =
            build_prompt_text(5, 0.0, "", true, true, &mgr, true, None).await;
        assert_eq!(
            fix_pane.as_deref(),
            Some("work-pane"),
            "manual /fix must resolve the active working pane"
        );
        assert!(
            built_prompt.contains("### Shell Context"),
            "autofix with a wt channel must ship shell context"
        );
    }

    /// Error-triggered autofix carries its own `source_pane_id`; the explicit
    /// source wins and `resolved_fix_pane` stays `None` (the App already knows
    /// the target).
    #[tokio::test]
    async fn build_prompt_text_autofix_explicit_source_not_reported_as_resolved() {
        let mgr = shell_mgr_with_pane(serde_json::json!({
            "session_id": "work-pane",
            "pid": std::process::id(),
            "is_agent_pane": false,
        }));
        let ctx = PaneContext {
            source_pane_id: Some("explicit-src".to_string()),
            ..Default::default()
        };
        let (_p, _s, _d, fix_pane) =
            build_prompt_text(6, 0.0, "", true, true, &mgr, true, Some(&ctx)).await;
        assert!(
            fix_pane.is_none(),
            "error-triggered autofix carries its source; resolved_fix_pane stays None"
        );
    }

    /// Regression: error-triggered autofix whose failing pane lives in a
    /// **non-focused** tab must describe *that* pane's shell/cwd in
    /// `### Shell Context`, not the currently-active pane's.
    #[tokio::test]
    async fn build_prompt_text_autofix_uses_source_pane_shell_not_active_pane() {
        let active = serde_json::json!({
            "session_id": "active-pane",
            "shell": "bash",
            "cwd": "C:\\activedir",
            "is_agent_pane": false,
        });
        let source_pane = serde_json::json!({
            "session_id": "src-pane",
            "shell": "pwsh.exe",
            "cwd": "C:\\srcdir",
            "is_agent_pane": false,
        });
        let mgr = shell_mgr_with_source_pane(active, source_pane);
        let ctx = PaneContext {
            tab_id: Some("stable-tab-xyz".to_string()),
            source_pane_id: Some("src-pane".to_string()),
            ..Default::default()
        };
        let (built_prompt, _s, _d, _f) =
            build_prompt_text(7, 0.0, "", true, true, &mgr, true, Some(&ctx)).await;
        assert!(
            built_prompt.contains("### Shell Context"),
            "got: {built_prompt}"
        );
        assert!(
            built_prompt.contains("\"shell\":\"pwsh.exe\""),
            "shell context must use the source pane's shell (pwsh); got: {built_prompt}"
        );
        assert!(
            built_prompt.contains("\"cwd\":\"C:\\\\srcdir\""),
            "shell context must use the source pane's cwd (srcdir); got: {built_prompt}"
        );
        assert!(
            !built_prompt.contains("\"shell\":\"bash\"") && !built_prompt.contains("activedir"),
            "the active pane's shell/cwd must NOT leak into shell context; got: {built_prompt}"
        );
    }

    #[test]
    fn snippet_takes_head_or_tail() {
        assert_eq!(snippet("hello world", 5, true), "hello");
        assert_eq!(snippet("hello world", 5, false), "world");
        // Budget larger than the text returns the whole thing either way.
        assert_eq!(snippet("hi", 5, true), "hi");
        assert_eq!(snippet("hi", 5, false), "hi");
        // Newlines are escaped for single-line logging.
        assert_eq!(snippet("a\nb", 5, true), "a\\nb");
    }

    #[test]
    fn session_short_returns_last_eight_chars() {
        assert_eq!(session_short("0123456789abcdef"), "89abcdef");
        // Shorter than 8 → whole string.
        assert_eq!(session_short("abc"), "abc");
    }
}
