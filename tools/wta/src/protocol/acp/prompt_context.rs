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

use super::client::{build_terminal_context_json, user_locale_tag};
use crate::coordinator::default_supported_delegate_agents;
use crate::shell::ShellManager;

/// Read-only inputs a [`ContextProvider`] may consult when deciding whether it
/// applies and what section to emit.
///
/// `build_prompt_text` resolves the expensive shared bits (the active pane, its
/// canonical shell, the failing pane's last output) **once** and lends them
/// here, so providers never re-query WT. Autofix-only fields are `None` for
/// planner turns and vice-versa; providers gate on them in
/// [`applies`](ContextProvider::applies).
pub(crate) struct ContextRequest<'a> {
    /// True for an auto-fix / `/fix` turn; false for a planner turn.
    pub is_autofix: bool,
    /// Whether the WT protocol channel is live (pane queries are meaningful).
    pub wt_connected: bool,
    /// Shell manager for providers that query WT directly (planner terminal
    /// context).
    pub shell_mgr: &'a ShellManager,
    /// Autofix only: the JSON of the pane whose shell/cwd describe the failing
    /// command (the source pane — for error-triggered autofix this can be a
    /// pane in a non-focused tab, not the active pane). `None` when WT is not
    /// connected / no pane resolved.
    pub context_pane: Option<&'a serde_json::Value>,
    /// Autofix only: the canonical shell exe of the failing pane
    /// (`pwsh.exe` / `cmd.exe` / `wsl.exe` …), from its pid.
    pub shell_exe: Option<&'a str>,
    /// Autofix only: the failing pane's last `[command + output]` buffer.
    pub terminal_output: Option<&'a str>,
}

/// One `### {heading}\n{body}` block to inject into the prompt. `heading` is
/// fixed per provider; `body` is the provider's already-formatted content
/// (including any code fences). The leading `### ` and the heading/body
/// newline are added by [`ContextSection::render`], so every provider produces
/// a uniformly-shaped section.
pub(crate) struct ContextSection {
    pub heading: &'static str,
    pub body: String,
}

impl ContextSection {
    /// Render to the exact `### {heading}\n{body}` text appended to the prompt.
    pub fn render(&self) -> String {
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
pub(crate) trait ContextProvider: Send + Sync {
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
pub(crate) fn default_providers() -> &'static [&'static dyn ContextProvider] {
    &[
        // Planner turns.
        &DelegateAgentsProvider,
        &TerminalContextProvider,
        // Autofix turns.
        &ShellContextProvider,
        &TerminalOutputProvider,
        &CommandNotFoundProvider,
    ]
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
        !req.is_autofix && req.wt_connected
    }

    async fn provide(&self, req: &ContextRequest<'_>) -> Option<ContextSection> {
        let json = build_terminal_context_json(req.shell_mgr).await?;
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

    fn req_planner(mgr: &ShellManager, wt_connected: bool) -> ContextRequest<'_> {
        ContextRequest {
            is_autofix: false,
            wt_connected,
            shell_mgr: mgr,
            context_pane: None,
            shell_exe: None,
            terminal_output: None,
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
    fn terminal_context_requires_planner_and_wt_connection() {
        let mgr = ShellManager::new();
        assert!(TerminalContextProvider.applies(&req_planner(&mgr, true)));
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
