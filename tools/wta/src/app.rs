use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::CrosstermBackend;
use ratatui::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

struct DeferredAcpParams {
    agent_cmd: String,
    acp_model: Option<String>,
    prompt_rx: Option<mpsc::UnboundedReceiver<crate::protocol::acp::client::PromptSubmission>>,
    cancel_rx: Option<mpsc::UnboundedReceiver<crate::protocol::acp::client::CancelRequest>>,
    new_session_rx: Option<mpsc::UnboundedReceiver<crate::protocol::acp::client::NewSessionForTab>>,
    load_session_rx:
        Option<mpsc::UnboundedReceiver<crate::protocol::acp::client::LoadSessionForTab>>,
    drop_session_rx:
        Option<mpsc::UnboundedReceiver<crate::protocol::acp::client::DropSessionRequest>>,
    rename_session_rx:
        Option<mpsc::UnboundedReceiver<crate::protocol::acp::client::RenameSessionRequest>>,
    restart_rx: Option<mpsc::UnboundedReceiver<crate::protocol::acp::client::RestartRequest>>,
    master_ext_rx: Option<mpsc::UnboundedReceiver<crate::protocol::acp::client::MasterExtRequest>>,
    shell_mgr: Arc<crate::shell::ShellManager>,
    wt_connected: bool,
    /// Master pipe name for a pipe-mode reconnect. Pre-stashed at boot in
    /// helper mode (main.rs) so that a post-FRE-login reconnect via
    /// [`App::try_start_acp`] goes back through wta-master over
    /// `run_acp_client_over_pipe`. Always `Some` in the shipped product
    /// (wta only runs as a wta-master-attached helper); a `None` here is a
    /// defensive error path since direct-agent mode was removed.
    master_pipe_name: Option<String>,
    /// Owner tab id for pipe-mode reconnect (mirrors the original
    /// `--owner-tab-id` CLI arg).
    owner_tab_id: Option<String>,
}

mod turn_state;
mod autofix;
use autofix::*;

pub use turn_state::{AutofixContext, ChunkKind, SubmittedPrompt, TurnOutcome, TurnState};

// ─── MVP sessions origin filter ────────────────────────────────────────────────────
//
// The session management view (`/sessions`) currently ships in MVP
// mode: it only surfaces shell-pane sessions (the user manually ran
// `copilot` / `claude` / `gemini` in a regular shell). Agent-pane
// sessions (Class A — created by WTA on behalf of an Intelligent
// Terminal agent pane) stay in the registry so Enter routing,
// alive-mirror reconciliation, and `wta sessions list` continue to
// work; they just don't render in the picker.
//
// To bring agent-pane sessions back into the picker once the manage UX
// is ready, flip this constant to `OriginFilter::All` and delete the
// `WTA_SESSIONS_SHOW_AGENT_PANE` env override below. No other call sites
// need to change — all consumers go through
// `App::sessions_origin_filter`.
const MVP_SESSIONS_ORIGIN_FILTER: crate::agent_sessions::OriginFilter =
    crate::agent_sessions::OriginFilter::ShellOnly;

/// Resolve the `/sessions` origin filter for this process.
///
/// Defaults to [`MVP_SESSIONS_ORIGIN_FILTER`]. The `WTA_SESSIONS_SHOW_AGENT_PANE`
/// env var (set to `1` / `true`) flips a single helper to
/// `OriginFilter::All` for debugging — matches the existing
/// `WTA_LOG_AGENT_EVENT` / `WTA_SOURCE_*` convention. Each helper is
/// a separate process so the override only affects the pane that
/// launched with the env var set; the rest of the Terminal keeps the
/// MVP default.
pub fn resolve_sessions_origin_filter() -> crate::agent_sessions::OriginFilter {
    match std::env::var("WTA_SESSIONS_SHOW_AGENT_PANE")
        .ok()
        .as_deref()
        .map(str::trim)
    {
        Some("1") | Some("true") | Some("TRUE") | Some("True") | Some("yes") => {
            crate::agent_sessions::OriginFilter::All
        }
        _ => MVP_SESSIONS_ORIGIN_FILTER,
    }
}

use crate::commands::{self, CommandKind, CommandSpec, ParseOutcome, ParsedCommand};
use crate::coordinator::{
    parse_autofix_response, parse_recommendation_set, recommended_choice_index,
    validate_recommendation_set_for_coordinator_target, AutofixDecision, RecommendationChoice,
    RecommendationSet,
};
use crate::pane_context::PaneContext;

use crate::protocol::acp::client::{
    prompt_timing_log, CancelRequest, DropSessionRequest, LoadSessionForTab, NewSessionForTab,
    PromptSubmission, RenameSessionRequest, RestartRequest,
};
use crate::ui;
use crate::ui_trace;

// --- Debug types ---

#[derive(Debug, Clone)]
pub enum DebugDir {
    Sent,
    Received,
}

#[derive(Debug, Clone)]
pub struct DebugMessage {
    pub timestamp: f64,
    pub direction: DebugDir,
    pub content: String,
}

// --- Application mode ---

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppMode {
    /// Normal agent chat (default).
    Chat,
    /// Setup / getting-started screen.
    Setup,
    /// Auth screen — agent selected but needs sign-in.
    Auth,
}

#[derive(Debug, Clone)]
pub struct AuthState {
    pub agent_id: String,
    pub agent_name: String,
    pub login_command: String,
    pub checking: bool,
    pub status_message: String,
    /// GitHub Enterprise sign-in: true while the domain input is shown/active.
    pub enterprise_mode: bool,
    /// The GitHub Enterprise domain being entered (e.g. "mycompany.ghe.com").
    pub enterprise_host: String,
}

/// Prefill the Copilot GHE sign-in state from the persisted host. Returns
/// `(enterprise_mode, enterprise_host)`: a returning GHE user starts with the
/// domain input expanded and pre-filled so they can sign in with one keypress.
fn copilot_enterprise_prefill(agent_id: &str) -> (bool, String) {
    if agent_id == "copilot" {
        if let Some(host) = crate::agent_check::load_copilot_enterprise_host() {
            return (true, host);
        }
    }
    (false, String::new())
}

/// The device-verification URL for a Copilot device-code login. Data-residency
/// GitHub Enterprise verifies device codes on the enterprise host (taken from
/// the `--host https://<host>` in the login command), not github.com.
fn device_verify_url(login_command: &str) -> String {
    login_command
        .split("--host ")
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .map(|h| h.trim_end_matches('/'))
        .filter(|h| !h.is_empty())
        .map(|h| format!("{}/login/device", h))
        .unwrap_or_else(|| "https://github.com/login/device".to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetupReason {
    AgentMissing,
    AgentError,
}

impl SetupReason {
    pub fn from_str(s: &str) -> Self {
        match s {
            "agent-missing" => Self::AgentMissing,
            "agent-error" => Self::AgentError,
            _ => Self::AgentError,
        }
    }

    pub fn title(&self) -> String {
        match self {
            Self::AgentMissing => t!("setup.title.agent_missing").into_owned(),
            Self::AgentError => t!("setup.title.agent_error").into_owned(),
        }
    }
}

/// A single option in the unified setup list.
#[derive(Debug, Clone)]
pub enum SetupOption {
    /// Preflight: reinstall via winget (automatic)
    Install {
        agent_id: String,
        display_name: String,
    },
    /// Preflight: sign in to fix auth
    SignIn {
        agent_id: String,
        display_name: String,
    },
    /// Preflight: retry connection (custom agent)
    Retry,
}

#[derive(Debug, Clone)]
pub struct SetupState {
    pub reason: SetupReason,
    pub selected_index: usize,
    /// Preflight result populated from `preflight::check_agent`.
    pub preflight: PreflightResult,
    /// True while a `winget install` task is running.
    pub install_in_progress: bool,
    /// Tail of the install command's output (last ~6 lines).
    pub install_log: Vec<String>,
    /// Error message from the most recent install attempt (cleared on retry).
    pub install_error: Option<String>,
    /// Unified options list for the setup screen.
    pub options: Vec<SetupOption>,
    /// Dynamic title for the setup screen.
    pub title: String,
    /// Dynamic subtitle for the setup screen.
    pub subtitle: String,
}

/// Status of a single preflight check.
#[derive(Debug, Clone, PartialEq)]
pub enum CheckStatus {
    Checking,
    Passed,
    Failed(String),
    Skipped,
}

/// Result of all preflight checks for an agent.
#[derive(Debug, Clone)]
pub struct PreflightResult {
    pub agent_id: String,
    pub display_name: String,
    pub cli_status: CheckStatus,
    pub cli_path: Option<String>,
    pub auth_status: CheckStatus,
    pub install_hint: String,
    pub install_url: String,
    pub auth_hint: String,
}

impl PreflightResult {
    pub fn all_passed(&self) -> bool {
        self.cli_status == CheckStatus::Passed
            && matches!(self.auth_status, CheckStatus::Passed | CheckStatus::Skipped)
    }

    /// Synthesize a `Passed` preflight result for a custom or unknown agent
    /// id. We deliberately do **not** run an out-of-band PATH check for these
    /// — the user-supplied command can be anything (`.cmd`, `.ps1`,
    /// `node script.js`, an alias) and any guess we make disagrees with what
    /// the spawner actually does. Real spawn failures surface via the
    /// `ConnectionFailed` → `ConnectionState::Failed` lifecycle, which is the
    /// authoritative error path.
    ///
    /// Returning `cli_status=Passed` keeps the TUI out of Setup mode so the
    /// chat input stays responsive. The display name is derived from the
    /// canonical id (`custom:<name>` → `<name>`) so the UI never collapses
    /// to the generic `DEFAULT_PROFILE` "Agent" label.
    pub fn passed_for_custom_agent(canonical_id: &str) -> Self {
        let display_name = canonical_id
            .strip_prefix("custom:")
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| canonical_id.to_string());
        Self {
            agent_id: canonical_id.to_string(),
            display_name,
            cli_status: CheckStatus::Passed,
            cli_path: None,
            auth_status: CheckStatus::Skipped,
            install_hint: String::new(),
            install_url: String::new(),
            auth_hint: String::new(),
        }
    }
}

/// True for the auth failures a post-login reconnect can hit when the shared
/// master CLI was spawned with a stale token: the plain `AuthRequired`, AND the
/// `HandshakeFailed { stage: NewSession }` that the pipe client wraps a
/// still-`AuthRequired` `new_session` into after a *successful* `authenticate`
/// (the Copilot CLI does not refresh its in-process auth on `authenticate`, so
/// only respawning it recovers — see `run_acp_client_over_pipe`).
///
/// Deliberately does NOT match `HandshakeFailed { stage: Authenticate }`: that
/// is a genuine `authenticate` RPC rejection or timeout (the credentials were
/// not accepted / the agent hung), which a master restart would not fix — it
/// routes to the sign-in screen via the normal `AgentError` path instead.
fn is_post_login_auth_failure(failure: &crate::protocol::acp::failure::AgentFailure) -> bool {
    use crate::protocol::acp::failure::{AgentFailure, HandshakeStage};
    matches!(
        failure,
        AgentFailure::AuthRequired { .. }
            | AgentFailure::HandshakeFailed {
                stage: HandshakeStage::NewSession,
                ..
            }
    )
}

/// Build the diagnostic setup options list based on the configured agent state:
/// install when the CLI is missing and auto-installable, sign in for Copilot
/// auth failures, or retry for external-auth / manually repaired cases.
pub fn build_setup_options(
    reason: &SetupReason,
    current_agent_status: Option<&crate::agent_check::AgentStatus>,
) -> Vec<SetupOption> {
    let mut opts = Vec::new();
    if let Some(status) = current_agent_status {
        if !status.cli_found {
            // CLI not found — offer install options
            if status.can_auto_install() {
                opts.push(SetupOption::Install {
                    agent_id: status.id.clone(),
                    display_name: status.display_name.clone(),
                });
            }
        } else if !status.has_credential || *reason == SetupReason::AgentError {
            // CLI found but auth missing or known to have failed
            if status.id == "copilot" {
                // Copilot: we can drive the device-flow sign-in
                opts.push(SetupOption::SignIn {
                    agent_id: status.id.clone(),
                    display_name: status.display_name.clone(),
                });
            } else {
                // Other agents: user must sign in externally, then retry
                opts.push(SetupOption::Retry);
            }
        }
        // If custom/unknown agent, offer retry
        if status.id == "unknown" || (!status.can_auto_install() && !status.cli_found) {
            opts.push(SetupOption::Retry);
        }
    } else {
        opts.push(SetupOption::Retry);
    }
    opts
}

// --- State types ---

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ConnectionState {
    Disconnected,
    Connecting(String),
    Connected,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ChatMessage {
    User(String),
    Agent(String),
    System(String),
    ToolCall {
        id: String,
        title: String,
        status: String,
    },
    Plan(Vec<PlanEntry>),
    Error(String),
    /// Informational WT event surfaced inline in the chat (e.g. shell exit
    /// codes, OSC sequences). Distinct from `Error` so we can theme it
    /// differently and skip autofix wiring.
    AgentEvent(String),
    /// "Intelligent Terminal uses AI. Check for mistakes" disclaimer.
    /// Pushed on every agent-pane startup,
    /// no persistence gating — getting cleared by the next turn is fine,
    /// the next pane startup re-pushes it.
    Disclaimer,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompletedTurn {
    pub prompt: String,
    #[serde(default)]
    pub details: Vec<ChatMessage>,
    /// Whether the turn's `details` are visible in the UI. Tab to select +
    /// Enter to toggle. Default false (collapsed) so history stays compact.
    #[serde(default)]
    pub expanded: bool,
    /// Trailing inline status marker rendered in DIM next to the turn's
    /// first content line (e.g. "(canceled)" / "→ executed: Run Get-Date").
    /// Set when the user dismisses or executes a recommendation card, or
    /// cancels a mid-stream turn — `None` for normal chat turns.
    #[serde(default)]
    pub trailing_marker: Option<String>,
}

/// Maximum displayed characters for a collapsed turn header preview.
/// Picked so the `▶ > <preview>…` row stays well under a typical 120-col
/// wrap width even after the chevron + prompt prefix; longer prompts get
/// truncated with a trailing ellipsis. The full original text is always
/// preserved in the turn's first `details` entry.
const COLLAPSED_PROMPT_PREVIEW_CHARS: usize = 80;

/// Build the single-line preview shown in a collapsed `CompletedTurn`
/// header. Takes the first non-blank line of the prompt and clips it to
/// `COLLAPSED_PROMPT_PREVIEW_CHARS`. Multi-line prompts (system prompts,
/// pasted blocks, etc.) collapse to one row instead of wrapping over
/// dozens of lines in the chat scrollback.
pub fn collapsed_prompt_preview(text: &str) -> String {
    let first_line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let mut iter = first_line.chars();
    let mut out: String = (&mut iter).take(COLLAPSED_PROMPT_PREVIEW_CHARS).collect();
    // Append ellipsis if the prompt has more content than the preview
    // covered — either the first line itself was longer, or there are
    // additional non-empty lines below.
    let truncated = iter.next().is_some()
        || text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .nth(1)
            .is_some();
    if truncated {
        out.push('…');
    }
    out
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanEntry {
    pub content: String,
    pub status: PlanEntryStatus,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PlanEntryStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PermOption {
    pub id: String,
    pub name: String,
    pub kind: String,
}

impl PermOption {
    /// True if this is an "allow" option. Case-insensitive because `kind`
    /// is the ACP `PermissionOptionKind` rendered via `format!("{:?}", …)`,
    /// which yields PascalCase variants like `AllowOnce` / `AllowAlways`.
    /// Matching the leading `allow` prefix here keeps the `y`/`n` quick-keys
    /// and the `[Y]`/`[N]` button labels in sync with the real wire values.
    /// Prefix-checked (not lowercased) to stay allocation-free on the render /
    /// key-handling hot path.
    pub fn is_allow(&self) -> bool {
        self.kind.get(..5).is_some_and(|p| p.eq_ignore_ascii_case("allow"))
    }

    /// True if this is a "reject" option. Allocation-free, case-insensitive —
    /// see [`PermOption::is_allow`].
    pub fn is_reject(&self) -> bool {
        self.kind.get(..6).is_some_and(|p| p.eq_ignore_ascii_case("reject"))
    }
}

pub struct PermissionState {
    pub description: String,
    pub options: Vec<PermOption>,
    pub selected: usize,
    pub responder: Option<tokio::sync::oneshot::Sender<String>>,
}

impl PermissionState {
    /// Index of the first "allow" option, used by the `y` quick-key and the
    /// `[Y]` button label.
    pub fn allow_index(&self) -> Option<usize> {
        self.options.iter().position(PermOption::is_allow)
    }

    /// Index of the first "reject" option, used by the `n` quick-key and the
    /// `[N]` button label.
    pub fn reject_index(&self) -> Option<usize> {
        self.options.iter().position(PermOption::is_reject)
    }
}

// --- WT Event Notification ---

#[derive(Debug, Clone, PartialEq)]
pub enum WtEventSeverity {
    Critical,
    Actionable,
    Informational,
}

#[derive(Debug, Clone)]
pub struct WtNotification {
    pub severity: WtEventSeverity,
    pub pane_id: String,
    /// WT tab StableId that owns the failing pane. `None` when the
    /// underlying event predates the tab_id wire (older WT builds) or
    /// arrived without a tab context. Autofix routing treats absence as
    /// "cannot route — drop with warn", to avoid the old failure mode
    /// where the fix landed in whatever tab happened to be active.
    pub tab_id: Option<String>,
    pub summary: String,
    pub acknowledged: bool,
    pub age_ticks: u32,
}

impl WtNotification {
    /// Auto-collapse informational notifications after ~5s (42 ticks at 120ms).
    /// Actionable/critical persist until dismissed.
    pub fn should_auto_dismiss(&self) -> bool {
        self.severity == WtEventSeverity::Informational && self.age_ticks > 42
    }
}

/// Open a URL in the user's default browser. Used by Setup mode's
/// "press O to open install URL" key handler.
fn open_url_in_browser(url: &str) -> std::io::Result<()> {
    crate::win32::open_url_in_default_browser(url)
}

/// Route a parsed `agent_event` payload into the AgentSessionRegistry.
///
/// `pane_session_id` is the **WT pane GUID** ($env:WT_SESSION in the
/// originating pane), carried in the COM broadcast as
/// `params.session_id`. It is NOT the CLI agent's own session id.
/// The agent's session id arrives as `params.agent_session_id` (the
/// `asid` local) and is what we use as the registry key when known —
/// see the module-level docs in `agent_sessions.rs` for the
/// distinction.
///
/// Returns `true` if the registry was updated and the UI should redraw.
#[allow(dead_code)]
pub fn route_agent_event_to_registry(
    reg: &mut crate::agent_sessions::AgentSessionRegistry,
    pane_session_id: &str,
    params: &serde_json::Value,
) -> bool {
    route_agent_event_to_registry_with_hook_sink(reg, pane_session_id, params, |_| {})
}

pub fn route_agent_event_to_registry_with_hook_sink<F>(
    reg: &mut crate::agent_sessions::AgentSessionRegistry,
    pane_session_id: &str,
    params: &serde_json::Value,
    mut hook_sink: F,
) -> bool
where
    F: FnMut(crate::agent_sessions::SessionEvent),
{
    use crate::agent_sessions::{CliSource, SessionEvent};
    use std::path::PathBuf;

    let event = params.get("event").and_then(|v| v.as_str()).unwrap_or("");
    if !event.starts_with("agent.") {
        tracing::debug!(target: "agent_route", event = %event, "skipped: not agent.*");
        return false;
    }

    let cli_source = CliSource::parse(params.get("cli_source").and_then(|v| v.as_str()));
    let asid = params
        .get("agent_session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let mut key = reg.resolve_or_synthesize_key(asid, pane_session_id);
    // Some agent CLIs fire hooks
    // without populating either `agent_session_id` (in the JSON
    // payload) or `WT_SESSION` (in the env of the hook subprocess).
    // The reproducible case is Copilot CLI's `Notification` hook,
    // which fires when the agent needs user input (e.g. "approve this
    // command?"). Without both inputs, `resolve_or_synthesize_key`
    // hands back `pane:<focused-pane-guid>` — a key that no real
    // session row owns. The reducer then no-ops, AND the synthetic
    // key gates the event out of the master publish path (see
    // `key_is_synthetic` below), so master never learns the row is
    // now waiting for input. Net effect: the session management row stays at
    // `Working` ("Active") from the prior `tool.starting` and never
    // flips to `Attention` ("Waiting for input").
    //
    // The fallback is intentionally narrow:
    //   * Only triggers when the resolved key is synthetic AND the
    //     event carried no agent_session_id at all (so we don't paper
    //     over genuinely unknown session ids the agent DID provide).
    //   * Only triggers for the events that observably exhibit the
    //     missing-id problem in the wild — limiting blast radius if a
    //     CLI starts emitting hooks for sessions WTA truly doesn't
    //     know about.
    //   * Filters by `cli_source` so a sessionless Copilot event can't
    //     accidentally land on the user's Claude row.
    //   * `most_recent_live_session_for_cli` rejects `Unknown` cli
    //     hints, so any event without a trustworthy CLI label still
    //     falls through to the synthetic key.
    let mut key_is_synthetic = key.starts_with("pane:");
    if key_is_synthetic && asid.is_empty() {
        let needs_fallback = matches!(
            event,
            "agent.notification"
                | "agent.tool.starting"
                | "agent.tool.completed"
                | "agent.tool.finished"
                | "agent.tool.failed"
        );
        if needs_fallback {
            if let Some(fallback) = reg.most_recent_live_session_for_cli(&cli_source) {
                tracing::info!(
                    target: "agent_route",
                    event = %event,
                    cli_source = ?cli_source,
                    pane_session_id = %pane_session_id,
                    from = %key,
                    to = %fallback,
                    "sessionless hook: falling back to most-recently-active live session for cli",
                );
                key = fallback;
                key_is_synthetic = false;
            }
        }
    }
    let key_for_refresh = key.clone();
    // Per-agent-event — debug, not info.
    tracing::debug!(
        target: "agent_route",
        event = %event,
        asid = %asid,
        key = %key,
        pane_session_id = %pane_session_id,
        cli_source = ?cli_source,
        "routing"
    );

    let payload = params
        .get("payload")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let cwd = payload
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
        .unwrap_or_default();
    let cwd_label = cwd
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();

    let session_known = reg.has_session(&key);
    let synth_title: String = if session_known {
        String::new()
    } else {
        cwd_label.clone()
    };
    // A `pane:<guid>` key means we couldn't resolve a real ACP session id
    // from the event payload (broken hook, race between hook arrival and
    // `new_session` reaching master, etc.) AND the cli-source fallback
    // above (`most_recent_live_session_for_cli`) didn't find a live
    // session to attach to. Keep the local placeholder for helper
    // bookkeeping (so `is_agent_pane(pane_id)` works for the OSC 133;A
    // handler) but DO NOT publish these to master — master only ever
    // learns about real ACP sessions via `new_session`/`load_session`,
    // and feeding it synthetic rows produces duplicate session management entries that
    // shadow the real session (one with the real sid, one with `pane:`
    // key, both pointing at the same agent — see PR B debug session log
    // around 2026-05-28T00:30 for the user-visible repro).
    let needs_synthetic_start = event != "agent.session.started" && !session_known;
    if needs_synthetic_start {
        let synthetic_event = SessionEvent::SessionStarted {
            key: key.clone(),
            cli_source: cli_source.clone(),
            pane_session_id: pane_session_id.to_string(),
            cwd: cwd.clone(),
            title: synth_title.clone(),
        };
        reg.apply(synthetic_event.clone());
        if !key_is_synthetic {
            hook_sink(synthetic_event);
        }
    }

    if event == "agent.session.started" && !asid.is_empty() {
        reg.drop_synthetic_for_pane(pane_session_id);
    }

    let ev = match event {
        "agent.session.started" | "agent.session.start" => SessionEvent::SessionStarted {
            key,
            cli_source,
            pane_session_id: pane_session_id.to_string(),
            cwd,
            title: synth_title,
        },
        "agent.tool.starting" => {
            let tool_name = payload
                .get("tool_name")
                .or_else(|| payload.get("toolName"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if crate::agent_sessions::is_user_input_tool(&tool_name) {
                let tool_event = SessionEvent::ToolStarting { key: key.clone(), tool_name };
                reg.apply(tool_event.clone());
                hook_sink(tool_event);
                let message = payload.get("tool_input")
                    .and_then(|ti| ti.get("question")
                        .or_else(|| ti.get("prompt"))
                        .or_else(|| ti.get("message")))
                    .and_then(|v| v.as_str())
                    .unwrap_or("waiting for user input")
                    .to_string();
                SessionEvent::Notification { key, message }
            } else {
                SessionEvent::ToolStarting { key, tool_name }
            }
        }
        "agent.prompt.submit" => SessionEvent::ToolStarting {
            key,
            tool_name: "prompt".to_string(),
        },
        // Tool completion does NOT end the turn. Copilot and Gemini fire a
        // `tool.finished` per tool — often several per turn, in parallel
        // batches — but the agent keeps working (thinking, streaming text,
        // running the next tool) until it emits `agent.stop`. Mapping each
        // `tool.finished` to `ToolCompleted` made multi-tool turns flicker to
        // Idle and, worse, sit at Idle during the agent's between-tool thinking
        // (Copilot fires only one `prompt.submit` + one `agent.stop` per user
        // request, with many tool pairs in between). So ignore tool completions
        // here and let `agent.stop` own the turn-end → Idle, mirroring the
        // watcher's turn-based `classify_copilot` / `classify_codex`, which also
        // ignore `tool.execution_complete`. Claude/Codex don't emit `tool.*`
        // hook events at all, so this only affects Copilot/Gemini.
        "agent.tool.completed" | "agent.tool.finished" | "agent.tool.failed" => {
            return reg.take_dirty();
        }
        "agent.stop" | "agent.subagent.stop" => SessionEvent::ToolCompleted { key },
        "agent.notification" => SessionEvent::Notification {
            key,
            message: payload
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        },
        "agent.session.stopped" | "agent.session.end" => SessionEvent::SessionStopped {
            key,
            reason: payload
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        },
        "agent.error" => SessionEvent::ConnectionFailed {
            pane_session_id: pane_session_id.to_string(),
            reason: payload
                .get("error")
                .and_then(|v| v.as_str())
                .or_else(|| payload.get("message").and_then(|v| v.as_str()))
                .unwrap_or("agent error")
                .to_string(),
        },
        _ => return reg.take_dirty(),
    };

    reg.apply(ev.clone());
    // Same synthetic-key gate as the SessionStarted placeholder above:
    // events keyed by `pane:<guid>` are helper-local bookkeeping only
    // and must NOT be published to master. Their session_id is fake
    // and would land in master's registry as a duplicate row alongside
    // the real ACP session.
    if !key_is_synthetic {
        hook_sink(ev);
    }

    // Stamp `AgentPane` origin on the live session if the agent-pane
    // origin index recorded its session id. This is what flips the
    // "agent pane" prefix on for *live* rows — historical rows pick up
    // the same flag through `history_loader::load_all`'s join. We
    // re-read the index on every routed event (small file, infrequent
    // event) rather than caching, to stay correct after a new session
    // is created while wta is already running.
    if !key_for_refresh.is_empty() {
        let agent_pane_keys = crate::agent_pane_origin::load_default_set();
        if agent_pane_keys.contains(&key_for_refresh) {
            reg.set_origin(
                &key_for_refresh,
                crate::agent_sessions::SessionOrigin::AgentPane,
            );
        }
    }

    let dirty = reg.take_dirty();
    // Per-agent-event (partner of "routing") — debug, not info.
    tracing::debug!(
        target: "agent_route",
        event = %event,
        dirty = dirty,
        session_count = reg.iter_sorted().len(),
        "applied"
    );
    dirty
}

/// Classify a WT protocol event into a notification.
pub fn classify_wt_event(
    method: &str,
    pane_id: &str,
    tab_id: Option<&str>,
    params: &serde_json::Value,
) -> WtNotification {
    let tab = tab_id.map(str::to_string);
    match method {
        "connection_state" => {
            let state = params
                .get("state")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            match state {
                "failed" => WtNotification {
                    severity: WtEventSeverity::Critical,
                    pane_id: pane_id.to_string(),
                    tab_id: tab,
                    summary: format!("Pane {}: connection failed", pane_id),
                    acknowledged: false,
                    age_ticks: 0,
                },
                "closed" => WtNotification {
                    severity: WtEventSeverity::Actionable,
                    pane_id: pane_id.to_string(),
                    tab_id: tab,
                    summary: format!("Pane {}: process exited", pane_id),
                    acknowledged: false,
                    age_ticks: 0,
                },
                "connected" => WtNotification {
                    severity: WtEventSeverity::Informational,
                    pane_id: pane_id.to_string(),
                    tab_id: tab,
                    summary: format!("Pane {}: connected", pane_id),
                    acknowledged: false,
                    age_ticks: 0,
                },
                // "unknown" is sent when the C++ try_as cast fails — ignore it.
                "unknown" => {
                    return WtNotification {
                        severity: WtEventSeverity::Informational,
                        pane_id: pane_id.to_string(),
                        tab_id: tab,
                        summary: String::new(),
                        acknowledged: true, // auto-acknowledge so it never shows
                        age_ticks: 100,     // will be auto-dismissed immediately
                    }
                }
                _ => WtNotification {
                    severity: WtEventSeverity::Informational,
                    pane_id: pane_id.to_string(),
                    tab_id: tab,
                    summary: format!("Pane {}: {}", pane_id, state),
                    acknowledged: false,
                    age_ticks: 0,
                },
            }
        }
        "vt_sequence" => {
            let seq = params
                .get("sequence")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            // OSC 133;D;<exit_code> — FinalTerm "command finished" marker.
            // Emitted by PowerShell/bash shell integration after every command.
            // Format: "osc:133;D;0" (success) or "osc:133;D;1" (failure)
            if let Some(rest) = seq.strip_prefix("osc:133;") {
                let parts: Vec<&str> = rest.splitn(2, ';').collect();
                if parts.first() == Some(&"D") {
                    let exit_code = parts
                        .get(1)
                        .and_then(|s| s.trim().parse::<i32>().ok())
                        .unwrap_or(-1);
                    if exit_code != 0 {
                        // TODO: fetch the actual command text via
                        // wt_read_pane_output(pane_id) and include it here
                        // (e.g. "`ls /nope` failed (exit 1)"). That requires
                        // an async hop; for now surface just the exit code.
                        return WtNotification {
                            severity: WtEventSeverity::Actionable,
                            pane_id: pane_id.to_string(),
                            tab_id: tab,
                            summary: format!("Command failed (exit {})", exit_code),
                            acknowledged: false,
                            age_ticks: 0,
                        };
                    } else {
                        // exit code 0 = success, not interesting
                        return WtNotification {
                            severity: WtEventSeverity::Informational,
                            pane_id: pane_id.to_string(),
                            tab_id: tab,
                            summary: String::new(),
                            acknowledged: true,
                            age_ticks: 100,
                        };
                    }
                }
            }

            // All other VT sequences — not interesting, suppress.
            WtNotification {
                severity: WtEventSeverity::Informational,
                pane_id: pane_id.to_string(),
                tab_id: tab,
                summary: String::new(),
                acknowledged: true,
                age_ticks: 100,
            }
        }
        "set_agent_state" => {
            // handle_event consumes set_agent_state at the top of WtEvent
            // before classification runs, so classify normally never sees
            // it. Add an explicit arm anyway so a future refactor that
            // drops the early return doesn't surface a stray
            // "Pane: set_agent_state" banner via the default catch-all.
            WtNotification {
                severity: WtEventSeverity::Informational,
                pane_id: pane_id.to_string(),
                tab_id: tab,
                summary: String::new(),
                acknowledged: true,
                age_ticks: 100,
            }
        }
        _ => WtNotification {
            severity: WtEventSeverity::Informational,
            pane_id: pane_id.to_string(),
            tab_id: tab,
            summary: format!("Pane {}: {}", pane_id, method),
            acknowledged: false,
            age_ticks: 0,
        },
    }
}

// --- Events ---

/// One entry of an ACP agent's advertised model list, mirrored into the
/// `agent_status` event so the XAML settings page can populate a real
/// dropdown instead of asking the user to type a free-form string.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AcpModelInfo {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Test-visible record of a wtcli command the App fired through the
/// `wt_channel::spawn_*` helpers. Captured under `cfg(test)` so we can
/// assert the agent session view dispatches the right shape of command
/// without needing a live wtcli to verify against.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DispatchedCommandKind {
    FocusPane,
    /// Plain Enter on a terminal-state row in the session management
    /// view: open a new WT tab whose primary pane runs
    /// `<cli> --resume <key>`. (Previously this was a split-pane in the
    /// current tab — see commit history; the new-tab variant keeps the
    /// originating tab clean and matches user expectation that
    /// resuming a historical session is a "go open my session" action,
    /// not a "split my workspace" action.)
    NewTabResume,
    /// Shift+Enter in the session management view — resume a historical
    /// session in the agent pane of a new tab via WT-side coordination +
    /// ACP session/load.
    ResumeInAgentPane,
    /// `decide_enter_action` returned `NotResumable` — a system message
    /// was pushed in the current tab and no wtcli/ACP side effect was
    /// triggered. The argv carries the [`NotResumableReason`] for
    /// observability.
    NotResumable,
}

#[cfg(test)]
#[derive(Clone, Debug)]
pub struct DispatchedCommand {
    pub kind: DispatchedCommandKind,
    pub session_id: Option<String>,
    pub argv: Vec<String>,
}

pub enum AppEvent {
    Key(KeyEvent),
    Tick,
    /// High-frequency (~30Hz) reveal animation tick. Drives the typewriter
    /// smoothing of the streaming agent response (advances `reveal_chars`).
    /// Separate from `Tick` so we can run the reveal at 30fps without
    /// quadrupling the spinner's full-frame flush rate: a `RevealTick` only
    /// forces a redraw when there is unrevealed pending text on the current
    /// tab (`has_reveal_backlog`).
    RevealTick,
    Resize(u16, u16), // terminal resize (handled by ratatui)
    /// XAML focus on our hosting TermControl changed — true when the agent
    /// pane gained focus, false when it lost focus. Sourced from xterm
    /// focus-in/out (CSI I / CSI O) delivered through conpty.
    FocusChanged(bool),
    ConnectionStage(String),
    /// `session_id` lets us route the status update to the originating tab
    /// once an ACP session is bound to it. Pre-session statuses (startup
    /// stages) carry None and fall through to the active tab.
    ProgressStatus {
        session_id: Option<String>,
        status: String,
    },
    AgentConnected {
        name: String,
        model: Option<String>,
        version: Option<String>,
        /// Session id for the implicitly-created `DEFAULT_TAB_ID` ("0")
        /// session at startup. Wires into App.session_to_tab. Other tabs
        /// get their own sessions lazily on first prompt — see
        /// `SessionAttached`.
        session_id: String,
        /// ACP-advertised models (NewSessionResponse.models.available_models).
        /// Empty when the agent didn't fill the field.
        available_models: Vec<AcpModelInfo>,
        /// ACP-advertised current model id (NewSessionResponse.models.current_model_id).
        current_model_id: Option<String>,
        /// Whether the agent advertised the `loadSession` capability in
        /// the initialize response. Used by the session management
        /// view's Shift+Enter handler to short-circuit with a clear
        /// error before opening a new tab when the agent can't
        /// rehydrate ACP sessions.
        load_session_supported: bool,
        /// Whether the agent advertised the `image` prompt capability
        /// (`promptCapabilities.image`) in its initialize response. Gates the
        /// Alt+V image-paste handler so the user gets a clear message instead
        /// of silently sending an image the agent will reject.
        image_supported: bool,
    },
    /// A new ACP session has been created and bound to a tab. Carries the
    /// per-tab model list (each ACP session can advertise its own).
    SessionAttached {
        tab_id: String,
        session_id: String,
        available_models: Vec<AcpModelInfo>,
        current_model_id: Option<String>,
    },
    /// Error scoped to a specific tab. Used by paths that know the tab
    /// (e.g. ACP `session/load` failure) but have no session_id yet
    /// because the session never came up. Routes into that tab's chat as
    /// a normal Error message; does NOT bounce through the auth/global
    /// disconnect fallback that `AgentError` triggers.
    TabError {
        tab_id: String,
        message: String,
    },
    /// Informational system message scoped to a specific tab. Used for
    /// session/load progress notes ("Resuming...", "Session loaded.")
    /// where we want the user to see something before the agent's
    /// session/update replay (if any) arrives.
    TabSystemMessage {
        tab_id: String,
        message: String,
    },
    PromptTemplateLoaded {
        name: String,
    },
    /// The working pane a manual `/fix` resolved to, plumbed back from the ACP
    /// client task so the App can fill `AutofixContext.target_pane_id` on the
    /// in-flight turn. The host fills `Send.parent` from it at execute time —
    /// the agent never echoes a pane id for autofix turns. Routed by
    /// `prompt_id` so a superseded turn (a newer `/fix`) is left untouched.
    AutofixTargetResolved {
        tab_id: Option<String>,
        prompt_id: u64,
        pane_id: String,
    },
    /// Errors raised before a session exists carry None for `session_id`
    /// and route to the active tab; in-flight failures route to the
    /// session's tab. `failure` is the typed classification that drives
    /// recovery (sign-in / `/restart` / show-and-stay); `message` is the
    /// human-readable line to display.
    AgentError {
        session_id: Option<String>,
        failure: crate::protocol::acp::failure::AgentFailure,
        message: String,
    },
    /// A turn that completed successfully at the protocol level but ended on a
    /// soft stop (output-token limit, request budget, or refusal). NOT a
    /// connection failure — the session stays `Connected`; this only appends an
    /// informational line to the session's chat. Emitted *after*
    /// `AgentMessageEnd` so the notice follows the agent's streamed content.
    AgentSoftStop {
        session_id: String,
        reason: crate::protocol::acp::soft_stop::SoftStopReason,
    },
    /// Same-tab single-flight guard rejection. The user submitted a new
    /// prompt while the previous one is still in flight on the same tab.
    /// The ACP client side enforces this for safety; the front-end Enter
    /// handler also has its own guard so the bounce is rare.
    AgentBusy {
        tab_id: String,
    },
    /// WT-side `tab_renamed` event: the user dragged a tab out into a new
    /// window (or otherwise caused the tab's StableId to change). The
    /// underlying helper process survives the drag (conpty + TermControl
    /// are reattached via WT's ContentId mechanism), but the tab key WT
    /// uses to address us has changed. Without rekeying, autofix /
    /// per-tab state events targeting the new id wouldn't match any
    /// entry in `tab_sessions`.
    TabRenamed {
        old_tab_id: String,
        new_tab_id: String,
        /// Dest window id (from WT's `tab_renamed` payload). When this
        /// helper rekeys onto the new id, it also updates `self.window_id`
        /// to this value so subsequent `set_agent_state` / `tab_changed`
        /// events from the new window pass the per-window filter. `None`
        /// for direct AppEvent dispatches that don't carry it (tests).
        new_window_id: Option<String>,
    },
    ExecutionInfo(String),
    AgentThoughtChunk {
        session_id: String,
        text: String,
    },
    AgentMessageChunk {
        session_id: String,
        text: String,
    },
    /// A `user_message_chunk` SessionUpdate received from the agent
    /// during an ACP `session/load` replay. Carries the historical
    /// user prompt that opens the next replayed turn. Accumulated into
    /// `pending_user_replay` and flushed as a `ChatMessage::User` when
    /// the next agent/tool/plan chunk lands or the load completes.
    /// Outside of `loading_session` mode, dropped — copilot uses these
    /// only during load.
    UserMessageReplayChunk {
        session_id: String,
        text: String,
    },
    AgentMessageEnd {
        session_id: String,
    },
    TimingMetric {
        session_id: String,
        note: String,
    },
    ToolCall {
        session_id: String,
        id: String,
        title: String,
        status: String,
    },
    ToolCallUpdate {
        session_id: String,
        id: String,
        status: String,
    },
    Plan {
        session_id: String,
        entries: Vec<PlanEntry>,
    },
    PermissionRequest {
        session_id: String,
        description: String,
        options: Vec<PermOption>,
        responder: tokio::sync::oneshot::Sender<String>,
    },
    SystemMessage(String),
    DebugPipeMessage(DebugMessage),
    /// Push event from Windows Terminal protocol (VT sequence or connection state).
    /// `pane_id` is the WT pane GUID where the event originated.
    /// `tab_id` is the WT tab StableId that owns the pane — used by autofix
    /// routing to send fixes to the failing tab's ACP session rather than
    /// whatever tab WTA happens to be focused on. `None` for events from
    /// older WT builds that don't yet carry tab_id.
    WtEvent {
        method: String,
        pane_id: String,
        tab_id: Option<String>,
        params: serde_json::Value,
    },
    /// Background agent install completed — refresh the detected agents list.
    AgentInstallComplete,
    /// Login progress — device code received, display to user.
    LoginProgress {
        device_code: String,
        verify_url: String,
    },
    /// Login flow completed.
    LoginComplete {
        agent_id: String,
        success: bool,
        /// On failure, the most specific error line captured from the login
        /// process output (if any), surfaced to the user. `None` on success.
        error: Option<String>,
    },
    /// Post-login auth recovery: a genuine post-login reconnect (helper/pipe
    /// mode) for an External-auth agent STILL failed auth, which means the
    /// shared long-lived master CLI was spawned with a stale token and
    /// `authenticate` can't refresh it. The handler shows a transient
    /// "Reconnecting…" and fires `restart_agent_stack` so a fresh master
    /// (which re-reads the now-valid on-disk token) takes over.
    PostLoginAuthRecovery {
        failure: crate::protocol::acp::failure::AgentFailure,
        tab_id: Option<String>,
        agent_id: String,
    },
    /// Dead-man fallback for `PostLoginAuthRecovery`: a successful restart
    /// tears this helper down before this fires; if it DOES fire (restart
    /// dropped/slow), surface the sign-in screen instead of stranding the user
    /// on a perpetual "Reconnecting…". `generation` pins this to the specific
    /// recovery that armed it, so a stale timer can't act on a later state.
    AuthRecoveryTimedOut {
        agent_id: String,
        generation: u64,
    },
    /// Result of `preflight::check_agent` run by main.rs before the TUI
    /// loop starts. If `all_passed()` is false the App switches into
    /// `AppMode::Setup` so the user can install / authenticate the CLI.
    PreflightComplete(PreflightResult),
    /// Background-thread callback from `wt_channel::spawn_wtcli_split_then_focus_with_callback`
    /// (used by `dispatch_resume`) reaches the registry through this variant.
    /// Posting via the main loop keeps `agent_sessions` access single-threaded
    /// and lets `tracing::*` calls emit on a stable thread.
    AgentSessionEvent(crate::agent_sessions::SessionEvent),
    /// Initial bootstrap of the alive-session mirror from master, in
    /// response to the helper's startup `session/list` request. The
    /// payload replaces any existing entries and flips `alive_loaded`
    /// to true so session management routing logic can start trusting `alive.lookup()`
    /// misses as "session is gone". See
    /// `crate::session_registry::apply_snapshot`.
    AliveSnapshotLoaded(Vec<crate::session_registry::SessionInfo>),
    /// Master broadcast a new alive session into the helper's mirror
    /// via `intellterm.wta/session_added` ext-notification. Applied to
    /// `App.alive` from the main event loop so the registry has a
    /// single writer.
    AliveSessionAdded(crate::session_registry::SessionInfo),
    /// Master broadcast that an alive session is gone via
    /// `intellterm.wta/session_removed`. Symmetric counterpart to
    /// `AliveSessionAdded`.
    AliveSessionRemoved(agent_client_protocol::SessionId),
    /// Apply an "upgrade Historical/Ended → Live" join between the
    /// historical-row registry (`agent_sessions`) and the alive-session
    /// mirror. Posted from `AliveSnapshotLoaded` (master's bootstrap
    /// reply): the handler converts each `SessionInfo` into a `(sid, pane)`
    /// pair, dispatches `AliveJoinUpgrade`, and lets the main loop apply it
    /// serialized w.r.t. other agent-sessions mutations.
    ///
    /// See [`crate::agent_sessions::AgentSessionRegistry::apply_alive_session_join`].
    AliveJoinUpgrade(Vec<(String, Option<String>)>),
    SessionsChanged,
    AgentsSnapshotLoaded {
        request_id: u64,
        sessions: Vec<crate::session_registry::SessionInfo>,
    },
    /// `sessions/list` RPC failed or timed out — unblock the tab's
    /// `refetch_in_flight` gate without overwriting the existing
    /// snapshot, so the 5s periodic tick / next `SessionsChanged`
    /// broadcast can retry. Emitted by `dispatch_master_ext_request`'s
    /// `SessionsList` arm when `conn.ext_method(...)` returns Err or
    /// `tokio::time::timeout` elapses. The timeout path is a
    /// workaround for a `agent-client-protocol@0.10` cancellation-
    /// safety bug in `RpcConnection::handle_io`: when
    /// `select_biased!`'s outgoing arm preempts an in-progress
    /// `read_line`, BufReader bytes already pulled off the pipe are
    /// silently dropped, the next read returns a frame starting
    /// mid-message, JSON parse fails, and the matching
    /// `pending_responses` entry never resolves — so the
    /// `ext_method` future would otherwise wait forever, keeping
    /// `refetch_in_flight=true` permanently for the affected tab.
    /// See the GH issue for upgrading to 0.12.
    AgentsSnapshotFailed {
        request_id: u64,
    },
    MasterMutationCompleted {
        request_id: u64,
    },
}

// --- Per-tab session storage ---

pub(crate) const DEFAULT_TAB_ID: &str = "0";

/// Single-axis scroll cursor. All mutations go through methods so callers
/// don't reinvent saturating-math; the upper bound `max` is established by
/// the layout/render pass once total content height is known and re-clamps
/// on every frame.
///
/// `by` deliberately does NOT clamp to `max` — the bound may be stale at
/// input time (the lazy chat build only learns `max` after exhausting
/// history). Clamping happens on the next `set_max`.
#[derive(Debug, Default, Clone, Copy)]
pub struct Scroll {
    pub offset: usize,
    pub max: usize,
}

impl Scroll {
    pub fn by(&mut self, delta: isize) {
        self.offset = if delta >= 0 {
            self.offset.saturating_add(delta as usize)
        } else {
            self.offset.saturating_sub(delta.unsigned_abs())
        };
    }

    /// Jump to an absolute offset, clamped to current `max`. Only meaningful
    /// after `max` has been set this frame.
    pub fn set(&mut self, offset: usize) {
        self.offset = offset.min(self.max);
    }

    pub fn set_max(&mut self, max: usize) {
        self.max = max;
        if self.offset > max {
            self.offset = max;
        }
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}


/// Everything that conceptually belongs to one tab's conversation: the
/// message history, the streaming buffer of the in-flight prompt, the
/// pending tool calls, the recommendations panel state, etc.
///
/// `App` holds a `HashMap<TabId, TabSession>` and a `tab_id` pointing at
/// the currently focused entry. Renderers read via `app.current_tab()`;
/// event handlers route updates to the relevant `TabSession` rather than
/// mutating shared `App` fields.
#[derive(Default)]
pub struct TabSession {
    /// Per-tab autofix state machine (see `TabAutofixState`).
    pub autofix: TabAutofixState,

    // Conversation history
    pub messages: Vec<ChatMessage>,
    pub completed_turns: Vec<CompletedTurn>,
    /// Tab/Shift+Tab selects a past turn (most recent first). Enter then
    /// toggles `CompletedTurn.expanded`. None means no selection — Enter
    /// goes to the input/prompt path as before.
    pub selected_completed_turn_idx: Option<usize>,
    pub chat_scroll: Scroll,

    // Streaming state
    pub pending_agent_response: String,
    /// Accumulator for `session/update` user_message_chunk events
    /// arriving during an ACP `session/load` replay (the historical
    /// user prompt for the next replayed turn). Flushed as a
    /// `ChatMessage::User` whenever a turn boundary is detected — an
    /// agent message / thought / tool call starts, OR the load
    /// completes (SessionAttached for the loading tab).
    pub pending_user_replay: String,
    /// True between the inbound `load_session` event and the
    /// `SessionAttached` event that closes out the ACP `session/load`
    /// call. While set, session/update chunk handlers accept chunks
    /// even though no `TurnState::Submitted` was created for the
    /// replay — `turn` stays Idle through the load.
    pub loading_session: bool,
    /// The session id we're currently loading into this tab, set when
    /// `loading_session` flips to true. The `SessionAttached` handler
    /// closes the replay window only when an attach event arrives whose
    /// `session_id` matches this value — otherwise an unrelated
    /// `SessionAttached` (e.g. the helper's bootstrap `session/new`
    /// that completed while a Plan-C `--initial-load-session-id` was
    /// still being processed) would prematurely flip `loading_session`
    /// off and the agent's replay chunks would be dropped at the chunk
    /// handlers' `if !loading_session { return; }` gate.
    pub loading_target_session_id: Option<String>,
    // Explicit per-turn lifecycle. Source of truth in the new state machine
    // (see `doc/specs/turn-state-refactor.md`).
    pub turn: TurnState,

    // Agent-supplied progress message (e.g. "Reading file foo.rs"). Falls
    // back to the spinner label derived from `turn` when None.
    pub progress_status: Option<String>,
    pub activity_frame: usize,
    /// Typewriter reveal cursor: how many characters of the *user-visible*
    /// streaming text are currently shown. The full text lives in
    /// `turn.buffer()`; the renderer only emits the first `reveal_chars`
    /// chars of it. Advanced toward the full length by `RevealTick`
    /// (`advance_reveal`), reset to 0 when a new turn starts streaming, and
    /// made irrelevant on finalize (the committed message renders in full).
    pub reveal_chars: usize,
    pub timing_note: Option<String>,
    pub selection_visible_pending: bool,

    // Tool calls / permission
    pub tool_calls: HashMap<String, (String, String)>,
    /// FIFO of pending permission requests for this session. The front
    /// entry is the one currently rendered and accepting keys; the rest
    /// queue up. Agents (Copilot in particular) sometimes fire multiple
    /// concurrent `request_permission` calls for one tool invocation
    /// — e.g. one per path that needs to be unlocked outside the trusted
    /// directory set — and each carries its own oneshot responder. The
    /// previous single-slot `Option` overwrote the prior entry on every
    /// new request, dropping its responder, which `WtaClient::request_permission`
    /// observed as `Cancelled` and the agent interpreted as "user rejected"
    /// — producing the silent tool-call failure tracked alongside the
    /// helper+master split.
    pub permission: VecDeque<PermissionState>,
    // Recommendation card UI focus (the set itself lives on
    // `turn.recommendations()`).
    pub selected_recommendation: usize,
    pub selected_button: usize,
    pub rec_scroll: Scroll,

    /// Last value the helper published for this tab in a
    /// `set_agent_chip_target` event. `Some(pane_id)` means we last asked
    /// C++ to pin the blue "Agent" chip onto that pane; `None` means we
    /// last asked C++ to fall back to the source-of-agent flag. Used as a
    /// dedupe key so we only fire an event when the effective chip target
    /// actually changes.
    pub last_emitted_chip_override: Option<String>,


    // Input editor state — per-tab so each tab keeps its own draft text,
    // cursor, and slash-command popup across switches.
    pub input: String,
    pub cursor_pos: usize,
    /// Images captured from the clipboard via Alt+V, waiting to be sent with
    /// the next prompt. Rendered as `[image #N]` chips above the input; drained
    /// into the `PromptSubmission` on Enter and cleared after submit, and on
    /// `/clear` / `/new` / session reset via `clear_chat_history`.
    pub pending_images: Vec<crate::clipboard_image::PastedImage>,
    /// Recomputed on every input mutation. Empty when not in
    /// command-prefix mode. The popup renderer treats an empty Vec as
    /// "do not render".
    pub command_popup_candidates: Vec<&'static CommandSpec>,
    /// Index into [`Self::command_popup_candidates`]. Clamped on every
    /// mutation that could shrink the list.
    pub command_popup_selected: usize,

    // Filled in Milestone 2 once each tab has its own ACP SessionId.
    #[allow(dead_code)]
    pub session_id: Option<String>,

    /// Per-pane ACP model override, set by the `/model` picker. `None` means
    /// "follow the global `acpModel` setting"; `Some(id)` pins this pane to a
    /// specific model and survives `/new` (re-applied to fresh sessions in the
    /// `SessionAttached` handler via `effective_model_for_tab`). It is a
    /// transient per-pane tweak: a global `acpModel` settings change is
    /// authoritative and clears it (see `apply_global_acp_model`). In-memory
    /// only — not persisted across pane close / Terminal restart. See
    /// `App::commit_model_pick`.
    pub model_override: Option<String>,
    /// True while the `/model` picker modal is up for this tab. Drives both
    /// the key-event intercept in `handle_key` and the popup render.
    pub model_picker_open: bool,
    /// Highlighted row in the open model picker — an index into the agent's
    /// advertised `App::available_models`. Clamped on open.
    pub model_picker_selected: usize,

    // agent session view (`/sessions`) — per-tab so each WT tab keeps
    // its own open/closed state and selected row across tab switches.
    pub current_view: View,
    pub agents_list_state: ratatui::widgets::ListState,
    pub agents_view: AgentsViewState,

    // "Does this tab want the agent pane visible?" — per-tab user intent.
    // Independent of where the (single, shared) XAML pane physically lives:
    // C++ relocates the pane to whichever active tab has `pane_open == true`
    // and hides it on tabs where it's `false`. wta owns this state so the
    // C++ side has one writer (`OnAgentStateChanged`) and the desync that
    // came from tracking it as a per-Tab.AgentPaneOpen flag on a moving
    // XAML pane is gone.
    //
    // Default false. Seeded to true at startup for the spawn owner tab
    // (the user just asked to open the pane on that tab). Flipped by
    // C++-originated `set_agent_state` requests (hotkey/button toggles)
    // and by wta-internal events like Ctrl+C×2 reset.
    pub pane_open: bool,

    // Pre-entry pane visibility, remembered when the user opens the
    // session-management (Agents) view so Esc can restore *that* state rather
    // than always landing on an open chat pane:
    //   * `Some(false)` — entered from a folded (stashed) pane → Esc re-folds.
    //   * `Some(true)`  — entered from an expanded chat pane → Esc returns to it.
    //   * `None`        — not currently in / entering the Agents view.
    // Captured in `open_agents_view_for_tab`, read by the Esc handler, cleared
    // in `close_agents_view_for_tab`. The capture is reliable because the C++
    // `set_agent_state` request applies `view` before `pane_open`: an unstash
    // sends `{view:sessions, pane_open:true}`, but the view switch (and thus
    // our snapshot) runs while `pane_open` still holds the old `false`.
    pub agents_view_prev_pane_open: Option<bool>,
}

impl TabSession {
    pub fn scroll_to_bottom(&mut self) {
        self.chat_scroll.offset = 0;
    }

    /// Whether the input box is the live, enterable caret target. False when
    /// the user is browsing a completed turn, a recommendation card is
    /// showing, or a permission card is up — in all three the input is not
    /// enterable (`handle_key` routes keys to that surface and returns early),
    /// so ↑↓ navigate it instead. UI indicators that track "is the input cell
    /// live" (e.g. the painted caret cell) gate on this together with the
    /// pane's XAML focus, so a non-enterable state reads the same as lost
    /// focus.
    pub fn input_has_nav_focus(&self) -> bool {
        self.selected_completed_turn_idx.is_none()
            && self.turn.recommendations().is_none()
            && self.permission.is_empty()
    }

    pub fn clear_recommendations(&mut self) {
        self.selected_recommendation = 0;
        self.selected_button = 0;
        self.rec_scroll.reset();
    }

    /// The pane the "Agent" chip should be pinned to while this tab has a
    /// recommendation card with a `Send` action selected, or `None` when the
    /// tab is not in that state. Returning `None` lets the C++ side fall
    /// back to its default behavior (chip follows the source-of-agent flag).
    ///
    /// Resolution order for the pane id:
    ///   1. `Send.parent` on the selected choice when non-empty.
    ///   2. Autofix `target_pane_id` on the current prompt (for autofix
    ///      turns where the recommendation's `Send.parent` is left blank
    ///      and only gets filled at execute time — see `turn_execute_card`).
    pub fn compute_chip_card_target(&self) -> Option<String> {
        let recs = self.turn.recommendations()?;
        let choice = recs.choices.get(self.selected_recommendation)?;
        let send_parent = choice.actions.iter().find_map(|a| match a {
            crate::coordinator::RecommendedAction::Send { parent, .. } if !parent.is_empty() => {
                Some(parent.clone())
            }
            _ => None,
        });
        if send_parent.is_some() {
            return send_parent;
        }
        // Autofix fallback: the autofix prompt's `target_pane_id` is what
        // `turn_execute_card` will fill `Send.parent` with at execute time,
        // so the chip should already point there now. Filter out empty
        // strings — the C++ side treats `pane_session_id == ""` as "no
        // override", so emitting `Some("")` would let the helper's dedupe
        // believe it pinned the chip while WT silently ignores the event.
        if choice
            .actions
            .iter()
            .any(|a| matches!(a, crate::coordinator::RecommendedAction::Send { .. }))
        {
            return self
                .turn
                .prompt()
                .and_then(|p| p.autofix.as_ref())
                .map(|a| a.target_pane_id.clone())
                .filter(|s| !s.is_empty());
        }
        None
    }

    pub fn clear_chat_history(&mut self) {
        self.messages.clear();
        self.tool_calls.clear();
        // Dropping pending responders signals `Cancelled` back to the
        // agent — appropriate when the user wipes chat history mid-turn.
        self.permission.clear();
        self.progress_status = None;
        self.activity_frame = 0;
        self.pending_agent_response.clear();
        self.pending_user_replay.clear();
        self.chat_scroll.reset();
        self.timing_note = None;
        self.selection_visible_pending = false;
        self.turn = TurnState::Idle;
        self.clear_recommendations();
        // Drop any clipboard image queued but not yet sent — a wiped/fresh
        // conversation must not carry a stale attachment into the next prompt.
        self.pending_images.clear();
    }

    /// Flush pending user/agent replay buffers at a turn boundary during
    /// an ACP `session/load`. Called when a new user_message_chunk
    /// arrives (the previous agent turn is complete) and again at end
    /// of load to drain whatever remains. Empty buffers no-op.
    pub fn flush_load_replay_pending(&mut self) {
        if !self.pending_user_replay.is_empty() {
            let text = std::mem::take(&mut self.pending_user_replay);
            self.messages.push(ChatMessage::User(text));
        }
        if !self.pending_agent_response.is_empty() {
            let text = std::mem::take(&mut self.pending_agent_response);
            self.messages.push(ChatMessage::Agent(text));
        }
    }

    /// Compact replayed history into collapsed `CompletedTurn` rows so a
    /// long resumed session doesn't dump the entire transcript inline.
    /// Called at session/load completion (after `flush_load_replay_pending`)
    /// from the `SessionAttached` handler.
    ///
    /// Algorithm: walk `self.messages` left-to-right; each `User` opens a
    /// new turn. The turn's `prompt` is a SHORT single-line preview of
    /// the user text (so the collapsed `▶ > <preview>` row stays at one
    /// visual line even for huge system-prompt-as-user dumps); the full
    /// original `User(text)` is stored as the first entry of `details`,
    /// followed by subsequent non-User messages. Messages that come
    /// BEFORE the first User (e.g. the `System("Resuming session …")`
    /// marker, or a stray Agent dump) stay in `messages` as-is — only
    /// User-anchored turns get packed. Each packed turn has `expanded:
    /// false` so history is collapsed by default. Tab + Enter toggles
    /// individual rows.
    pub fn pack_replayed_messages_into_turns(&mut self) {
        if self.messages.is_empty() {
            return;
        }
        let drained: Vec<ChatMessage> = std::mem::take(&mut self.messages);
        let mut kept: Vec<ChatMessage> = Vec::new();
        // `details` always opens with the full original ChatMessage::User
        // so expanding the turn shows the entire prompt text. `prompt`
        // is the short preview used in the collapsed header row.
        let mut current: Option<(String, Vec<ChatMessage>)> = None;
        for msg in drained {
            match msg {
                ChatMessage::User(text) => {
                    if let Some((prompt, details)) = current.take() {
                        self.completed_turns.push(CompletedTurn {
                            prompt,
                            details,
                            expanded: false,
                            trailing_marker: None,
                        });
                    }
                    let preview = collapsed_prompt_preview(&text);
                    let details = vec![ChatMessage::User(text)];
                    current = Some((preview, details));
                }
                other => {
                    if let Some((_, details)) = current.as_mut() {
                        details.push(other);
                    } else {
                        kept.push(other);
                    }
                }
            }
        }
        if let Some((prompt, details)) = current.take() {
            self.completed_turns.push(CompletedTurn {
                prompt,
                details,
                expanded: false,
                trailing_marker: None,
            });
        }
        self.messages = kept;
    }

    /// Cycle the past-turn selection toward older entries.
    /// `None → last (most recent) → ... → 0 → None`. No-op when there are
    /// no completed turns.
    pub fn select_older_completed_turn(&mut self) {
        let len = self.completed_turns.len();
        if len == 0 {
            self.selected_completed_turn_idx = None;
            return;
        }
        self.selected_completed_turn_idx = match self.selected_completed_turn_idx {
            None => Some(len - 1),
            Some(0) => None,
            Some(i) => Some(i - 1),
        };
    }

    /// Cycle the past-turn selection toward newer entries.
    /// `None → 0 (oldest) → ... → last → None`.
    pub fn select_newer_completed_turn(&mut self) {
        let len = self.completed_turns.len();
        if len == 0 {
            self.selected_completed_turn_idx = None;
            return;
        }
        self.selected_completed_turn_idx = match self.selected_completed_turn_idx {
            None => Some(0),
            Some(i) if i + 1 >= len => None,
            Some(i) => Some(i + 1),
        };
    }

    /// Flip `expanded` on the currently selected past turn. No-op if nothing
    /// is selected or the index is out of range (defensive — selection
    /// should track turn count, but a stale index shouldn't panic).
    pub fn toggle_selected_completed_turn(&mut self) {
        let Some(idx) = self.selected_completed_turn_idx else {
            return;
        };
        if let Some(turn) = self.completed_turns.get_mut(idx) {
            turn.expanded = !turn.expanded;
        }
    }

    pub fn current_turn_details(&self) -> Vec<ChatMessage> {
        self.messages
            .iter()
            .filter(|message| !matches!(message, ChatMessage::User(_)))
            .cloned()
            .collect()
    }

    pub fn clear_input(&mut self) {
        self.input.clear();
        self.cursor_pos = 0;
        self.refresh_command_popup();
    }

    pub fn insert_input_char(&mut self, ch: char) {
        self.cursor_pos = clamp_cursor_to_boundary(&self.input, self.cursor_pos);
        self.input.insert(self.cursor_pos, ch);
        self.cursor_pos += ch.len_utf8();
        self.refresh_command_popup();
    }

    pub fn delete_before_cursor(&mut self) {
        self.cursor_pos = clamp_cursor_to_boundary(&self.input, self.cursor_pos);
        if self.cursor_pos == 0 {
            return;
        }

        let previous = prev_char_boundary(&self.input, self.cursor_pos);
        self.input.replace_range(previous..self.cursor_pos, "");
        self.cursor_pos = previous;
        self.refresh_command_popup();
    }

    pub fn delete_word_before_cursor(&mut self) {
        self.cursor_pos = clamp_cursor_to_boundary(&self.input, self.cursor_pos);
        if self.cursor_pos == 0 {
            return;
        }
        let word_start = prev_word_boundary(&self.input, self.cursor_pos);
        self.input.replace_range(word_start..self.cursor_pos, "");
        self.cursor_pos = word_start;
        self.refresh_command_popup();
    }

    pub fn delete_at_cursor(&mut self) {
        self.cursor_pos = clamp_cursor_to_boundary(&self.input, self.cursor_pos);
        if self.cursor_pos >= self.input.len() {
            return;
        }

        let next = next_char_boundary(&self.input, self.cursor_pos);
        self.input.replace_range(self.cursor_pos..next, "");
        self.refresh_command_popup();
    }

    pub fn move_cursor_left(&mut self) {
        self.cursor_pos = prev_char_boundary(&self.input, self.cursor_pos);
    }

    pub fn move_cursor_right(&mut self) {
        self.cursor_pos = next_char_boundary(&self.input, self.cursor_pos);
    }

    pub fn move_cursor_word_left(&mut self) {
        self.cursor_pos = prev_word_boundary(&self.input, self.cursor_pos);
    }

    pub fn move_cursor_word_right(&mut self) {
        self.cursor_pos = next_word_boundary(&self.input, self.cursor_pos);
    }

    pub fn move_cursor_home(&mut self) {
        self.cursor_pos = 0;
    }

    pub fn move_cursor_end(&mut self) {
        self.cursor_pos = self.input.len();
    }

    /// Recompute the slash-command popup candidates from the current
    /// input. Called after every input mutation. Clamps the selected
    /// index so it stays valid when the candidate list shrinks.
    pub fn refresh_command_popup(&mut self) {
        if commands::is_command_prefix(&self.input) {
            // Strip leading whitespace + the `/` to get the user's
            // partial name. `is_command_prefix` already guarantees the
            // shape, so the unwrap is safe.
            let trimmed = self.input.trim_start();
            let name = trimmed.strip_prefix('/').unwrap_or("");
            self.command_popup_candidates = commands::matches(name);
        } else {
            self.command_popup_candidates.clear();
        }
        if self.command_popup_candidates.is_empty() {
            self.command_popup_selected = 0;
        } else if self.command_popup_selected >= self.command_popup_candidates.len() {
            self.command_popup_selected = self.command_popup_candidates.len() - 1;
        }
    }

    pub fn command_popup_visible(&self) -> bool {
        !self.command_popup_candidates.is_empty()
    }

    pub fn command_popup_up(&mut self) {
        if self.command_popup_selected > 0 {
            self.command_popup_selected -= 1;
        }
    }

    pub fn command_popup_down(&mut self) {
        if self.command_popup_selected + 1 < self.command_popup_candidates.len() {
            self.command_popup_selected += 1;
        }
    }

    pub fn selected_command_spec(&self) -> Option<&'static CommandSpec> {
        self.command_popup_candidates
            .get(self.command_popup_selected)
            .copied()
    }

    /// Tab-completion: replace the input buffer with `/<name> ` (with a
    /// trailing space if the command takes args; otherwise just the
    /// name) and reset the cursor to the end. Triggered by Tab when the
    /// popup is visible.
    pub fn accept_command_popup_completion(&mut self) {
        if let Some(spec) = self.selected_command_spec() {
            self.input = if spec.takes_args {
                format!("/{} ", spec.name)
            } else {
                format!("/{}", spec.name)
            };
            self.cursor_pos = self.input.len();
            self.refresh_command_popup();
        }
    }
}

// --- App ---

pub struct App {
    pub mode: AppMode,
    pub setup: Option<SetupState>,
    pub auth: Option<AuthState>,
    /// Channel for spawning background tasks from event handlers.
    event_tx: Option<mpsc::UnboundedSender<AppEvent>>,
    /// Set after login completes — consumed by main loop to spawn ACP client.
    pub pending_acp_start: bool,
    /// Set by LoginComplete success — consumed once by try_start_acp to pass
    /// `post_login_reconnect=true` to the pipe-mode ACP client. This ensures
    /// the authenticate RPC is only sent on genuine post-login reconnects, not
    /// on agent-switch / retry / install-complete reconnects that also go
    /// through try_start_acp.
    needs_post_login_authenticate: bool,
    /// Monotonic id for the in-flight post-login auth recovery. Bumped each
    /// time `PostLoginAuthRecovery` arms its 8s dead-man timer, and bumped
    /// again on a successful `AgentConnected`. The `AuthRecoveryTimedOut`
    /// fallback only fires if its captured generation still matches — so a
    /// stale timer from an earlier recovery (or one whose connection already
    /// succeeded) cannot force the sign-in screen onto a later, unrelated
    /// `Connecting` state.
    auth_recovery_generation: u64,
    /// Agent ID selected by user (FRE/preflight) — sent to C++ once connected.
    pending_agent_selection: Option<String>,
    /// Show first-run welcome hint until user sends first message.
    pub show_welcome_hint: bool,
    deferred_acp: Option<DeferredAcpParams>,
    pub state: ConnectionState,
    /// The agent ID we're trying to connect to (set at preflight/FRE time).
    pub current_agent_id: String,
    /// True when preflight detected an issue and is showing Setup screen.
    /// Prevents AgentError from overriding the preflight Setup.
    preflight_setup_active: bool,
    pub agent_name: String,
    pub agent_model: Option<String>,
    pub agent_version: Option<String>,
    /// Models the ACP agent advertised at session start. Empty until the
    /// first AgentConnected event with non-empty data; published into the
    /// `agent_status` event so the settings UI can render a dropdown.
    pub available_models: Vec<AcpModelInfo>,
    pub current_model_id: Option<String>,
    pub prompt_name: Option<String>,
    pub session_id: String,
    #[allow(dead_code)]
    pub wt_connected: bool,
    pub terminal_rows: u16,
    pub terminal_cols: u16,
    /// Whether our hosting agent pane currently has XAML focus. Driven by
    /// xterm focus-in/out delivered through conpty. Default true: a freshly
    /// opened pane is normally focused, and conpty only delivers an event
    /// on the *transition*, so absent a signal we assume focused.
    pub pane_focused: bool,
    pub should_quit: bool,
    prompt_tx: mpsc::UnboundedSender<PromptSubmission>,
    recommendation_tx: mpsc::UnboundedSender<crate::coordinator::ChoiceExecution>,
    permission_tx: mpsc::UnboundedSender<String>,
    cancel_tx: mpsc::UnboundedSender<CancelRequest>,
    new_session_tx: mpsc::UnboundedSender<NewSessionForTab>,
    load_session_tx: mpsc::UnboundedSender<LoadSessionForTab>,
    drop_session_tx: mpsc::UnboundedSender<DropSessionRequest>,
    rename_session_tx: mpsc::UnboundedSender<RenameSessionRequest>,
    restart_tx: mpsc::UnboundedSender<RestartRequest>,
    master_request_tx: mpsc::UnboundedSender<crate::protocol::acp::client::MasterExtRequest>,
    debug_capture_enabled: Arc<AtomicBool>,
    /// Cached for creating DeferredAcpParams after auth-error recovery.
    shell_mgr: Arc<crate::shell::ShellManager>,
    // Slash-command UI state. The /help overlay is global — it covers
    // the chat area regardless of which tab is active. Per-tab popup
    // state (the command-completion candidates as the user types `/he…`)
    // lives on `TabSession`.
    pub help_overlay_visible: bool,
    /// True once the helper's ACP transport to wta-master is lost
    /// (`AgentFailure::TransportLost` — master died/crashed/was killed). The
    /// helper has no in-process reconnect, so every slash command except
    /// `/restart` would only fail against the dead pipe. While this is set the
    /// command popup is filtered down to just `/restart` (other commands are
    /// hidden, not greyed), and typing/Entering any other command is refused
    /// with the reconnect hint. `/restart` is the one recovery that routes via
    /// `wtcli publish` → C++ `SharedWta::Restart` (a path that doesn't touch
    /// the dead pipe). Cleared when a fresh connection reaches `Connected`
    /// (e.g. the post-sign-in reconnect).
    pub transport_lost: bool,
    // Debug panel
    pub debug_messages: Vec<DebugMessage>,
    pub show_debug_panel: bool,
    pub debug_scroll: usize,
    // Pane identity (populated via VT channel)
    pub pane_id: Option<String>,
    pub tab_id: Option<String>,
    // The tab id this helper's agent pane was spawned to own. Unlike
    // `tab_id` (which floats with `tab_changed` to track WT's currently-
    // focused tab), this is anchored to the helper's owning pane and
    // follows only `tab_renamed` events (cross-window drag). Used as the
    // `tab_id` field on outbound `agent_status` and `autofix_state` events
    // so the C++ side can route per-pane state to the right
    // AgentPaneContent / bottom bar window without fan-out.
    pub owner_tab_id: Option<String>,
    pub window_id: Option<String>,
    // WT event notifications (global — affects bottom-bar / banner across tabs)
    pub wt_notifications: std::collections::VecDeque<WtNotification>,
    pub show_notification_banner: bool,
    // Auto-fix global on/off. Per-tab autofix machinery (pane_id,
    // generation, suggested_pane_id, armed_at, bar_snapshot) lives on
    // `TabSession.autofix`.
    pub autofix_enabled: bool,
    // Per-tab conversation sessions. Keyed by the stable tab GUID WT mints
    // at tab construction. The active tab is `tab_id` — seeded from the
    // `--owner-tab-id` CLI arg before ACP init in the WT-spawned path, or
    // None (falling back to `DEFAULT_TAB_ID`) for manual `wta` runs.
    // Lazily extended on each new `tab_changed` event.
    pub(crate) tab_sessions: HashMap<String, TabSession>,
    // Reverse lookup: ACP `SessionId` → tab id. Populated from
    // `AgentConnected` (the startup session, bound to whichever tab the
    // process owns) and `SessionAttached` (lazily-created sessions for
    // other tabs the user has visited). All ACP-emitted events route via
    // this map: chunks, tool calls, end notifications all carry a
    // `session_id`, the App looks up the owning tab and writes to that
    // `TabSession`.
    session_to_tab: HashMap<String, String>,
    // ── Agent management view state (re-applied on top of theirs) ──
    /// Live & historical CLI agent sessions. Populated from `agent_event`
    /// hook payloads via `route_agent_event_to_registry`. Cross-tab — the
    /// session list itself is global; only the *picker view* (open state
    /// + selected row) lives per-tab on `TabSession`.
    pub agent_sessions: crate::agent_sessions::AgentSessionRegistry,
    /// Whether the connected ACP agent advertised the `loadSession`
    /// capability in its initialize response. Used by the
    /// session management view's Shift+Enter handler to short-circuit
    /// with a clear error before opening a new tab when the agent
    /// can't rehydrate ACP sessions. Set on `AgentConnected`.
    pub agent_supports_load_session: bool,
    /// Whether the connected ACP agent advertised the `image` prompt
    /// capability (`promptCapabilities.image`). Gates the Alt+V image-paste
    /// handler. Set on `AgentConnected`.
    pub agent_supports_image: bool,
    /// Origin filter for the `/sessions` picker. Captured once at
    /// `App::new` time via [`resolve_sessions_origin_filter`] so the value is
    /// stable for the lifetime of this helper process. Read by
    /// [`Self::agents_rows_for_tab`] (the cursor / Enter source of
    /// truth), the post-history-scan auto-select, the Delete clamp,
    /// and the `agents_view::render` call in `ui/layout.rs`. See
    /// [`MVP_SESSIONS_ORIGIN_FILTER`] for the gate to flip when un-MVP.
    pub sessions_origin_filter: crate::agent_sessions::OriginFilter,
    // Onboarding: signals main.rs to install agent hook plugins on demand.
    install_request_tx: Option<mpsc::UnboundedSender<()>>,
    /// Posts `AppEvent::AgentSessionEvent` from background callbacks
    /// (split-pane callback in `dispatch_resume`) back into the main
    /// event loop so they can apply to `agent_sessions` on the UI thread.
    /// Set by `set_agent_event_tx` from main.rs after the event channel
    /// is constructed; remains None in tests so dispatch_resume is a
    /// no-op outside the integration loop.
    agent_event_tx: Option<mpsc::UnboundedSender<AppEvent>>,
    /// Helper-mode fire-and-forget publisher for `intellterm.wta/session_hook`.
    session_hook_tx: Option<mpsc::UnboundedSender<crate::agent_sessions::SessionEvent>>,
    /// Hot-updatable delegate config, shared with the recommendation
    /// executor (`run_recommendation_executor`). Rebuilt in place on an
    /// `agent_config_changed` settings event so the configured delegate
    /// agent/model can change without restarting the agent pane. None in
    /// tests / manual runs where no executor is wired.
    delegate_agents:
        Option<Arc<std::sync::Mutex<Vec<crate::coordinator::DelegateAgentRuntime>>>>,
    /// The helper's own `--agent` cmdline. Needed to re-derive the delegate
    /// runtime commandline when only the delegate agent/model change.
    delegate_base_agent_cmd: String,
    /// The configured ACP model override (the `--acp-model` setting). Seeded
    /// from the spawn cmdline and updated on `agent_config_changed`. Re-applied
    /// to every freshly-created session (via `SessionAttached`) so `/new` and
    /// lazy-first-prompt sessions stay on the configured model, not just the
    /// bootstrap one. None = "agent default" (no override).
    acp_model: Option<String>,
    /// Test-only: last command issued via the agent session view's Enter
    /// dispatch (`dispatch_resume` / focus). Used by unit tests in
    /// place of a live wtcli; not compiled into release builds.
    #[cfg(test)]
    pub last_dispatched_command: Option<DispatchedCommand>,
    /// Source pane GUID (set from `WTA_SOURCE_SESSION_ID` env var by the
    /// launching pane). Used by autofix to attribute which pane originated
    /// the failing command we're about to fix.
    pub source_session_id: Option<String>,
    /// Source pane working directory (set from `WTA_SOURCE_CWD`).
    pub source_cwd: Option<String>,
    /// When true, surface raw `agent_event` payloads in the chat as
    /// `ChatMessage::AgentEvent` for diagnostics. Controlled by the
    /// `WTA_LOG_AGENT_EVENT` env var (1/true/yes).
    pub log_agent_events: bool,
    /// Spinner tick counter used by Setup mode (per-tab `activity_frame`
    /// drives chat-mode spinners; this one is for the wizard view which
    /// has no tab context). Bumped from the Tick handler when in Setup.
    pub activity_frame: u8,

    /// First-press timestamp of the double-Ctrl+C "close pane" sequence. Set
    /// when the user presses Ctrl+C while input is empty and nothing is in
    /// flight. A second Ctrl+C within `CLOSE_PANE_ARM_WINDOW` closes the
    /// pane (we ask WT to do it; ConPty then SIGKILLs us). Cleared on any
    /// other key, on prompt activity, or after the window elapses.
    pub close_pane_armed_at: Option<std::time::Instant>,
    /// Transient one-line hint rendered at the bottom of the chat area
    /// (e.g. "Press Ctrl+C again to close pane"). Auto-clears at the
    /// recorded deadline.
    pub transient_hint: Option<(String, std::time::Instant)>,
    /// Mirror of master's authoritative live-session set, pushed via
    /// ACP `intellterm.wta/session_*` ext-notifications. session management Enter
    /// routing reads this to decide Focus vs Resume without an extra
    /// IPC round-trip. Wired into B-6 (subscribe) and B-10 (consult);
    /// here we just hold the mirror so the rest of the helper can
    /// reference it through a stable handle.
    pub alive: std::sync::Arc<dyn crate::session_registry::SessionRegistry>,
    /// True once we've received the initial `session/list` snapshot
    /// from master. Until then, the helper must *not* interpret an
    /// `alive.lookup()` miss as "session is dead" — there's a window
    /// at startup where the registry is legitimately empty because
    /// the bootstrap RPC hasn't returned yet. Tracked as an Atomic so
    /// the bootstrap task can flip it from a non-`&mut self` context.
    pub alive_loaded: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// How long the "Press Ctrl+C again to close pane" arm stays live. Long
/// enough that the user can react after seeing the hint; short enough that
/// a stale arm doesn't bite the next time they want to clear input.
pub const CLOSE_PANE_ARM_WINDOW: std::time::Duration = std::time::Duration::from_millis(1500);

/// Top-level UI view selector. Toggled with Ctrl+Shift+/.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Chat,
    Agents,
}

impl Default for View {
    fn default() -> Self {
        View::Chat
    }
}

#[derive(Debug, Default, Clone)]
pub struct AgentsViewState {
    pub snapshot: Option<Vec<crate::session_registry::SessionInfo>>,
    pub focused_sid: Option<agent_client_protocol::SessionId>,
    pub refetch_in_flight: bool,
    pub dirty: bool,
    pub next_request_id: u64,
    pub latest_request_id: Option<u64>,
    /// Set by F5 in the session view to request a master-side disk re-scan
    /// (`load_for_cli`) on the next dispatched `sessions/list`. Sticky across
    /// in-flight coalescing: only cleared when a request is actually built, so
    /// an F5 pressed while a poll is in flight still re-scans on the trailing
    /// refetch. Reset on view close.
    pub pending_rescan: bool,
    /// True while an F5 rescan request is in flight (set when dispatched,
    /// cleared when the response/failure lands). Drives the loading shimmer for
    /// the whole refresh so F5 has visible feedback even when the list already
    /// has rows — a normal 5s poll leaves it false and never flashes loading.
    pub rescan_in_flight: bool,
}

// (Historical-session load-state tracking was removed: the helper no longer
// scans on-disk history; the session view renders from master's `session/list`
// snapshot. See doc/specs/per-cli-history-filtering.md.)

/// Reverse of `CliSource::from_agent_id` — yields the lowercase CLI id
/// used by the command-synthesis template and dispatch routing.
/// Returns `None` for `CliSource::Unknown(_)` so each call-site retains
/// its current Unknown-handling semantics (display fallback / bool
/// false / early return — they differ).
pub(crate) fn known_cli_id(src: &crate::agent_sessions::CliSource) -> Option<&'static str> {
    use crate::agent_sessions::CliSource;
    match src {
        CliSource::Claude  => Some("claude"),
        CliSource::Codex   => Some("codex"),
        CliSource::Copilot => Some("copilot"),
        CliSource::Gemini  => Some("gemini"),
        CliSource::Unknown(_) => None,
    }
}

pub(crate) fn session_info_to_agent_session(
    info: &crate::session_registry::SessionInfo,
) -> crate::agent_sessions::AgentSession {
    use crate::agent_sessions::{AgentSession, AgentStatus, CliSource, SessionOrigin};
    let status = info.status.clone().unwrap_or(AgentStatus::Historical);
    let origin = info.origin.clone().unwrap_or(SessionOrigin::Unknown);
    let last_activity_at = info
        .last_activity_at_ms
        .map(|ms| std::time::UNIX_EPOCH + std::time::Duration::from_millis(ms))
        .unwrap_or_else(std::time::SystemTime::now);
    AgentSession {
        key: info.session_id.0.to_string(),
        cli_source: info.cli_source.clone().unwrap_or(CliSource::Unknown(String::new())),
        pane_session_id: info.pane_session_id.clone(),
        window_id: None,
        tab_id: None,
        title: info.title.clone().unwrap_or_else(|| "—".to_string()),
        cwd: info.cwd.clone(),
        started_at: last_activity_at,
        last_activity_at,
        status,
        last_error: info.last_error.clone(),
        current_tool: info.current_tool.clone(),
        attention_reason: info.attention_reason.clone(),
        log_path: None,
        origin,
        location: info.location.clone(),
    }
}

impl App {
    pub fn new(
        prompt_tx: mpsc::UnboundedSender<PromptSubmission>,
        recommendation_tx: mpsc::UnboundedSender<crate::coordinator::ChoiceExecution>,
        permission_tx: mpsc::UnboundedSender<String>,
        cancel_tx: mpsc::UnboundedSender<CancelRequest>,
        new_session_tx: mpsc::UnboundedSender<NewSessionForTab>,
        load_session_tx: mpsc::UnboundedSender<LoadSessionForTab>,
        drop_session_tx: mpsc::UnboundedSender<DropSessionRequest>,
        rename_session_tx: mpsc::UnboundedSender<RenameSessionRequest>,
        restart_tx: mpsc::UnboundedSender<RestartRequest>,
        master_request_tx: mpsc::UnboundedSender<crate::protocol::acp::client::MasterExtRequest>,
        debug_capture_enabled: Arc<AtomicBool>,
        wt_connected: bool,
        autofix_enabled: bool,
        shell_mgr: Arc<crate::shell::ShellManager>,
    ) -> Self {
        let mut tab_sessions = HashMap::new();
        tab_sessions.insert(DEFAULT_TAB_ID.to_string(), TabSession::default());
        Self {
            mode: AppMode::Chat,
            setup: None,
            auth: None,
            event_tx: None,
            pending_acp_start: false,
            needs_post_login_authenticate: false,
            auth_recovery_generation: 0,
            pending_agent_selection: None,
            show_welcome_hint: false,
            deferred_acp: None,
            state: ConnectionState::Connecting(t!("connection.starting").into_owned()),
            current_agent_id: String::new(),
            preflight_setup_active: false,
            agent_name: String::new(),
            agent_model: None,
            agent_version: None,
            available_models: Vec::new(),
            current_model_id: None,
            prompt_name: None,
            session_id: String::new(),
            wt_connected,
            terminal_rows: 24,
            terminal_cols: 80,
            pane_focused: true,
            should_quit: false,
            prompt_tx,
            recommendation_tx,
            permission_tx,
            cancel_tx,
            new_session_tx,
            load_session_tx,
            drop_session_tx,
            rename_session_tx,
            restart_tx,
            master_request_tx,
            debug_capture_enabled,
            help_overlay_visible: false,
            transport_lost: false,
            debug_messages: Vec::new(),
            show_debug_panel: false,
            debug_scroll: 0,
            pane_id: None,
            tab_id: None,
            owner_tab_id: None,
            window_id: None,
            wt_notifications: VecDeque::new(),
            show_notification_banner: false,
            autofix_enabled,
            tab_sessions,
            session_to_tab: HashMap::new(),
            agent_sessions: crate::agent_sessions::AgentSessionRegistry::new(),
            agent_supports_load_session: false,
            agent_supports_image: false,
            sessions_origin_filter: resolve_sessions_origin_filter(),
            install_request_tx: None,
            agent_event_tx: None,
            session_hook_tx: None,
            delegate_agents: None,
            delegate_base_agent_cmd: String::new(),
            acp_model: None,
            #[cfg(test)]
            last_dispatched_command: None,
            source_session_id: None,
            source_cwd: None,
            log_agent_events: false,
            activity_frame: 0,
            close_pane_armed_at: None,
            transient_hint: None,
            alive: crate::session_registry::InMemoryRegistry::shared(),
            alive_loaded: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            shell_mgr,
        }
    }

    /// Stash pipe-mode launch parameters on App so that a post-FRE-login
    /// reconnect via [`Self::try_start_acp`] goes back through
    /// `run_acp_client_over_pipe` (talking to wta-master).
    ///
    /// The bug this guards against: in helper mode (`--connect-master`),
    /// the initial `run_acp_client_over_pipe` task fails immediately with
    /// `Authentication required` if the user is in FRE / not yet logged
    /// in. The helper falls into the setup screen, the user logs in, and
    /// `LoginComplete` fires `try_start_acp`. Without this pre-stash,
    /// `LoginComplete` finds `deferred_acp.is_none()` and `try_start_acp`
    /// has no pipe name to reconnect with — the agent pane never comes
    /// back. With it, `try_start_acp` reuses the stashed pipe name to
    /// re-attach to master.
    ///
    /// All `_rx` fields are seeded `None`; `try_start_acp` creates fresh
    /// channels on reconnect and re-binds the `_tx` halves on App, plus
    /// re-creates the `session_hook` channel and re-binds
    /// `self.session_hook_tx`.
    pub fn set_master_pipe_acp_params(
        &mut self,
        pipe_name: String,
        agent_cmd: String,
        acp_model: Option<String>,
        owner_tab_id: Option<String>,
        shell_mgr: Arc<crate::shell::ShellManager>,
        wt_connected: bool,
    ) {
        self.deferred_acp = Some(DeferredAcpParams {
            agent_cmd,
            acp_model,
            prompt_rx: None,
            cancel_rx: None,
            new_session_rx: None,
            load_session_rx: None,
            drop_session_rx: None,
            rename_session_rx: None,
            restart_rx: None,
            master_ext_rx: None,
            shell_mgr,
            wt_connected,
            master_pipe_name: Some(pipe_name),
            owner_tab_id,
        });
    }

    /// Try to start the ACP client if login just completed.
    /// Creates fresh channels if previous ones were consumed by a failed attempt.
    ///
    /// **Pipe-mode branch.** When `deferred_acp.master_pipe_name.is_some()`
    /// (set at boot by [`Self::set_master_pipe_acp_params`] in helper
    /// mode), we route the reconnect through
    /// [`run_acp_client_over_pipe`] so the rebuilt helper talks to the
    /// shared wta-master singleton — same as the cold-boot helper path.
    /// We also rebuild the `session_hook` channel and re-bind the `_tx`
    /// half on `self.session_hook_tx`, because the original receiver was
    /// consumed (and dropped) by the dead initial pipe-mode task.
    ///
    /// **No-pipe branch.** When `master_pipe_name.is_none()` we surface a
    /// defensive `AgentError` rather than starting an agent: direct-agent
    /// mode was removed, so wta only runs as a wta-master-attached helper
    /// and a missing pipe here means a wiring bug.
    pub fn try_start_acp(&mut self) {
        if !self.pending_acp_start {
            return;
        }
        self.pending_acp_start = false;
        let post_login_auth = self.needs_post_login_authenticate;
        self.needs_post_login_authenticate = false;
        tracing::info!(target: "acp", has_event_tx = self.event_tx.is_some(), has_deferred = self.deferred_acp.is_some(), post_login_auth, "try_start_acp triggered");

        if let (Some(ref tx), Some(ref mut params)) = (&self.event_tx, &mut self.deferred_acp) {
            // If channels were consumed by a previous (failed) attempt, create fresh ones.
            // Also update all sender fields on self so the App routes to the new ACP client.
            if params.prompt_rx.is_none() {
                let (ptx, prx) = mpsc::unbounded_channel();
                let (ctx, crx) = mpsc::unbounded_channel();
                let (ntx, nrx) = mpsc::unbounded_channel();
                let (ltx, lrx) = mpsc::unbounded_channel();
                let (dtx, drx) = mpsc::unbounded_channel();
                let (rntx, rnrx) = mpsc::unbounded_channel();
                let (rtx, rrx) = mpsc::unbounded_channel();
                let (mtx, mrx) = mpsc::unbounded_channel();
                self.prompt_tx = ptx;
                self.cancel_tx = ctx;
                self.new_session_tx = ntx;
                self.load_session_tx = ltx;
                self.drop_session_tx = dtx;
                self.rename_session_tx = rntx;
                self.restart_tx = rtx;
                self.master_request_tx = mtx;
                params.prompt_rx = Some(prx);
                params.cancel_rx = Some(crx);
                params.new_session_rx = Some(nrx);
                params.load_session_rx = Some(lrx);
                params.drop_session_rx = Some(drx);
                params.rename_session_rx = Some(rnrx);
                params.restart_rx = Some(rrx);
                params.master_ext_rx = Some(mrx);
            }

            if let (
                Some(prompt_rx),
                Some(cancel_rx),
                Some(new_session_rx),
                Some(load_session_rx),
                Some(drop_session_rx),
                Some(rename_session_rx),
                Some(restart_rx),
                Some(master_ext_rx),
            ) = (
                params.prompt_rx.take(),
                params.cancel_rx.take(),
                params.new_session_rx.take(),
                params.load_session_rx.take(),
                params.drop_session_rx.take(),
                params.rename_session_rx.take(),
                params.restart_rx.take(),
                params.master_ext_rx.take(),
            ) {
                let acp_model = params.acp_model.clone();
                let event_tx = tx.clone();
                let shell_mgr = Arc::clone(&params.shell_mgr);
                let wt_connected = params.wt_connected;
                let pipe_name_opt = params.master_pipe_name.clone();
                let owner_tab_opt = params.owner_tab_id.clone();

                if let Some(pipe_name) = pipe_name_opt {
                    // Pipe-mode reconnect (helper after FRE login).
                    // Rebuild the session_hook channel — the original
                    // rx was consumed and dropped with the dead initial
                    // task, leaving `self.session_hook_tx` pointing at a
                    // closed channel (every `publish_session_hook` call
                    // logs "channel closed"). Reinstall a live tx so
                    // hooks reach master again, and hand the matching
                    // rx to the new pipe-mode task.
                    let (shtx, shrx) = mpsc::unbounded_channel();
                    self.session_hook_tx = Some(shtx);
                    tracing::info!(
                        target: "acp",
                        pipe = %pipe_name,
                        "try_start_acp: reconnecting via master pipe"
                    );
                    // Captured for post-login auth recovery: who failed (agent)
                    // and which tab, so a still-auth-failing post-login
                    // reconnect can request a fresh master targeting that tab.
                    // Taken before `owner_tab_opt` is moved into the client.
                    let recovery_tab_id = owner_tab_opt.clone();
                    let recovery_agent_id = self.current_agent_id.clone();
                    let event_tx_for_pipe = event_tx.clone();
                    tokio::task::spawn_local(async move {
                        if let Err(e) =
                            crate::protocol::acp::client::run_acp_client_over_pipe(
                                pipe_name,
                                acp_model,
                                owner_tab_opt,
                                None, // initial_load_session_id: already handled by the dead initial task
                                event_tx_for_pipe.clone(),
                                prompt_rx,
                                cancel_rx,
                                new_session_rx,
                                load_session_rx,
                                drop_session_rx,
                                rename_session_rx,
                                restart_rx,
                                shrx,
                                master_ext_rx,
                                shell_mgr,
                                wt_connected,
                                post_login_auth, // only true on genuine LoginComplete reconnects
                            )
                            .await
                        {
                            tracing::error!(
                                target: "helper",
                                error = %e,
                                "run_acp_client_over_pipe failed on reconnect"
                            );
                            let failure = crate::protocol::acp::failure::classify_anyhow(
                                &e,
                                crate::protocol::acp::failure::HandshakeStage::Initialize,
                            );
                            // A post-login reconnect for an External-auth agent
                            // that STILL fails auth means the long-lived shared
                            // master CLI is poisoned and `authenticate` won't
                            // refresh it. Request a fresh master (auth recovery)
                            // instead of looping back to the sign-in screen.
                            // Match BOTH the plain AuthRequired and the post-
                            // login HandshakeFailed{NewSession} the client
                            // wraps a still-AuthRequired new_session into.
                            let is_external = matches!(
                                crate::agent_registry::lookup_profile_by_id(&recovery_agent_id)
                                    .acp_auth_flow,
                                crate::agent_registry::AcpAuthFlow::External
                            );
                            if post_login_auth
                                && is_external
                                && is_post_login_auth_failure(&failure)
                            {
                                tracing::warn!(
                                    target: "auth_recovery",
                                    agent_id = %recovery_agent_id,
                                    tab_id = ?recovery_tab_id,
                                    "post-login reconnect still auth-failing on shared master CLI; requesting auth recovery"
                                );
                                let _ = event_tx_for_pipe.send(AppEvent::PostLoginAuthRecovery {
                                    failure,
                                    tab_id: recovery_tab_id.clone(),
                                    agent_id: recovery_agent_id.clone(),
                                });
                            } else {
                                let _ = event_tx_for_pipe.send(AppEvent::AgentError {
                                    session_id: None,
                                    failure,
                                    message: format!(
                                        "helper ACP transport failed on reconnect: {e:#}"
                                    ),
                                });
                            }
                        }
                    });
                } else {
                    // Unreachable in the shipped product: wta only runs as a
                    // wta-master-attached helper, so deferred reconnect params
                    // always carry a master pipe name. Direct-agent mode was
                    // removed; surface a clear error rather than panicking if
                    // we somehow reach here with no pipe.
                    tracing::error!(
                        target: "acp",
                        "try_start_acp: no master pipe in deferred params — \
                         direct-agent mode was removed; cannot start ACP client"
                    );
                    let _ = event_tx.send(AppEvent::AgentError {
                        session_id: None,
                        failure: crate::protocol::acp::failure::AgentFailure::HandshakeFailed {
                            stage: crate::protocol::acp::failure::HandshakeStage::Initialize,
                            detail: "missing wta-master connection".to_string(),
                        },
                        message: "Agent pane could not start: missing wta-master \
                                  connection (direct mode is no longer supported)."
                            .to_string(),
                    });
                }
            }
        }
    }

    /// Wire a sender that signals main.rs to run the agent-hooks installer
    /// (Settings UI -> Install button -> main.rs spawns
    /// `agent_hooks_installer::ensure_installed`).
    pub fn set_install_request_tx(&mut self, tx: mpsc::UnboundedSender<()>) {
        self.install_request_tx = Some(tx);
    }

    /// Wire the main loop's `AppEvent` sender so background callbacks
    /// (e.g. `dispatch_resume`'s split-pane completion) can post
    /// `AgentSessionEvent`s back into the event loop instead of needing
    /// shared mutable access to `agent_sessions`.
    pub fn set_agent_event_tx(&mut self, tx: mpsc::UnboundedSender<AppEvent>) {
        self.agent_event_tx = Some(tx);
    }

    pub fn set_session_hook_tx(
        &mut self,
        tx: mpsc::UnboundedSender<crate::agent_sessions::SessionEvent>,
    ) {
        self.session_hook_tx = Some(tx);
    }

    /// Seed the hot-updatable runtime agent config: the delegate runtime
    /// table shared with the recommendation executor, the helper's own
    /// agent cmdline (used to re-derive the delegate commandline on partial
    /// updates), and the configured acp-model override.
    pub fn set_runtime_agent_config(
        &mut self,
        delegate_agents: Arc<std::sync::Mutex<Vec<crate::coordinator::DelegateAgentRuntime>>>,
        base_agent_cmd: String,
        acp_model: Option<String>,
    ) {
        self.delegate_agents = Some(delegate_agents);
        self.delegate_base_agent_cmd = base_agent_cmd;
        self.acp_model = acp_model.filter(|s| !s.trim().is_empty());
    }

    /// Low-level: ask the ACP client task to apply `model` via
    /// `set_session_model`. `session_id == Some` targets exactly that live
    /// session (the per-pane `/model` pick); `None` fans out to every session
    /// this helper owns. No-op on an empty/whitespace model — an empty
    /// override means "agent default", which `set_session_model` can't
    /// express.
    fn send_session_model(&self, session_id: Option<String>, model: String) {
        if model.trim().is_empty() {
            return;
        }
        let _ = self.master_request_tx.send(
            crate::protocol::acp::client::MasterExtRequest::SetSessionModel {
                session_id: session_id.map(agent_client_protocol::SessionId::new),
                model,
            },
        );
    }

    /// The model a given tab should run on: its explicit per-pane override
    /// (set via `/model`) wins, else the global `acpModel`. `None` means no
    /// opinion — leave the session on the agent's default.
    fn effective_model_for_tab(&self, tab_key: &str) -> Option<String> {
        self.tab_sessions
            .get(tab_key)
            .and_then(|t| t.model_override.clone())
            .or_else(|| self.acp_model.clone())
            .filter(|s| !s.trim().is_empty())
    }

    /// Push the global `acpModel` to *every* tab's live session. A global
    /// settings change is authoritative — it overrides per-pane `/model`
    /// picks too (see `apply_global_acp_model`, which clears the overrides
    /// first), so this no longer skips overridden tabs.
    fn send_acp_model_update(&self) {
        let Some(model) = self.acp_model.as_ref().filter(|s| !s.trim().is_empty()) else {
            return;
        };
        for tab in self.tab_sessions.values() {
            if let Some(sid) = tab.session_id.clone() {
                self.send_session_model(Some(sid), model.clone());
            }
        }
    }

    /// Apply a global `acpModel` settings change. This is authoritative over
    /// per-pane `/model` picks: it
    ///   1. clears every tab's local override (so all panes — now and on
    ///      their next `/new` session — follow the new global model),
    ///   2. points the shared current-model display at the new value so the
    ///      title bar / settings dropdown / `/model` row update on every pane,
    ///   3. pushes the model to every live session, and
    ///   4. republishes agent status.
    /// An empty value means "agent default": overrides still clear and the
    /// sessions fall back on their next attach, but we send nothing (the
    /// default can't be expressed as `set_session_model`).
    fn apply_global_acp_model(&mut self, new_model: Option<String>) {
        self.acp_model = new_model.filter(|s| !s.trim().is_empty());
        for tab in self.tab_sessions.values_mut() {
            tab.model_override = None;
        }
        if self.acp_model.is_some() {
            self.current_model_id = self.acp_model.clone();
        }
        self.send_acp_model_update();
        self.publish_agent_status();
    }

    // ── /model picker ───────────────────────────────────────────────────

    /// True while the model picker modal is up for the active tab.
    fn model_picker_visible(&self) -> bool {
        self.current_tab().model_picker_open
    }

    /// `/model [id]` — switch this pane's model. With an argument, match it
    /// against the agent's advertised list and apply directly; bare `/model`
    /// opens the interactive picker.
    fn cmd_model(&mut self, arg: String) {
        let arg = arg.trim().to_string();
        if self.available_models.is_empty() {
            let tab = self.current_tab_mut();
            tab.messages
                .push(ChatMessage::System(t!("system.no_models").into_owned()));
            tab.scroll_to_bottom();
            return;
        }
        if arg.is_empty() {
            self.open_model_picker();
            return;
        }
        // Direct switch: exact id first, then case-insensitive id/name.
        let matched = self
            .available_models
            .iter()
            .find(|m| m.id == arg)
            .or_else(|| {
                self.available_models
                    .iter()
                    .find(|m| m.id.eq_ignore_ascii_case(&arg) || m.name.eq_ignore_ascii_case(&arg))
            })
            .map(|m| m.id.clone());
        match matched {
            Some(id) => self.apply_model_pick(id),
            None => {
                let tab = self.current_tab_mut();
                tab.messages.push(ChatMessage::System(
                    t!("system.model_unknown", model = arg.as_str()).into_owned(),
                ));
                tab.scroll_to_bottom();
            }
        }
    }

    /// Open the picker on the active tab, pre-selecting the model the pane is
    /// currently effectively on (so Enter is a confirm and arrows move
    /// relative to "here"). Mirrors `current_model_display`'s precedence:
    /// per-pane override, then the agent's reported `current_model_id`, then
    /// the global `acpModel` (so a pane following the global value preselects
    /// it before the agent reports `current_model_id`).
    fn open_model_picker(&mut self) {
        if self.available_models.is_empty() {
            return;
        }
        let current = self
            .current_tab()
            .model_override
            .clone()
            .or_else(|| self.current_model_id.clone())
            .or_else(|| self.acp_model.clone());
        let selected = current
            .and_then(|cur| self.available_models.iter().position(|m| m.id == cur))
            .unwrap_or(0);
        let tab = self.current_tab_mut();
        tab.model_picker_open = true;
        tab.model_picker_selected = selected;
    }

    fn close_model_picker(&mut self) {
        self.current_tab_mut().model_picker_open = false;
    }

    fn model_picker_up(&mut self) {
        let tab = self.current_tab_mut();
        if tab.model_picker_selected > 0 {
            tab.model_picker_selected -= 1;
        }
    }

    fn model_picker_down(&mut self) {
        // `saturating_sub` keeps this safe if the model list is empty while
        // the picker is somehow open (len 0 -> last index clamps to 0).
        let last = self.available_models.len().saturating_sub(1);
        let tab = self.current_tab_mut();
        if tab.model_picker_selected < last {
            tab.model_picker_selected += 1;
        }
    }

    /// Commit the highlighted row in the open picker.
    fn commit_model_pick(&mut self) {
        let idx = self.current_tab().model_picker_selected;
        let id = self.available_models.get(idx).map(|m| m.id.clone());
        self.close_model_picker();
        if let Some(id) = id {
            self.apply_model_pick(id);
        }
    }

    /// Pin the active pane to `model_id`: record the per-pane override, mirror
    /// it into the status projection (title bar / settings dropdown), and
    /// hot-apply it to the tab's live session. Shared by the picker (Enter)
    /// and `/model <id>`. If no session is live yet, the override is stored
    /// and `SessionAttached` applies it via `effective_model_for_tab`.
    fn apply_model_pick(&mut self, model_id: String) {
        let name = self
            .available_models
            .iter()
            .find(|m| m.id == model_id)
            .map(|m| m.name.clone())
            .unwrap_or_else(|| model_id.clone());
        let session_id = {
            let tab = self.current_tab_mut();
            tab.model_override = Some(model_id.clone());
            tab.messages.push(ChatMessage::System(
                t!("system.model_set", model = name.as_str()).into_owned(),
            ));
            tab.scroll_to_bottom();
            tab.session_id.clone()
        };
        self.current_model_id = Some(model_id.clone());
        if let Some(sid) = session_id {
            self.send_session_model(Some(sid), model_id);
        }
        self.publish_agent_status();
    }

    /// Rebuild the shared delegate runtime table from a settings change.
    /// `delegate_agent` / `delegate_model` are the new effective values
    /// (empty string = unset → fall back to deriving from the base agent
    /// cmd). No-op when no executor is wired (tests / manual runs).
    fn apply_delegate_config(&self, delegate_agent: &str, delegate_model: &str) {
        let Some(shared) = &self.delegate_agents else {
            return;
        };
        // Treat whitespace-only values as unset so the fallback-to-derived
        // path kicks in (matches the acp_model handling in handle_event).
        let runtimes = crate::coordinator::default_delegate_agent_runtimes(
            Some(delegate_agent).filter(|s| !s.trim().is_empty()),
            Some(self.delegate_base_agent_cmd.as_str()),
            Some(delegate_model).filter(|s| !s.trim().is_empty()),
        );
        *shared.lock().unwrap() = runtimes;
        tracing::info!(
            target: "autofix",
            delegate_agent,
            delegate_model,
            "delegate config hot-updated from settings change"
        );
    }

    fn publish_session_hook(&self, event: crate::agent_sessions::SessionEvent) {
        if let Some(tx) = &self.session_hook_tx {
            if let Err(err) = tx.send(event) {
                tracing::warn!(
                    target: "session_hook",
                    error = %err,
                    "failed to queue session_hook event for master"
                );
            }
        }
    }

    /// Trigger an install-hooks request. No-op if no channel is wired
    /// (e.g. running outside the packaged app).
    #[allow(dead_code)]
    pub fn request_install_hooks(&self) {
        if let Some(tx) = &self.install_request_tx {
            let _ = tx.send(());
        }
    }

    /// Filter to apply to the session management view based on which
    /// agent CLI the WTA agent pane is currently driving. Returns
    /// `Some(CliSource::*)` when `current_agent_id` resolves to a tracked
    /// CLI (copilot / claude / gemini) so only matching rows are listed.
    /// Returns `None` when no agent has been selected yet or the agent is
    /// not one the session registry tracks (codex / unknown) — in that
    /// case the view falls back to showing every row so the user can still
    /// see and resume their history.
    pub fn current_cli_filter(&self) -> Option<crate::agent_sessions::CliSource> {
        crate::agent_sessions::CliSource::from_agent_id(&self.current_agent_id)
    }

    /// Extracted focus-pane dispatch for Live rows. Shared between the
    /// legacy [`Self::activate_agent_session`] and the new
    /// [`Self::activate_agent_session_with_shift`] dispatcher.
    ///
    /// Behavior:
    ///   * No-op if the row's pane GUID matches our own
    ///     (`self.pane_id`) — focusing yourself races WT teardown.
    ///   * Otherwise spawns `wtcli focus-pane -t <pane>` on a background
    ///     thread, wiring `FocusPaneFailureReason::NotFound` failures
    ///     back through `AgentSessionEvent::PaneClosed` so a row whose
    ///     pane died silently transitions to Ended instead of staying
    ///     stuck.
    fn dispatch_focus_pane(&mut self, pane: &str, log_key: &str) {
        let is_self = self
            .pane_id
            .as_deref()
            .map(|own| own.eq_ignore_ascii_case(pane))
            .unwrap_or(false);
        if is_self {
            tracing::info!(
                target: "agents_view",
                key = %log_key,
                pane = %pane,
                "skipping session_focus: row points at our own pane",
            );
            return;
        }
        tracing::info!(target: "agents_view", key = %log_key, pane = %pane, "session_focus RPC scheduled");
        self.dispatch_session_focus_rpc(log_key);
    }

    /// B-10: state-machine-driven Enter / Shift+Enter dispatcher.
    ///
    /// Routes through [`crate::session_mgmt::decide_enter_action`] —
    /// the pure-function core that closed-form maps
    /// `(origin, liveness, cli, capabilities, shift)` to one of
    /// `Focus | ResumeInAgentPane | ResumeCliFlag | NotResumable`.
    /// All side effects (system messages, wtcli spawn, optimistic
    /// state flips) live on the dispatch side here
    /// or in the existing [`Self::dispatch_resume`] /
    /// [`Self::dispatch_resume_in_agent_pane`] helpers we call into.
    ///
    /// Why this matters: today the Enter / Shift+Enter branches in the
    /// key handler bake the routing rules inline (Shift on
    /// Ended/Historical → resume_in_agent_pane; else → legacy
    /// activate). That branch was correct for Class B (Unknown
    /// origin) but flipped for Class A (AgentPane origin) — for a
    /// session WE started in an agent pane, the natural Enter target
    /// is the *same* agent pane (via ACP `session/load`), and the
    /// escape hatch is the CLI `--resume` flag. This dispatcher
    /// honors the per-origin default and treats Shift as "flip the
    /// default".
    ///
    /// Live rows are unaffected: Shift on a Live row is the same as
    /// Enter (agents forbid two clients on one session, so any
    /// "force second copy" attempt would just error).
    fn activate_agent_session_with_shift(
        &mut self,
        s: &crate::agent_sessions::AgentSession,
        shift: bool,
    ) {
        use crate::session_mgmt::{
            decide_enter_action, liveness_from_status, EnterAction, NotResumableReason, RowSnapshot,
        };
        // WSL rows can only resume via the CLI `--resume` flag *inside*
        // the distro. ACP `session/load` (the Shift target for Class B
        // dead rows) can't rehydrate a Linux session into a host agent
        // pane, so collapse Shift to Enter — both route to ResumeCliFlag.
        let shift = shift && !s.location.is_wsl();
        // Ambient: load_session capability is set during ACP init;
        // resume-flag support is a per-CLI profile constant — true for
        // Claude / Codex / Copilot / Gemini (all four CLIs accept some
        // form of `--resume`/`resume <id>` re-attach surface).
        let cli_supports_resume_flag = match known_cli_id(&s.cli_source) {
            Some(id) => !crate::agent_registry::lookup_profile_by_id(id)
                .resume_flag
                .is_empty(),
            None => false,
        };
        let row = RowSnapshot {
            origin: s.origin.clone(),
            liveness: liveness_from_status(&s.status, s.pane_session_id.clone()),
            key: s.key.clone(),
            cli_source: s.cli_source.clone(),
            load_session_supported: self.agent_supports_load_session,
            cli_supports_resume_flag,
        };
        let action = decide_enter_action(&row, shift);

        tracing::info!(
            target: "agents_view",
            key = %s.key,
            status = ?s.status,
            origin = ?s.origin,
            pane_session_id = ?s.pane_session_id,
            cli = ?s.cli_source,
            shift = shift,
            action = ?action,
            "activate_agent_session_with_shift: decided action",
        );

        match action {
            EnterAction::Focus { pane_session_id } => {
                self.dispatch_focus_pane(&pane_session_id, &s.key);
            }
            EnterAction::ResumeInAgentPane { .. } => {
                // dispatch_resume_in_agent_pane owns the loadSession
                // capability gate (also re-checked),
                // optimistic ResumeDispatched, and emit
                // resume_in_new_agent_tab to WT.
                self.dispatch_resume_in_agent_pane(s);
            }
            EnterAction::ResumeCliFlag { .. } => {
                // dispatch_resume owns the resume-flag check,
                // optimistic ResumeDispatched, and new-tab spawn.
                self.dispatch_resume(s);
            }
            EnterAction::NotResumable { reason } => {
                // Surface a user-visible system message scoped to the
                // current tab so the user can read it from the
                // agent session view (which is rendered in-tab).
                let agent_display: String = match known_cli_id(&s.cli_source) {
                    Some(id) => crate::agent_registry::lookup_profile_by_id(id)
                        .display_name
                        .to_string(),
                    None => t!("system.fallback.this_agent").into_owned(),
                };
                let msg = match reason {
                    NotResumableReason::LiveWithoutPane => {
                        t!("system.cannot_focus_session", session_id = s.key.as_str())
                            .into_owned()
                    }
                    NotResumableReason::LoadSessionNotSupported => {
                        let agent: String = if self.agent_name.is_empty() {
                            t!("system.fallback.connected_agent").into_owned()
                        } else {
                            self.agent_name.clone()
                        };
                        t!(
                            "system.cannot_resume_no_load_session",
                            agent = agent.as_str()
                        )
                        .into_owned()
                    }
                    NotResumableReason::CliHasNoResumeFlag => t!(
                        "system.cannot_resume_no_resume_flag",
                        agent = agent_display.as_str()
                    )
                    .into_owned(),
                    NotResumableReason::UnknownCli => t!(
                        "system.cannot_resume_unknown_agent",
                        session_id = s.key.as_str()
                    )
                    .into_owned(),
                };
                tracing::warn!(
                    target: "agents_view",
                    key = %s.key,
                    reason = ?reason,
                    "activate_agent_session_with_shift: not resumable",
                );
                let tab = self.current_tab_mut();
                tab.messages.push(ChatMessage::System(msg));
                tab.scroll_to_bottom();
                #[cfg(test)]
                {
                    self.last_dispatched_command = Some(DispatchedCommand {
                        kind: DispatchedCommandKind::NotResumable,
                        session_id: Some(s.key.clone()),
                        argv: vec!["not-resumable".to_string(), format!("{:?}", reason)],
                    });
                }
            }
        }
    }

    /// Enter handler for the agent session view. For live rows (Idle / Working
    /// / Attention / Error), focus the underlying WT pane. For terminal-
    /// state rows (Ended / Historical), spawn a new pane that runs the
    /// CLI's `--resume <session_id>` flow via [`Self::dispatch_resume`].
    fn activate_agent_session(&mut self, s: &crate::agent_sessions::AgentSession) {
        use crate::agent_sessions::AgentStatus::*;
        tracing::info!(
            target: "agents_view",
            key = %s.key,
            status = ?s.status,
            pane_session_id = ?s.pane_session_id,
            cli = ?s.cli_source,
            "activate_agent_session: Enter on row",
        );
        match s.status {
            Idle | Working | Attention | Error => {
                if let Some(pane) = &s.pane_session_id {
                    // Skip self-focus: if the user pressed Enter on the
                    // row that represents the pane this WTA is already
                    // running in, the focus call is a no-op for them and
                    // can throw `winrt::hresult_error` (E_FAIL /
                    // 0x80004005) on the WT side. Compare case-insensitively
                    // because pane GUIDs arrive in mixed case (hooks emit
                    // lowercase, WT-native events emit canonical
                    // uppercase) and `self.pane_id` is populated from
                    // whichever path discovered it first.
                    let is_self = self
                        .pane_id
                        .as_deref()
                        .map(|own| own.eq_ignore_ascii_case(pane.as_str()))
                        .unwrap_or(false);
                    if is_self {
                        tracing::info!(
                            target: "agents_view",
                            key = %s.key,
                            pane = %pane,
                            "skipping focus_pane: row points at our own pane",
                        );
                    } else {
                        self.dispatch_session_focus_rpc(&s.key);
                    }
                    #[cfg(test)]
                    {
                        self.last_dispatched_command = Some(DispatchedCommand {
                            kind: DispatchedCommandKind::FocusPane,
                            session_id: Some(s.key.clone()),
                            argv: vec![
                                "session_focus".to_string(),
                                "--sid".to_string(),
                                s.key.clone(),
                            ],
                        });
                    }
                } else {
                    tracing::warn!(
                        target: "agents_view",
                        key = %s.key,
                        "live row has no pane_session_id; Enter is a no-op",
                    );
                }
            }
            Ended | Historical => {
                self.dispatch_resume(s);
            }
        }
    }

    /// Open a new WT tab whose primary pane runs `<cli> <resume_flag>
    /// <session_key>` to rehydrate a Historical/Ended agent session from
    /// the CLI's on-disk session store. Silent no-op for CLIs without a
    /// resume flag or unknown CLI sources.
    ///
    /// Flow:
    ///   1. Apply `ResumeDispatched` synchronously so a rapid second Enter
    ///      on the same row no-ops while this resume is in flight.
    ///   2. Issue `wtcli --json new-tab -c "<cli> <flag> <key>" -d "<cwd>"`
    ///      on a background thread via
    ///      `spawn_wtcli_split_then_focus_with_callback` — the helper is
    ///      generic (parses `session_id` from JSON and focuses the new
    ///      pane), so it works equally well for new-tab and split-pane.
    ///      Routing through `new-tab` keeps the originating tab clean
    ///      and matches user expectation that resuming a historical
    ///      session is a "go open my session" action, not a "split my
    ///      workspace" action.
    ///   3. The callback posts `AgentSessionEvent(ResumePaneAssigned{...})`
    ///      through `agent_event_tx` so the registry can bind the new
    ///      tab's primary pane GUID to the row even for hook-less CLIs
    ///      (Gemini), allowing a later `PaneClosed` to transition the
    ///      row back to Ended.
    fn dispatch_resume(&mut self, s: &crate::agent_sessions::AgentSession) {
        let cli_id = match known_cli_id(&s.cli_source) {
            Some(id) => id,
            None => {
                tracing::debug!(
                    target: "agents_view",
                    key = %s.key,
                    "dispatch_resume: unknown cli_source, skipping",
                );
                return;
            }
        };
        let profile = crate::agent_registry::lookup_profile_by_id(cli_id);
        if profile.resume_flag.is_empty() {
            tracing::debug!(
                target: "agents_view",
                key = %s.key,
                cli = %cli_id,
                "dispatch_resume: CLI does not advertise a resume flag, skipping",
            );
            return;
        }

        let key = s.key.clone();
        let resume_invocation = format!("{} {} {}", cli_id, profile.resume_flag, key);
        // WSL rows run the distro's own CLI *inside* the distro. Two
        // WSL/cmd quirks shape this command line:
        //   * The distro name is **not** quoted. `wsl -d "Ubuntu"` fails with
        //     WSL_E_DISTRO_NOT_FOUND when the command runs under the
        //     `cmd /c echo … && …` banner wrapper — cmd/wsl don't strip the
        //     quotes off `-d`, so wsl looks for a distro literally named
        //     `"Ubuntu"`. Distro names from `wsl -l` are space-free, so bare
        //     `-d <distro>` is safe. The `--cd` path keeps its quotes (it can
        //     contain spaces and quoting works fine there).
        //   * The CLI is launched through a **login shell** (`bash -lc`) so the
        //     user's PATH is set up — a snap-installed Copilot lives in
        //     `/snap/bin`, which a bare `wsl -- copilot` misses ("command not
        //     found"). A login shell sources the profile that adds it.
        let login_invocation = format!("bash -lc \"{resume_invocation}\"");
        let commandline = match &s.location {
            crate::agent_sessions::SessionLocation::Wsl { distro } => match linux_cwd_arg(&s.cwd) {
                Some(cwd) => format!("wsl -d {distro} --cd \"{cwd}\" -- {login_invocation}"),
                None => format!("wsl -d {distro} -- {login_invocation}"),
            },
            crate::agent_sessions::SessionLocation::Host => resume_invocation,
        };

        // Per-CLI session stores are keyed by an encoding of the *current*
        // working directory (e.g. Claude looks under
        // `~/.claude/projects/<encoded-cwd>/<id>.jsonl`; Copilot and
        // Gemini behave similarly). Without the right cwd the CLI
        // reports `No conversation found with session ID: <id>` even
        // though the JSONL exists on disk.
        //
        // `wtcli new-tab` exposes `-d <cwd>` (see
        // `src/tools/wtcli/main.cpp:326-353` → COM `CreateTab(...,
        // startingDirectory, ...)`) so the new tab's primary pane
        // launches in the historical session's project root directly,
        // without needing a `cd /d` shell prefix.
        //
        // We still wrap the CLI invocation in `cmd /c` because
        // npm-installed CLIs (`copilot.cmd`, `claude.cmd`, `gemini.cmd`)
        // need cmd.exe's PATHEXT resolution to launch from a bare name
        // (`CreateProcess` returns 0x80070002 for `.cmd` shims).
        //
        // Loading banner (issue #135): the agent CLIs take 1–3s of
        // Node.js cold-start + JSONL history parse before they paint
        // anything, so the new tab was blank with no feedback. Prepend
        // a blinking ANSI banner (`SGR 1;36;5` = bold cyan slow-blink)
        // so the user sees immediate animated feedback in the new
        // pane while the CLI cold-starts. The CLI's alt-screen TUI
        // takes over once it boots and overwrites this line cleanly,
        // so the banner leaves no residue on success. On CLI launch
        // failure the banner stays put together with cmd.exe's error
        // message — that's a feature, not a bug (the short id helps
        // the user file a useful report). The trailing `\x1b[0m`
        // reset guarantees any post-failure output isn't tinted /
        // blinking.
        let raw_cwd_string = s.cwd.to_string_lossy().to_string();
        // Drop stale cwd so wtcli falls back to the profile default
        // rather than failing CreateProcessW with ERROR_DIRECTORY.
        // WSL rows use `wsl --cd` inside the distro command; passing
        // the Linux path as a Windows `-d` flag to wtcli would fail.
        let valid_cwd = if s.location.is_wsl() {
            None
        } else {
            crate::cwd_util::validate_starting_directory(&s.cwd)
        };
        if valid_cwd.is_none() && !raw_cwd_string.is_empty() {
            tracing::warn!(
                target: "agents_view",
                key = %key,
                "dispatch_resume: stored cwd is no longer a valid directory; falling back to profile default",
            );
        }
        let short_key: String = key.chars().take(8).collect();
        // Loading banner shown in the new pane while the CLI cold-starts.
        // WSL rows also name the distro ("Resuming copilot session abc-123
        // in Ubuntu (WSL)...") so the user can see which distro is being
        // entered; host rows keep just the short session id. A WSL session
        // only appears in the list because its distro was already started and
        // scanned, so it is running at resume time — a "starting the distro…"
        // hint would usually be wrong. (WSL2 can auto-shut-down an idle distro
        // later, but a frequently-wrong hint is worse than none.)
        let banner = match &s.location {
            crate::agent_sessions::SessionLocation::Wsl { distro } => {
                format!("Resuming {cli_id} session {short_key} in {distro} (WSL)...")
            }
            crate::agent_sessions::SessionLocation::Host => {
                format!("Resuming {cli_id} session {short_key}...")
            }
        };
        let launch_commandline =
            format!("cmd /c echo \x1b[2;37m{banner}\x1b[0m && {commandline}");
        let mut argv = vec![
            "new-tab".to_string(),
            "-c".to_string(),
            launch_commandline.clone(),
        ];
        if let Some(ref cwd) = valid_cwd {
            argv.push("-d".to_string());
            argv.push(cwd.clone());
        }
        // Optimistic state flip: bump Historical/Ended -> Idle so a rapid
        // second Enter on the same row sees a non-terminal status and
        // skips this branch (idempotent: ResumeDispatched no-ops on live
        // rows). See `agent_sessions::SessionEvent::ResumeDispatched`.
        let resume_event = crate::agent_sessions::SessionEvent::ResumeDispatched { key: key.clone() };
        self.agent_sessions.apply(resume_event.clone());
        self.publish_session_hook(resume_event);
        self.dispatch_session_resume_dispatched_rpc(&key);
        // for hook-less CLIs (Gemini) so a future `PaneClosed` can
        // transition the row to Ended; harmless duplicate work for
        // Claude/Copilot whose hooks beat us to the same binding.
        // `wtcli new-tab --json` emits a `session_id` field on the new
        // tab's primary pane in the same shape as `split-pane --json`,
        // so the existing helper handles both.
        let cb_key = key.clone();
        let event_tx = self.agent_event_tx.clone();
        let on_pane_id: Option<Box<dyn FnOnce(String) + Send + 'static>> = match event_tx {
            Some(tx) => Some(Box::new(move |pane_session_id| {
                let _ = tx.send(AppEvent::AgentSessionEvent(
                    crate::agent_sessions::SessionEvent::ResumePaneAssigned {
                        key: cb_key,
                        pane_session_id,
                    },
                ));
            })),
            None => None,
        };
        crate::shell::wt_channel::spawn_wtcli_split_then_focus_with_callback(&argv, on_pane_id);

        tracing::info!(
            target: "agents_view",
            key = %key,
            cli = %cli_id,
            commandline = %commandline,
            launch_commandline = %launch_commandline,
            "dispatch_resume: new-tab scheduled",
        );

        #[cfg(test)]
        {
            self.last_dispatched_command = Some(DispatchedCommand {
                kind: DispatchedCommandKind::NewTabResume,
                session_id: None,
                argv,
            });
        }
    }

    /// Shift+Enter handler for terminal-state rows (Ended/Historical) in
    /// the session management view. Rather than splitting a normal pane
    /// (which `dispatch_resume` does for plain Enter), this resumes the
    /// session **inside the agent pane of a new WT tab** via ACP
    /// `session/load`.
    ///
    /// Flow:
    ///   1. Short-circuit with a system message in the current view when
    ///      the connected agent didn't advertise the `loadSession`
    ///      capability — opening a new tab would just dead-end on a
    ///      `JSON-RPC method not found` from the agent.
    ///   2. Optimistically apply `ResumeDispatched` to bump
    ///      Historical/Ended -> Idle so a rapid second Shift+Enter on the
    ///      same row no-ops (shared with `dispatch_resume`).
    ///   3. Emit a `resume_in_new_agent_tab` event to WT carrying the
    ///      session key + cwd. WT is responsible for:
    ///        - Creating a new tab (default profile, optionally honoring
    ///          cwd as the starting directory).
    ///        - Reconciling the shared agent pane onto the new tab.
    ///        - Publishing a `load_session` event BACK to WTA with the
    ///          new tab's StableId + the same session key + cwd.
    ///   4. The inbound `load_session` event handler in
    ///      `handle_wt_protocol_event` then forwards a `LoadSessionForTab`
    ///      request to the ACP client, which calls `conn.load_session`.
    ///
    /// Silent no-op for CLIs whose `cli_source` doesn't have a recognized
    /// id (unknown adapters); the inflight check is best-effort because
    /// only the agent-side knows whether the session id is recognizable.
    fn dispatch_resume_in_agent_pane(&mut self, s: &crate::agent_sessions::AgentSession) {
        tracing::info!(
            target: "agents_view",
            key = %s.key,
            status = ?s.status,
            cli = ?s.cli_source,
            supports_load = self.agent_supports_load_session,
            "dispatch_resume_in_agent_pane: Shift+Enter on row",
        );

        // Capability gate. ACP's `session/load` is opt-in (initialize
        // advertises `agentCapabilities.loadSession: bool`). Without it
        // the agent will reject the call — and we'd burn a new WT tab
        // to land on an error message. Short-circuit here instead and
        // keep the session management view focused so the user can
        // press plain Enter to fall back to the split-pane resume path.
        if !self.agent_supports_load_session {
            let agent: String = if self.agent_name.is_empty() {
                t!("system.fallback.connected_agent").into_owned()
            } else {
                self.agent_name.clone()
            };
            let msg = t!(
                "system.cannot_resume_no_load_session",
                agent = agent.as_str()
            )
            .into_owned();
            tracing::warn!(
                target: "agents_view",
                key = %s.key,
                agent = %self.agent_name,
                "dispatch_resume_in_agent_pane: agent does not support loadSession",
            );
            let tab = self.current_tab_mut();
            tab.messages.push(ChatMessage::System(msg));
            tab.scroll_to_bottom();
            #[cfg(test)]
            {
                self.last_dispatched_command = Some(DispatchedCommand {
                    kind: DispatchedCommandKind::ResumeInAgentPane,
                    session_id: Some(s.key.clone()),
                    argv: vec![
                        "resume_in_new_agent_tab".to_string(),
                        "--unsupported".to_string(),
                    ],
                });
            }
            return;
        }

        let key = s.key.clone();
        let raw_cwd_string = s.cwd.to_string_lossy().to_string();
        let valid_cwd = crate::cwd_util::validate_starting_directory(&s.cwd);
        if valid_cwd.is_none() && !raw_cwd_string.is_empty() {
            tracing::warn!(
                target: "agents_view",
                key = %key,
                "dispatch_resume_in_agent_pane: stored cwd is no longer a valid directory; omitting from resume_in_new_agent_tab event",
            );
        }
        let cwd_string = valid_cwd.unwrap_or_default();

        // Mirror dispatch_resume's optimistic state flip so a rapid
        // double press doesn't double-dispatch.
        let resume_event = crate::agent_sessions::SessionEvent::ResumeDispatched { key: key.clone() };
        self.agent_sessions.apply(resume_event.clone());
        self.publish_session_hook(resume_event);
        self.dispatch_session_resume_dispatched_rpc(&key);

        let mut params = serde_json::Map::new();
        params.insert("session_id".to_string(), serde_json::Value::String(key.clone()));
        if !cwd_string.is_empty() {
            params.insert("cwd".to_string(), serde_json::Value::String(cwd_string.clone()));
        }
        let evt = serde_json::json!({
            "type": "event",
            "method": "resume_in_new_agent_tab",
            "params": params,
        });
        send_wt_protocol_event(evt.to_string());

        tracing::info!(
            target: "agents_view",
            key = %s.key,
            "dispatch_resume_in_agent_pane: resume_in_new_agent_tab event published",
        );

        #[cfg(test)]
        {
            let mut argv = vec![
                "resume_in_new_agent_tab".to_string(),
                "--session-id".to_string(),
                s.key.clone(),
            ];
            if !cwd_string.is_empty() {
                argv.push("--cwd".to_string());
                argv.push(cwd_string);
            }
            self.last_dispatched_command = Some(DispatchedCommand {
                kind: DispatchedCommandKind::ResumeInAgentPane,
                session_id: Some(s.key.clone()),
                argv,
            });
        }
    }

    /// Test-only accessor for the most recent agent session view dispatch.
    #[cfg(test)]
    pub fn last_dispatched_command_for_test(&self) -> Option<DispatchedCommand> {
        self.last_dispatched_command.clone()
    }

    fn next_agents_rpc_request_id(&mut self) -> u64 {
        let tab = self.current_tab_mut();
        tab.agents_view.next_request_id = tab.agents_view.next_request_id.wrapping_add(1);
        tab.agents_view.next_request_id
    }

    fn dispatch_session_focus_rpc(&mut self, sid: &str) {
        let request_id = self.next_agents_rpc_request_id();
        let sid = agent_client_protocol::SessionId::new(sid.to_string());
        let _ = self.master_request_tx.send(
            crate::protocol::acp::client::MasterExtRequest::SessionFocus {
                request_id,
                sid: sid.clone(),
            },
        );
        #[cfg(test)]
        {
            self.last_dispatched_command = Some(DispatchedCommand {
                kind: DispatchedCommandKind::FocusPane,
                session_id: Some(sid.0.to_string()),
                argv: vec![
                    "session_focus".to_string(),
                    "--sid".to_string(),
                    sid.0.to_string(),
                ],
            });
        }
    }

    fn dispatch_session_resume_dispatched_rpc(&mut self, sid: &str) {
        let request_id = self.next_agents_rpc_request_id();
        let sid = agent_client_protocol::SessionId::new(sid.to_string());
        let _ = self.master_request_tx.send(
            crate::protocol::acp::client::MasterExtRequest::SessionResumeDispatched {
                request_id,
                sid,
            },
        );
    }

    pub(crate) fn open_agents_view_for_tab(&mut self, tab_id: String) {
        let rows_available = !self.agents_rows_for_tab(&tab_id).is_empty();
        {
            let tab = self.tab_mut(&tab_id);
            // Snapshot the pre-entry pane visibility so Esc can restore it
            // (a folded pane re-folds, an expanded chat pane stays open).
            // Read before any mutation below: at this point `pane_open` still
            // holds the value from before this transition (see the field docs
            // on `agents_view_prev_pane_open`).
            tab.agents_view_prev_pane_open = Some(tab.pane_open);
            tab.current_view = View::Agents;
            tab.agents_view.snapshot = Some(Vec::new());
            tab.agents_view.dirty = false;
            if tab.agents_list_state.selected().is_none() && rows_available {
                tab.agents_list_state.select(Some(0));
            }
        }
        self.update_agents_focus_for_tab(&tab_id);
        self.schedule_agents_refetch_for_tab(&tab_id);
    }

    fn close_agents_view_for_tab(&mut self, tab_id: &str) {
        let tab = self.tab_mut(tab_id);
        tab.current_view = View::Chat;
        tab.agents_view.snapshot = None;
        tab.agents_view.refetch_in_flight = false;
        tab.agents_view.dirty = false;
        tab.agents_view.focused_sid = None;
        tab.agents_view.pending_rescan = false;
        tab.agents_view.rescan_in_flight = false;
        tab.agents_view_prev_pane_open = None;
    }

    fn schedule_agents_refetch_for_tab(&mut self, tab_id: &str) {
        let request = {
            let tab = self.tab_mut(tab_id);
            if tab.agents_view.snapshot.is_none() {
                return;
            }
            if tab.agents_view.refetch_in_flight {
                tab.agents_view.dirty = true;
                return;
            }
            tab.agents_view.refetch_in_flight = true;
            tab.agents_view.dirty = false;
            tab.agents_view.next_request_id = tab.agents_view.next_request_id.wrapping_add(1);
            let request_id = tab.agents_view.next_request_id;
            tab.agents_view.latest_request_id = Some(request_id);
            // Consume the sticky F5 rescan intent only when we actually build a
            // request; if we coalesced (in-flight) above, it stays set so the
            // trailing refetch carries it.
            let rescan = std::mem::take(&mut tab.agents_view.pending_rescan);
            // Mirror onto rescan_in_flight so the loading shimmer shows for the
            // whole F5 refresh (a normal poll keeps this false). Cleared when
            // the response / failure lands.
            tab.agents_view.rescan_in_flight = rescan;
            crate::protocol::acp::client::MasterExtRequest::SessionsList { request_id, rescan }
        };
        let _ = self.master_request_tx.send(request);
    }

    fn schedule_agents_refetch_for_open_views(&mut self) {
        let tabs: Vec<String> = self
            .tab_sessions
            .iter()
            .filter_map(|(id, tab)| tab.agents_view.snapshot.as_ref().map(|_| id.clone()))
            .collect();
        for tab_id in tabs {
            self.schedule_agents_refetch_for_tab(&tab_id);
        }
    }

    fn handle_agents_snapshot_loaded(
        &mut self,
        request_id: u64,
        sessions: Vec<crate::session_registry::SessionInfo>,
    ) {
        let tabs: Vec<String> = self
            .tab_sessions
            .iter()
            .filter_map(|(id, tab)| {
                (tab.agents_view.latest_request_id == Some(request_id)).then(|| id.clone())
            })
            .collect();
        for tab_id in tabs {
            let old_selected = self
                .tab_sessions
                .get(&tab_id)
                .and_then(|t| t.agents_list_state.selected())
                .unwrap_or(0);
            let needs_trailing = {
                let tab = self.tab_mut(&tab_id);
                if tab.agents_view.snapshot.is_none() {
                    false
                } else {
                    tab.agents_view.snapshot = Some(sessions.clone());
                    tab.agents_view.refetch_in_flight = false;
                    tab.agents_view.rescan_in_flight = false;
                    let dirty = tab.agents_view.dirty;
                    tab.agents_view.dirty = false;
                    dirty
                }
            };
            self.restore_agents_selection(&tab_id, old_selected);
            if needs_trailing {
                self.schedule_agents_refetch_for_tab(&tab_id);
            }
        }
    }

    /// Counterpart to [`Self::handle_agents_snapshot_loaded`] for the
    /// failure / timeout path. Clears `refetch_in_flight` so the 5s
    /// periodic tick (or the next `sessions/changed` broadcast) can
    /// retry, but leaves `snapshot` untouched so the rendered rows
    /// stay on the last good data instead of flashing empty.
    ///
    /// Drives the `dirty` trailing-refetch the same way the success
    /// path does: if pushes coalesced while this RPC was in flight,
    /// schedule one follow-up immediately rather than wait 5s.
    fn handle_agents_snapshot_failed(&mut self, request_id: u64) {
        let tabs: Vec<String> = self
            .tab_sessions
            .iter()
            .filter_map(|(id, tab)| {
                (tab.agents_view.latest_request_id == Some(request_id)).then(|| id.clone())
            })
            .collect();
        for tab_id in tabs {
            let needs_trailing = {
                let tab = self.tab_mut(&tab_id);
                if tab.agents_view.snapshot.is_none() {
                    false
                } else {
                    tab.agents_view.refetch_in_flight = false;
                    tab.agents_view.rescan_in_flight = false;
                    let dirty = tab.agents_view.dirty;
                    tab.agents_view.dirty = false;
                    dirty
                }
            };
            if needs_trailing {
                self.schedule_agents_refetch_for_tab(&tab_id);
            }
        }
    }

    fn restore_agents_selection(&mut self, tab_id: &str, old_selected: usize) {
        let rows = self.agents_rows_for_tab(tab_id);
        let tab = self.tab_mut(tab_id);
        if rows.is_empty() {
            tab.agents_list_state.select(None);
            tab.agents_view.focused_sid = None;
            return;
        }
        let focused = tab.agents_view.focused_sid.clone();
        let idx = focused
            .as_ref()
            .and_then(|sid| rows.iter().position(|row| row.key == sid.0.as_ref()))
            .unwrap_or_else(|| old_selected.min(rows.len() - 1));
        tab.agents_list_state.select(Some(idx));
        tab.agents_view.focused_sid =
            Some(agent_client_protocol::SessionId::new(rows[idx].key.clone()));
    }

    fn update_agents_focus_for_tab(&mut self, tab_id: &str) {
        let rows = self.agents_rows_for_tab(tab_id);
        let selected = self
            .tab_sessions
            .get(tab_id)
            .and_then(|t| t.agents_list_state.selected());
        let focused = selected.and_then(|idx| {
            rows.get(idx)
                .map(|s| agent_client_protocol::SessionId::new(s.key.clone()))
        });
        self.tab_mut(tab_id).agents_view.focused_sid = focused;
    }

    fn agents_rows_for_tab(&self, tab_id: &str) -> Vec<crate::agent_sessions::AgentSession> {
        let filter = self.current_cli_filter();
        let origin = self.sessions_origin_filter;
        if let Some(snapshot) = self
            .tab_sessions
            .get(tab_id)
            .and_then(|t| t.agents_view.snapshot.as_ref())
        {
            let mut rows: Vec<_> = snapshot.iter().map(session_info_to_agent_session).collect();
            rows.sort_by(|a, b| b.last_activity_at.cmp(&a.last_activity_at));
            if let Some(want) = filter.as_ref() {
                rows.retain(|s| &s.cli_source == want || matches!(&s.cli_source, crate::agent_sessions::CliSource::Unknown(v) if v.is_empty()));
            }
            // Apply the MVP origin filter on top of the cli filter.
            // Snapshot rows come from master via SessionInfo where origin
            // is Option<SessionOrigin>; session_info_to_agent_session
            // collapses None -> SessionOrigin::Unknown so a registry-style
            // `matches(&s.origin)` is sufficient and stays consistent
            // with the registry branch below.
            rows.retain(|s| origin.matches(&s.origin));
            rows
        } else {
            self.agent_sessions
                .iter_sorted_with_filters(filter.as_ref(), origin)
                .into_iter()
                .cloned()
                .collect()
        }
    }

    /// Build the resolved ACP command string for an agent (e.g. "C:\...\claude.exe --acp").
    fn build_agent_cmd(&self, agent_id: &str) -> String {
        let profile = crate::agent_registry::lookup_profile_by_id(agent_id);
        let cmd = if !profile.acp_launch_command.is_empty() {
            profile.acp_launch_command.to_string()
        } else {
            let exe =
                crate::agent_check::find_exe(agent_id).unwrap_or_else(|| agent_id.to_string());
            let mut cmd = exe;
            for flag in profile.acp_flags {
                cmd.push(' ');
                cmd.push_str(flag);
            }
            cmd
        };
        resolve_agent_cmd(&cmd)
    }

    /// Update the deferred ACP params to use the selected agent's command.
    fn update_deferred_acp_agent(&mut self, agent_id: &str) {
        if agent_id.is_empty() {
            return;
        }
        let profile = crate::agent_registry::lookup_profile_by_id(agent_id);
        let new_cmd = if !profile.acp_launch_command.is_empty() {
            profile.acp_launch_command.to_string()
        } else {
            let exe =
                crate::agent_check::find_exe(agent_id).unwrap_or_else(|| agent_id.to_string());
            let mut cmd = exe;
            for flag in profile.acp_flags {
                cmd.push(' ');
                cmd.push_str(flag);
            }
            cmd
        };
        // Resolve to full path
        let resolved = resolve_agent_cmd(&new_cmd);
        if let Some(ref mut params) = self.deferred_acp {
            tracing::info!(
                "Updating ACP agent command: {} -> {}",
                params.agent_cmd,
                resolved
            );
            params.agent_cmd = resolved;
        }
        // Remember the selected agent so we can notify C++ after connection succeeds.
        // We don't notify now because mid-FRE WriteSettingsToDisk triggers
        // _RebuildAgentStack which tears down the in-progress agent pane.
        self.pending_agent_selection = Some(agent_id.to_string());
    }

    pub fn set_event_tx(&mut self, tx: mpsc::UnboundedSender<AppEvent>) {
        self.event_tx = Some(tx);
    }

    /// Enter the "checking" state for a (re)login: show the spinner and clear
    /// any prior status. A stale `Login failed…` (or device-code) line must not
    /// leak into the checking view, which treats a non-empty status as live
    /// device-flow progress and would otherwise render a phantom "code copied".
    fn begin_auth_checking(&mut self) {
        if let Some(ref mut auth) = self.auth {
            auth.checking = true;
            auth.status_message.clear();
        }
    }

    fn spawn_login(&self, agent_id: &str, login_command: &str) {
        if let Some(ref tx) = self.event_tx {
            let tx = tx.clone();
            let cmd = login_command.to_string();
            let id = agent_id.to_string();
            tokio::task::spawn_local(async move {
                let progress_tx = tx.clone();
                let result = tokio::task::spawn_blocking(move || {
                    use std::io::BufRead;

                    // Parse command into exe + args (e.g. "C:\path\copilot.exe login")
                    // Handle quoted paths: "C:\path with spaces\copilot.exe" login
                    let (exe, args) = if cmd.starts_with('"') {
                        // Quoted path: find closing quote
                        if let Some(end) = cmd[1..].find('"') {
                            let exe = &cmd[1..end + 1];
                            let rest = cmd[end + 2..].trim();
                            (
                                exe.to_string(),
                                rest.split_whitespace()
                                    .map(String::from)
                                    .collect::<Vec<_>>(),
                            )
                        } else {
                            (cmd.clone(), vec![])
                        }
                    } else {
                        let parts: Vec<&str> = cmd.splitn(2, ' ').collect();
                        (
                            parts[0].to_string(),
                            parts
                                .get(1)
                                .map(|s| s.split_whitespace().map(String::from).collect())
                                .unwrap_or_default(),
                        )
                    };

                    // The device-verification URL follows the (optional)
                    // `--host` (see `device_verify_url`).
                    let verify_url = device_verify_url(&cmd);
                    let verify_url_stderr = verify_url.clone();

                    let mut child = match std::process::Command::new(&exe)
                        .args(&args)
                        .stdout(std::process::Stdio::piped())
                        .stderr(std::process::Stdio::piped())
                        .stdin(std::process::Stdio::null())
                        .spawn()
                    {
                        Ok(c) => c,
                        Err(e) => {
                            tracing::warn!("spawn_login: failed to spawn '{}': {}", exe, e);
                            return (false, None);
                        }
                    };

                    // Read both stdout and stderr — copilot login may
                    // write to either depending on buffering/version.
                    let stdout = child.stdout.take();
                    let stderr = child.stderr.take();

                    let progress_tx2 = progress_tx.clone();
                    let stderr_handle = std::thread::spawn(move || {
                        let mut found_success = false;
                        let mut error_line: Option<String> = None;
                        if let Some(stderr) = stderr {
                            let reader = std::io::BufReader::new(stderr);
                            for line in reader.lines().map_while(Result::ok) {
                                // Raw auth-flow output carries the device code — trace only.
                                tracing::trace!(target: "login.content", "login stderr: {}", line);
                                if line.contains("enter code") {
                                    if let Some(code) = line.split("enter code ").nth(1) {
                                        let code = code.trim_end_matches('.');
                                        let _ = progress_tx2.send(AppEvent::LoginProgress {
                                            device_code: code.to_string(),
                                            verify_url: verify_url_stderr.clone(),
                                        });
                                    }
                                }
                                if line.contains("Signed in successfully")
                                    || line.contains("already logged in")
                                {
                                    found_success = true;
                                    break;
                                }
                                let low = line.to_lowercase();
                                if low.contains("fail") || low.contains("error") {
                                    error_line = Some(line.clone());
                                }
                            }
                        }
                        (found_success, error_line)
                    });

                    let mut found_success = false;
                    let mut error_line: Option<String> = None;
                    if let Some(stdout) = stdout {
                        let reader = std::io::BufReader::new(stdout);
                        for line in reader.lines().map_while(Result::ok) {
                            // Raw auth-flow output carries the device code — trace only.
                            tracing::trace!(target: "login.content", "login stdout: {}", line);
                            if line.contains("enter code") {
                                if let Some(code) = line.split("enter code ").nth(1) {
                                    let code = code.trim_end_matches('.');
                                    let _ = progress_tx.send(AppEvent::LoginProgress {
                                        device_code: code.to_string(),
                                        verify_url: verify_url.clone(),
                                    });
                                }
                            }
                            if line.contains("Signed in successfully")
                                || line.contains("already logged in")
                            {
                                found_success = true;
                                break;
                            }
                            let low = line.to_lowercase();
                            if low.contains("fail") || low.contains("error") {
                                error_line = Some(line.clone());
                            }
                        }
                    }

                    if found_success {
                        // Stdout confirmed login succeeded — return
                        // immediately. Don't wait for stderr or the
                        // child process; copilot login may have spawned
                        // sub-processes that keep pipes open.
                        tracing::info!("login: stdout success detected, returning immediately");
                        let _ = child.kill();
                        // Don't call child.wait() — it can block if
                        // sub-processes are still running.
                        drop(stderr_handle);
                        return (true, None);
                    }

                    let (stderr_success, stderr_error) =
                        stderr_handle.join().unwrap_or((false, None));
                    found_success = stderr_success;

                    if !found_success {
                        // Wait for process and check exit code
                        found_success = child.wait().map(|s| s.success()).unwrap_or(false);
                    } else {
                        let _ = child.kill();
                        let _ = child.wait();
                    }
                    // On failure, surface the most specific error line we saw
                    // (stdout preferred, then stderr) so the UI can show *why*.
                    let error = if found_success {
                        None
                    } else {
                        error_line.or(stderr_error)
                    };
                    (found_success, error)
                })
                .await;

                let (success, error) = result.unwrap_or((false, None));
                if !success {
                    tracing::warn!(
                        target: "login",
                        agent = %id,
                        reason = error.as_deref().unwrap_or("(no reason captured)"),
                        "login failed"
                    );
                }
                tracing::info!("login: spawn_blocking returned, sending LoginComplete success={}", success);
                let send_result = tx.send(AppEvent::LoginComplete {
                    agent_id: id,
                    success,
                    error,
                });
                tracing::info!("login: LoginComplete send result={:?}", send_result.is_ok());
            });
        }
    }

    /// Diagnostic setup-mode key handler. Covers install, sign-in, and retry
    /// actions via the `SetupOption` variants.
    fn handle_setup_key(&mut self, key: KeyEvent) {
        // Block all input during install (except Ctrl+C / Esc to quit)
        let is_installing = self.setup.as_ref().map_or(false, |s| s.install_in_progress);
        tracing::debug!(target: "setup_key", code = ?key.code, is_installing, selected = ?self.setup.as_ref().map(|s| s.selected_index), options_count = ?self.setup.as_ref().map(|s| s.options.len()), "handle_setup_key");

        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            KeyCode::Esc => {
                self.should_quit = true;
            }
            _ if is_installing => {
                return; // block all other keys during install
            }
            KeyCode::Up => {
                if let Some(ref mut setup) = self.setup {
                    if setup.selected_index > 0 {
                        setup.selected_index -= 1;
                    }
                }
            }
            KeyCode::Down => {
                if let Some(ref mut setup) = self.setup {
                    let max = setup.options.len().saturating_sub(1);
                    if setup.selected_index < max {
                        setup.selected_index += 1;
                    }
                }
            }
            KeyCode::Enter => {
                // Clone the selected option so we can act on it without borrowing setup
                let selected_opt = self
                    .setup
                    .as_ref()
                    .and_then(|s| s.options.get(s.selected_index).cloned());
                if let Some(opt) = selected_opt {
                    self.handle_setup_enter(opt);
                }
            }
            KeyCode::Char('o') | KeyCode::Char('O') => {
                // Open install URL if the selected option is an install-related one
                if let Some(ref setup) = self.setup {
                    if let Some(opt) = setup.options.get(setup.selected_index) {
                        match opt {
                            SetupOption::Install { .. } => {
                                let url = setup.preflight.install_url.clone();
                                if !url.is_empty() {
                                    let _ = open_url_in_browser(&url);
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Dispatch Enter on the selected `SetupOption`.
    fn handle_setup_enter(&mut self, opt: SetupOption) {
        tracing::info!(target: "setup_key", option = ?std::mem::discriminant(&opt), "handle_setup_enter");
        match opt {
            SetupOption::Install { agent_id, .. } => {
                if let Some(ref setup) = self.setup {
                    if setup.install_in_progress {
                        return;
                    }
                }
                if let Some(ref mut setup) = self.setup {
                    setup.install_in_progress = true;
                    setup.install_error = None;
                    setup.install_log.clear();
                    setup.install_log.push(format!(
                        "{} {}",
                        t!("setup.status.installing"),
                        agent_id
                    ));
                }
                // Spawn async winget install via agent_check
                if let Some(ref tx) = self.event_tx {
                    let tx = tx.clone();
                    let id = agent_id.clone();
                    tokio::task::spawn_local(async move {
                        let result = crate::agent_check::install(&id, |_line| {
                            // Could send log lines as events, but keep simple for now
                        })
                        .await;
                        match result {
                            Ok(()) => {
                                tracing::info!("Install {} succeeded", id);
                            }
                            Err(e) => {
                                tracing::warn!("Install {} failed: {}", id, e);
                            }
                        }
                        let _ = tx.send(AppEvent::AgentInstallComplete);
                    });
                }
            }
            SetupOption::SignIn {
                agent_id,
                display_name,
            } => {
                self.mode = AppMode::Auth;
                let (enterprise_mode, enterprise_host) = copilot_enterprise_prefill(&agent_id);
                self.auth = Some(AuthState {
                    agent_id: agent_id.clone(),
                    agent_name: display_name,
                    login_command: crate::agent_check::build_login_cmd(&agent_id, None),
                    checking: false,
                    status_message: String::new(),
                    enterprise_mode,
                    enterprise_host,
                });
            }
            SetupOption::Retry => {
                // Re-run preflight detection and try to reconnect
                if let Some(ref setup) = self.setup {
                    let agent_id = setup.preflight.agent_id.clone();
                    if !agent_id.is_empty() {
                        let status = crate::agent_check::check_agent(&agent_id);
                        if status.cli_found {
                            // CLI found — try to connect (auth will be checked by ACP).
                            // Stay in Setup mode with "Connecting..." to avoid a flash
                            // of red error text in Chat if ACP fails immediately.
                            self.update_deferred_acp_agent(&agent_id);
                            self.state = ConnectionState::Connecting(
                                t!("connection.reconnecting").into_owned(),
                            );
                            self.preflight_setup_active = false;
                            if self.deferred_acp.is_some() {
                                self.pending_acp_start = true;
                            } else {
                                let new_cmd = self.build_agent_cmd(&agent_id);
                                let _ = self.restart_tx.send(RestartRequest {
                                    agent_cmd: Some(new_cmd),
                                });
                            }
                            // Don't clear setup yet — AgentConnected will transition to Chat,
                            // AgentError will update the Setup screen.
                        }
                    }
                }
            }
        }
    }

    /// Key used for lookup into `tab_sessions`. Falls back to
    /// `DEFAULT_TAB_ID` until `tab_changed` from Windows Terminal arrives.
    fn active_tab_key(&self) -> &str {
        self.tab_id.as_deref().unwrap_or(DEFAULT_TAB_ID)
    }

    /// Read-only view of the currently focused tab's per-tab state. Always
    /// non-panicking: `App::new` seeds `DEFAULT_TAB_ID` and
    /// `tab_changed` lazily creates the entry for any new tab via
    /// `current_tab_mut`/`tab_mut`.
    pub fn current_tab(&self) -> &TabSession {
        let key = self.active_tab_key();
        self.tab_sessions
            .get(key)
            .expect("active tab session always materialized")
    }

    /// Mutable view of the currently focused tab's per-tab state.
    /// Lazily inserts a default `TabSession` if the key is missing.
    pub fn current_tab_mut(&mut self) -> &mut TabSession {
        let key = self
            .tab_id
            .clone()
            .unwrap_or_else(|| DEFAULT_TAB_ID.to_string());
        self.tab_sessions.entry(key).or_default()
    }

    /// Mutable view of an arbitrary tab's per-tab state, lazily inserting
    /// a default `TabSession` if missing. Used by `tab_changed` and (in
    /// Milestone 2) by chunk routing keyed on `SessionId`.
    #[allow(dead_code)]
    pub fn tab_mut(&mut self, tab_id: &str) -> &mut TabSession {
        self.tab_sessions.entry(tab_id.to_string()).or_default()
    }

    /// Resolve a `SessionId` to the tab that owns it. Returns the active
    /// tab as a fallback when the session is unknown -- covers events
    /// emitted before a session was attached (rare) or pre-session
    /// startup events.
    fn tab_for_session(&self, session_id: &str) -> String {
        self.session_to_tab
            .get(session_id)
            .cloned()
            .or_else(|| self.tab_id.clone())
            .unwrap_or_else(|| DEFAULT_TAB_ID.to_string())
    }

    /// Mutable view of the tab that owns the given session id. Lazily
    /// creates the `TabSession` if missing.
    pub fn session_tab_mut(&mut self, session_id: &str) -> &mut TabSession {
        let key = self.tab_for_session(session_id);
        self.tab_sessions.entry(key).or_default()
    }

    /// Read-only view of the tab that owns the given session id.
    pub fn session_tab(&self, session_id: &str) -> &TabSession {
        let key = self.tab_for_session(session_id);
        self.tab_sessions
            .get(&key)
            .or_else(|| self.tab_sessions.get(DEFAULT_TAB_ID))
            .expect("active tab session always materialized")
    }

    pub async fn run(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        mut ui_rx: mpsc::UnboundedReceiver<AppEvent>,
        mut event_rx: mpsc::UnboundedReceiver<AppEvent>,
    ) -> Result<()> {
        const MAX_EVENTS_PER_FRAME: usize = 64;

        let initial_draw_started = std::time::Instant::now();
        self.draw_frame(terminal)?;
        ui_trace::log_slow("initial_draw", initial_draw_started.elapsed(), || {
            self.trace_state()
        });

        loop {
            tokio::select! {
                biased;

                Some(event) = ui_rx.recv() => {
                    let event_name = Self::event_name(&event);
                    self.apply_resize_if_needed(terminal, &event)?;
                    let should_redraw = self.event_requires_redraw(&event);
                    let handle_started = std::time::Instant::now();
                    self.handle_event(event);
                    ui_trace::log_slow("ui_event_handle", handle_started.elapsed(), || {
                        format!("event={} {}", event_name, self.trace_state())
                    });
                    if should_redraw {
                        let draw_started = std::time::Instant::now();
                        self.draw_frame(terminal)?;
                        ui_trace::log_slow("ui_event_draw", draw_started.elapsed(), || {
                            format!("event={} {}", event_name, self.trace_state())
                        });
                    }
                }

                Some(event) = event_rx.recv() => {
                    let first_event_name = Self::event_name(&event);
                    self.apply_resize_if_needed(terminal, &event)?;
                    let batch_started = std::time::Instant::now();
                    let mut processed = 0usize;

                    let mut should_redraw_now = self.event_requires_redraw(&event);
                    self.handle_event(event);
                    processed += 1;

                    while processed < MAX_EVENTS_PER_FRAME {
                        match event_rx.try_recv() {
                            Ok(event) => {
                                self.apply_resize_if_needed(terminal, &event)?;
                                if self.event_requires_redraw(&event) {
                                    should_redraw_now = true;
                                }
                                self.handle_event(event);
                                processed += 1;
                            }
                            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => break,
                        }
                    }

                    ui_trace::log_slow("event_batch_handle", batch_started.elapsed(), || {
                        format!(
                            "first_event={} processed={} redraw={} {}",
                            first_event_name,
                            processed,
                            should_redraw_now,
                            self.trace_state()
                        )
                    });

                    if should_redraw_now {
                        let draw_started = std::time::Instant::now();
                        self.draw_frame(terminal)?;
                        ui_trace::log_slow("event_batch_draw", draw_started.elapsed(), || {
                            format!(
                                "first_event={} processed={} {}",
                                first_event_name,
                                processed,
                                self.trace_state()
                            )
                        });
                    }
                }

                else => {
                    break; // All senders dropped
                }
            }

            // Deferred ACP start after login completes
            self.try_start_acp();

            if self.should_quit {
                break;
            }
        }
        Ok(())
    }

    fn apply_resize_if_needed(
        &self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        event: &AppEvent,
    ) -> Result<()> {
        let AppEvent::Resize(width, height) = event else {
            return Ok(());
        };

        let resize_started = std::time::Instant::now();
        terminal.resize(Rect::new(0, 0, *width, *height))?;
        ui_trace::log_slow("terminal_resize", resize_started.elapsed(), || {
            format!("width={} height={}", width, height)
        });
        Ok(())
    }

    fn draw_frame(&mut self, terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
        let total_started = std::time::Instant::now();

        let mut frame = terminal.get_frame();

        let render_started = std::time::Instant::now();
        ui::render(&mut frame, self);
        ui_trace::log_slow("ui_render", render_started.elapsed(), || self.trace_state());

        // The text caret is painted as an inverse buffer cell by `ui::input`
        // in every state, so the OS cursor is always hidden. With no
        // `show_cursor`/`set_cursor_position` interleaved after the content
        // flush, there's no partial-frame tearing to hide — hence no need for
        // a synchronized-update (CSI ? 2026) wrapper around the frame (which
        // was also the prime suspect for frames being held until the next
        // redraw on an unfocused pane).
        let flush_started = std::time::Instant::now();
        terminal.hide_cursor()?;
        terminal.flush()?;
        ui_trace::log_slow("terminal_flush", flush_started.elapsed(), || {
            self.trace_state()
        });

        terminal.swap_buffers();

        let backend_flush_started = std::time::Instant::now();
        terminal.backend_mut().flush()?;
        ui_trace::log_slow(
            "terminal_backend_flush",
            backend_flush_started.elapsed(),
            || self.trace_state(),
        );

        self.log_selection_visible_if_needed();

        ui_trace::log_slow("draw_frame_total", total_started.elapsed(), || {
            self.trace_state()
        });

        Ok(())
    }

    fn event_name(event: &AppEvent) -> &'static str {
        match event {
            AppEvent::Key(_) => "key",
            AppEvent::Tick => "tick",
            AppEvent::Resize(_, _) => "resize",
            AppEvent::FocusChanged(_) => "focus_changed",
            AppEvent::ConnectionStage(_) => "connection_stage",
            AppEvent::ProgressStatus { .. } => "progress_status",
            AppEvent::AgentConnected { .. } => "agent_connected",
            AppEvent::SessionAttached { .. } => "session_attached",
            AppEvent::TabError { .. } => "tab_error",
            AppEvent::TabSystemMessage { .. } => "tab_system_message",
            AppEvent::PromptTemplateLoaded { .. } => "prompt_template_loaded",
            AppEvent::AutofixTargetResolved { .. } => "autofix_target_resolved",
            AppEvent::AgentError { .. } => "agent_error",
            AppEvent::AgentSoftStop { .. } => "agent_soft_stop",
            AppEvent::AgentBusy { .. } => "agent_busy",
            AppEvent::TabRenamed { .. } => "tab_renamed",
            AppEvent::ExecutionInfo(_) => "execution_info",
            AppEvent::AgentThoughtChunk { .. } => "agent_thought_chunk",
            AppEvent::AgentMessageChunk { .. } => "agent_message_chunk",
            AppEvent::UserMessageReplayChunk { .. } => "user_message_replay_chunk",
            AppEvent::AgentMessageEnd { .. } => "agent_message_end",
            AppEvent::TimingMetric { .. } => "timing_metric",
            AppEvent::ToolCall { .. } => "tool_call",
            AppEvent::ToolCallUpdate { .. } => "tool_call_update",
            AppEvent::Plan { .. } => "plan",
            AppEvent::PermissionRequest { .. } => "permission_request",
            AppEvent::SystemMessage(_) => "system_message",
            AppEvent::DebugPipeMessage(_) => "debug_pipe_message",
            AppEvent::WtEvent { .. } => "wt_event",
            AppEvent::AgentInstallComplete => "agent_install_complete",
            AppEvent::LoginProgress { .. } => "login_progress",
            AppEvent::LoginComplete { .. } => "login_complete",
            AppEvent::PostLoginAuthRecovery { .. } => "post_login_auth_recovery",
            AppEvent::AuthRecoveryTimedOut { .. } => "auth_recovery_timed_out",
            AppEvent::PreflightComplete(_) => "preflight_complete",
            AppEvent::AgentSessionEvent(_) => "agent_session_event",
            AppEvent::AliveSnapshotLoaded(_) => "alive_snapshot_loaded",
            AppEvent::AliveSessionAdded(_) => "alive_session_added",
            AppEvent::AliveSessionRemoved(_) => "alive_session_removed",
            AppEvent::AliveJoinUpgrade(_) => "alive_join_upgrade",
            AppEvent::SessionsChanged => "sessions_changed",
            AppEvent::AgentsSnapshotLoaded { .. } => "agents_snapshot_loaded",
            AppEvent::AgentsSnapshotFailed { .. } => "agents_snapshot_failed",
            AppEvent::MasterMutationCompleted { .. } => "master_mutation_completed",
            AppEvent::RevealTick => "reveal_tick",
        }
    }

    fn trace_state(&self) -> String {
        let tab = self.current_tab();
        format!(
            "state={:?} turn={:?} messages={} completed_turns={} input_chars={} pending_chars={} scroll={} activity_frame={} recommendations={} permission={} timing_note={}",
            self.state,
            std::mem::discriminant(&tab.turn),
            tab.messages.len(),
            tab.completed_turns.len(),
            tab.input.chars().count(),
            tab.turn.buffer().map(|b| b.chars().count()).unwrap_or(0),
            tab.chat_scroll.offset,
            tab.activity_frame,
            tab.turn.recommendations().map(|r| r.choices.len()).unwrap_or(0),
            !tab.permission.is_empty(),
            tab.timing_note.is_some()
        )
    }

    /// Render the sign-in / setup screen for `agent_id` (the
    /// `SetupReason::AgentError` flavor). Used by the `AuthRecoveryTimedOut`
    /// dead-man fallback so a dropped/slow auth-recovery restart still lands
    /// the user on an actionable sign-in screen (mirrors the `AgentError`
    /// auth-fallback path).
    fn show_signin_setup_screen(&mut self, agent_id: String) {
        tracing::info!("show_signin_setup_screen: agent_id={}", agent_id);
        let profile = crate::agent_registry::lookup_profile(&agent_id);
        let agent_status = crate::agent_check::check_agent(profile.id);
        let reason = SetupReason::AgentError;
        let options = build_setup_options(&reason, Some(&agent_status));
        self.mode = AppMode::Setup;
        self.state = ConnectionState::Disconnected;
        self.auth = None;
        self.setup = Some(SetupState {
            reason,
            selected_index: 0,
            preflight: PreflightResult {
                agent_id: profile.id.to_string(),
                display_name: profile.display_name.to_string(),
                // Reflect the CLI's real presence (we just computed
                // `agent_status`) instead of hard-coding "found" — on the
                // dead-man fallback the CLI may genuinely be the problem.
                cli_status: if agent_status.cli_found {
                    CheckStatus::Passed
                } else {
                    CheckStatus::Failed(t!("agent.status.not_found").into_owned())
                },
                cli_path: agent_status.cli_path.clone(),
                auth_status: CheckStatus::Failed(
                    t!("system.authentication_failed").into_owned(),
                ),
                install_hint: profile.install_hint.to_string(),
                install_url: String::new(),
                auth_hint: profile.auth_hint.to_string(),
            },
            install_in_progress: false,
            install_log: Vec::new(),
            install_error: None,
            options,
            title: t!("setup.title.sign_in").into_owned(),
            subtitle: if profile.id == "copilot" {
                t!("setup.subtitle.copilot_auth", agent = profile.display_name)
                    .into_owned()
            } else {
                t!("setup.subtitle.agent_auth", agent = profile.display_name)
                    .into_owned()
            },
        });
        let tab = self.current_tab_mut();
        tab.messages.retain(|m| !matches!(m, ChatMessage::Error(_)));
    }

    fn handle_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::Key(key) => self.handle_key(key),
            AppEvent::Tick => {
                // Fan out across all tabs: a background tab with an in-flight
                // prompt should keep its shimmer phase advancing so when the
                // user switches back the animation is in step.
                for tab in self.tab_sessions.values_mut() {
                    if tab.turn.spinner_label().is_some() || tab.progress_status.is_some() {
                        tab.activity_frame =
                            (tab.activity_frame + 1) % crate::ui::ACTIVITY_CYCLE_FRAMES;
                    }
                }
                // Setup-mode spinner: ticks while we're showing the wizard
                // (e.g. spinning during a `winget install` background job).
                // Also advance while the agents view waits on its first
                // session/list snapshot so the "Loading" shimmer keeps animating.
                if self.mode == AppMode::Setup
                    || self.mode == AppMode::Auth
                    || self.agents_view_awaiting_snapshot()
                    // Keep the connecting indicator animating during the
                    // pipe-connect → ACP init → session/new handshake so a cold
                    // start (which can run tens of seconds) doesn't look frozen
                    // (F7). Without this the chat sat static with no progress.
                    || matches!(self.state, ConnectionState::Connecting(_))
                {
                    self.activity_frame = self.activity_frame.wrapping_add(1);
                }
                // Age and auto-dismiss notifications
                for n in self.wt_notifications.iter_mut() {
                    n.age_ticks = n.age_ticks.saturating_add(1);
                }
                self.wt_notifications.retain(|n| !n.should_auto_dismiss());
                if self.wt_notifications.is_empty()
                    || self.wt_notifications.iter().all(|n| n.acknowledged)
                {
                    self.show_notification_banner = false;
                }
            }
            AppEvent::Resize(w, h) => {
                self.terminal_cols = w;
                self.terminal_rows = h;
            }
            AppEvent::RevealTick => {
                self.advance_reveal();
            }
            AppEvent::FocusChanged(focused) => {
                self.pane_focused = focused;
            }
            AppEvent::ConnectionStage(stage) => {
                self.state = ConnectionState::Connecting(stage);
                self.publish_agent_status();
            }
            AppEvent::ProgressStatus { session_id, status } => {
                let tab = match session_id {
                    Some(sid) => self.session_tab_mut(&sid),
                    None => self.current_tab_mut(),
                };
                tab.progress_status = Some(status);
                tab.scroll_to_bottom();
            }
            AppEvent::AgentConnected {
                name,
                model,
                version,
                session_id,
                available_models,
                current_model_id,
                load_session_supported,
                image_supported,
            } => {
                self.agent_name = name;
                self.agent_model = model;
                self.agent_version = version;
                self.session_id = session_id.clone();
                self.available_models = available_models.clone();
                self.current_model_id = current_model_id.clone();
                self.agent_supports_load_session = load_session_supported;
                self.agent_supports_image = image_supported;
                self.state = ConnectionState::Connected;
                // A successful connect resolves any in-flight auth recovery:
                // bump the generation so a still-pending dead-man timer becomes
                // stale and can't later force the sign-in screen.
                self.auth_recovery_generation = self.auth_recovery_generation.wrapping_add(1);
                // A live connection cancels the degraded latch (e.g. the
                // post-sign-in reconnect that goes back through master).
                self.transport_lost = false;
                self.preflight_setup_active = false;
                // If we were in Setup (e.g. after Retry), transition to Chat
                if self.mode == AppMode::Setup {
                    self.mode = AppMode::Chat;
                    self.setup = None;
                }
                // Show welcome hint on first-ever connect (persisted in state.json).
                // The disclaimer card is pushed as a `ChatMessage::Disclaimer`
                // on every agent-pane startup that lands on an empty chat (no
                // prior completed turns and no other in-flight messages), so
                // a session restored with history doesn't get a disclaimer
                // injected on top. Once shown it's allowed to be cleared by
                // a subsequent turn — the next startup re-pushes it.
                if !welcome_shown_in_state() {
                    self.show_welcome_hint = true;
                }
                // Bind the startup session to whichever tab we own.
                let bind_tab = self
                    .tab_id
                    .clone()
                    .unwrap_or_else(|| DEFAULT_TAB_ID.to_string());
                self.session_to_tab
                    .insert(session_id.clone(), bind_tab.clone());
                let tab = self.tab_mut(&bind_tab);
                tab.session_id = Some(session_id);
                let has_real_content = !tab.completed_turns.is_empty()
                    || tab
                        .messages
                        .iter()
                        .any(|m| !matches!(m, ChatMessage::Disclaimer));
                if !has_real_content
                    && !tab
                        .messages
                        .iter()
                        .any(|m| matches!(m, ChatMessage::Disclaimer))
                {
                    tab.messages.insert(0, ChatMessage::Disclaimer);
                }
                self.publish_agent_status();
            }
            AppEvent::SessionAttached {
                tab_id,
                session_id,
                available_models,
                current_model_id,
            } => {
                self.session_to_tab
                    .insert(session_id.clone(), tab_id.clone());
                let tab = self.tab_mut(&tab_id);
                tab.session_id = Some(session_id.clone());
                // Close the session/load replay window only when this
                // attach is for the session we asked to load. An
                // unrelated `SessionAttached` (e.g. the bootstrap
                // `session/new` that runs once at helper startup, which
                // can race against a Plan-C `--initial-load-session-id`
                // load_session queued at boot) would otherwise flip
                // `loading_session` off prematurely and the agent's
                // replay chunks would hit the chunk handlers'
                // `if !loading_session { return; }` gate and be
                // dropped.
                let is_load_target = tab
                    .loading_target_session_id
                    .as_deref()
                    .map(|t| t == session_id.as_str())
                    .unwrap_or(false);
                if tab.loading_session && is_load_target {
                    tab.flush_load_replay_pending();
                    tab.pack_replayed_messages_into_turns();
                    tab.loading_session = false;
                    tab.loading_target_session_id = None;
                    tab.scroll_to_bottom();
                }
                // Per-session model lists could differ — surface the new
                // tab's models when the agent_status event publishes for
                // this session in the future. For now we keep
                // App.available_models pointing at the active session's
                // models so the existing settings UI stays correct.
                if !available_models.is_empty() {
                    self.available_models = available_models;
                }
                if current_model_id.is_some() {
                    self.current_model_id = current_model_id;
                }
                // Keep freshly-created sessions on the effective model for
                // this tab — its per-pane `/model` override if set, else the
                // global acp-model. A resumed (loaded) session keeps whatever
                // model it was saved with; only fresh `/new` and lazy-first-
                // prompt sessions adopt the override. This is what makes a
                // local `/model` pick survive `/new`. The bootstrap session is
                // already model-applied by the client at startup.
                if !is_load_target {
                    if let Some(model) = self.effective_model_for_tab(&tab_id) {
                        self.send_session_model(Some(session_id.clone()), model);
                    }
                }
                self.publish_agent_status();
            }
            AppEvent::TabError { tab_id, message } => {
                // Scoped error for a specific tab. Bypasses the global
                // auth-fallback / ConnectionState::Failed flip in
                // AgentError because the error is local to one tab's
                // session-load attempt, not the whole connection.
                let tab = self.tab_mut(&tab_id);
                tab.loading_session = false;
                tab.loading_target_session_id = None;
                tab.progress_status = None;
                tab.pending_agent_response.clear();
                tab.pending_user_replay.clear();
                tab.timing_note = None;
                tab.turn = TurnState::Idle;
                tab.messages.push(ChatMessage::Error(message));
                tab.scroll_to_bottom();
            }
            AppEvent::TabSystemMessage { tab_id, message } => {
                let tab = self.tab_mut(&tab_id);
                tab.messages.push(ChatMessage::System(message));
                tab.scroll_to_bottom();
            }
            AppEvent::PromptTemplateLoaded { name } => {
                self.prompt_name = Some(name);
            }
            AppEvent::AutofixTargetResolved {
                tab_id,
                prompt_id,
                pane_id,
            } => {
                self.apply_autofix_target_resolved(tab_id, prompt_id, pane_id);
            }
            AppEvent::AgentBusy { tab_id } => {
                let tab = self.tab_mut(&tab_id);
                tab.messages
                    .push(ChatMessage::System(t!("system.agent_busy").into_owned()));
                tab.scroll_to_bottom();
            }
            AppEvent::TabRenamed {
                old_tab_id,
                new_tab_id,
                new_window_id,
            } => {
                self.rename_tab_session(&old_tab_id, &new_tab_id, new_window_id.as_deref());
            }
            AppEvent::AgentError {
                session_id,
                failure,
                message,
            } => {
                // Classification is typed (`AgentFailure`), done once at the
                // helper boundary where the `acp::Error` code / transport
                // signal is still available. No substring matching here — the
                // discriminant decides the recovery path. `message` is only the
                // human-readable line to display.
                tracing::info!(
                    target: "failure",
                    class = failure.class(),
                    session_id = ?session_id,
                    "agent failure"
                );

                // A user-initiated cancel surfaced as an error is not a
                // failure — the turn already ended via the cancel path, so
                // show nothing and leave the state untouched.
                if failure.is_cancelled() {
                    return;
                }

                // The transport to master is gone — latch the degraded state
                // so the slash-command popup greys out everything but
                // /restart (the only command that can recover without the
                // dead pipe). Cleared on the next Connected.
                if matches!(
                    failure,
                    crate::protocol::acp::failure::AgentFailure::TransportLost
                ) {
                    self.transport_lost = true;
                }

                let is_auth_error = failure.is_auth();
                if is_auth_error && !self.preflight_setup_active {
                    tracing::info!("AgentError auth fallback: showing setup screen");
                    // Use current_agent_id — set at preflight or agent selection time.
                    let agent_id = if !self.current_agent_id.is_empty() {
                        self.current_agent_id.clone()
                    } else {
                        "copilot".to_string()
                    };
                    tracing::info!("AgentError: resolved agent_id={}", agent_id);
                    let profile = crate::agent_registry::lookup_profile(&agent_id);
                    let agent_status = crate::agent_check::check_agent(profile.id);
                    let reason = SetupReason::AgentError;
                    let options = build_setup_options(&reason, Some(&agent_status));
                    self.mode = AppMode::Setup;
                    self.state = ConnectionState::Disconnected;
                    self.auth = None;
                    self.setup = Some(SetupState {
                        reason,
                        selected_index: 0,
                        preflight: PreflightResult {
                            agent_id: profile.id.to_string(),
                            display_name: profile.display_name.to_string(),
                            cli_status: CheckStatus::Passed,
                            cli_path: None,
                            auth_status: CheckStatus::Failed(
                                t!("system.authentication_failed").into_owned(),
                            ),
                            install_hint: profile.install_hint.to_string(),
                            install_url: String::new(),
                            auth_hint: profile.auth_hint.to_string(),
                        },
                        install_in_progress: false,
                        install_log: Vec::new(),
                        install_error: None,
                        options,
                        title: t!("setup.title.sign_in").into_owned(),
                        subtitle: if profile.id == "copilot" {
                            t!("setup.subtitle.copilot_auth", agent = profile.display_name)
                                .into_owned()
                        } else {
                            t!("setup.subtitle.agent_auth", agent = profile.display_name)
                                .into_owned()
                        },
                    });
                    // Clear error messages
                    let tab = self.current_tab_mut();
                    tab.messages.retain(|m| !matches!(m, ChatMessage::Error(_)));
                } else {
                    self.state = ConnectionState::Failed(message.clone());
                    self.publish_agent_status();
                    let tab = match session_id.as_deref() {
                        Some(sid) => self.session_tab_mut(sid),
                        None => self.current_tab_mut(),
                    };
                    tab.progress_status = None;
                    tab.activity_frame = 0;
                    tab.timing_note = None;
                    tab.turn = TurnState::Idle;
                    // Suppress only an *identical* consecutive error, not any
                    // trailing error. When the master/agent dies, two errors can
                    // arrive: the raw transport error (returned as-is) and the
                    // `handle_io` watchdog's connection.lost ("/restart") line.
                    // Those are different messages and BOTH should show — the raw
                    // one says what broke, the connection.lost one says how to
                    // recover. Collapsing every consecutive error (the previous
                    // behavior) could hide the /restart hint behind an unrelated
                    // or in-flight error. Dedup only true duplicates so the same
                    // line never stacks.
                    let is_duplicate = matches!(
                        tab.messages.last(),
                        Some(ChatMessage::Error(prev)) if prev == &message
                    );
                    if !is_duplicate {
                        tab.messages.push(ChatMessage::Error(message));
                    }
                }
            }
            AppEvent::PostLoginAuthRecovery {
                failure,
                tab_id,
                agent_id,
            } => {
                tracing::warn!(
                    target: "auth_recovery",
                    failure_class = failure.class(),
                    tab_id = ?tab_id,
                    agent_id = %agent_id,
                    "post-login auth recovery: shared master CLI still AuthRequired \
                     after a successful login; reconnecting via a fresh master \
                     (restart_agent_stack)"
                );
                let resolved = if !agent_id.is_empty() {
                    agent_id.clone()
                } else {
                    "copilot".to_string()
                };
                // Pin this recovery to a fresh generation so a stale dead-man
                // timer (from an earlier recovery, or one whose reconnect later
                // succeeds — see AgentConnected) can't fire onto an unrelated
                // Connecting state.
                self.auth_recovery_generation = self.auth_recovery_generation.wrapping_add(1);
                let recovery_generation = self.auth_recovery_generation;
                // (i) Transient "Reconnecting…" — NOT the sign-in screen. The
                // restart below tears this pane down + respawns it, so the
                // common (successful) case never flashes the setup screen
                // between login and the fresh pane connecting. Only a dropped/
                // slow restart leaves us alive long enough for the
                // `AuthRecoveryTimedOut` fallback path to surface the sign-in screen.
                self.mode = AppMode::Chat;
                self.setup = None;
                self.auth = None;
                self.state =
                    ConnectionState::Connecting(t!("connection.reconnecting").into_owned());
                {
                    let tab = self.current_tab_mut();
                    tab.messages.retain(|m| !matches!(m, ChatMessage::Error(_)));
                }
                // (ii) Request a fresh master CLI. The long-lived shared CLI
                // cached its unauthenticated state at spawn and `authenticate`
                // does not refresh it; only a respawn (which re-reads the now
                // valid on-disk credential) recovers. Reuse the tested
                // `/restart` machinery; `tab_id` lets C++ reopen the failing
                // tab rather than the active one.
                let evt = serde_json::json!({
                    "type": "event",
                    "method": "restart_agent_stack",
                    "params": { "reason": "auth_recovery", "tab_id": tab_id },
                });
                send_wt_protocol_event(evt.to_string());
                // (iii) Dead-man fallback: if the restart actually respawned
                // this pane, this helper process is gone before the timer
                // fires. If it survives (dropped/slow restart), surface the
                // sign-in screen so the user isn't stranded on "Reconnecting…".
                // Guarded on a live async runtime so unit tests (no LocalSet)
                // don't panic in `spawn_local`.
                if let Some(ref tx) = self.event_tx {
                    if tokio::runtime::Handle::try_current().is_ok() {
                        let tx = tx.clone();
                        tokio::task::spawn_local(async move {
                            tokio::time::sleep(std::time::Duration::from_secs(8)).await;
                            let _ = tx.send(AppEvent::AuthRecoveryTimedOut {
                                agent_id: resolved,
                                generation: recovery_generation,
                            });
                        });
                    }
                }
            }
            AppEvent::AuthRecoveryTimedOut {
                agent_id,
                generation,
            } => {
                // Only reached when the auth-recovery restart did NOT tear this
                // pane down within the window (dropped/slow delivery) — a
                // successful restart kills this helper process first. Surface
                // the sign-in fallback so the user can retry instead of being
                // stranded on a perpetual "Reconnecting…".
                //
                // The generation guard drops a stale timer: if a newer recovery
                // started, or the reconnect already succeeded (AgentConnected
                // bumps the generation), this no longer matches the current
                // recovery and must not force the sign-in screen.
                if generation == self.auth_recovery_generation
                    && self.mode != AppMode::Setup
                    && matches!(self.state, ConnectionState::Connecting(_))
                {
                    tracing::warn!(
                        target: "auth_recovery",
                        agent_id = %agent_id,
                        "auth-recovery restart did not take effect within the window; \
                         falling back to the sign-in screen"
                    );
                    let resolved = if !agent_id.is_empty() {
                        agent_id
                    } else {
                        "copilot".to_string()
                    };
                    self.show_signin_setup_screen(resolved);
                }
            }
            AppEvent::AgentSoftStop { session_id, reason } => {
                use crate::protocol::acp::soft_stop::SoftStopReason;
                // A soft stop is an *outcome*, not a connection failure — the
                // session stays Connected and the turn already closed via
                // AgentMessageEnd. We only append an informational line so the
                // user knows why the reply ended (truncation / budget / refusal)
                // instead of silently trailing off.
                tracing::info!(
                    target: "soft_stop",
                    class = reason.class(),
                    session_id = %session_id,
                    "agent turn ended on a soft stop"
                );
                let msg = match reason {
                    SoftStopReason::MaxTokens => t!("system.stopped_max_tokens"),
                    SoftStopReason::MaxTurnRequests => t!("system.stopped_max_turn_requests"),
                    SoftStopReason::Refusal => t!("system.stopped_refusal"),
                };
                let tab = self.session_tab_mut(&session_id);
                tab.messages.push(ChatMessage::System(msg.into_owned()));
                tab.scroll_to_bottom();
            }
            AppEvent::ExecutionInfo(message) => {
                self.push_execution_info(message);
                self.current_tab_mut().scroll_to_bottom();
            }
            AppEvent::AgentThoughtChunk { session_id, text } => {
                // Late chunk after cancel / completion is dropped by
                // `turn_observe_chunk` (state isn't Submitted/Streaming).
                self.turn_observe_chunk(&session_id, ChunkKind::Thought, &text);
            }
            AppEvent::AgentMessageChunk { session_id, text } => {
                let tab = self.session_tab_mut(&session_id);
                // Late chunks after cancel / completion are dropped by
                // `turn_observe_chunk` (state isn't Submitted/Streaming).
                // During session/load replay no Submitted state exists,
                // so we still need to gate on `loading_session` here to
                // accept replayed chunks into `messages`.
                if !tab.turn.is_in_flight() && !tab.loading_session {
                    return;
                }
                // Turn boundary detection during replay: an agent
                // message chunk after a buffered user_message_chunk
                // means the previous user turn is complete — flush it
                // as a ChatMessage::User so the chat stays in turn
                // order.
                if tab.loading_session && !tab.pending_user_replay.is_empty() {
                    let text = std::mem::take(&mut tab.pending_user_replay);
                    tab.messages.push(ChatMessage::User(text));
                }
                tab.progress_status = None;
                tab.pending_agent_response.push_str(&text);

                // Append to the streaming buffer. The state machine drops
                // late chunks and handles the stale-autofix generation check
                // before returning whether the buffer actually grew.
                let advanced = self.turn_observe_chunk(&session_id, ChunkKind::Message, &text);

                // Surface the card the moment the streamed JSON parses,
                // instead of waiting for AgentMessageEnd (gated behind
                // Copilot's Stop/SessionEnd hooks, ~8s on Windows).
                if advanced {
                    self.turn_try_eager_surface(&session_id);
                }
            }
            AppEvent::UserMessageReplayChunk { session_id, text } => {
                // Replayed historical user prompt from a `session/load`
                // SessionUpdate. Only meaningful during the load window;
                // dropped otherwise. A new user_message_chunk after a
                // buffered agent response marks the turn boundary —
                // flush the previous agent message first.
                let tab = self.session_tab_mut(&session_id);
                if !tab.loading_session {
                    return;
                }
                if !tab.pending_agent_response.is_empty() {
                    let prev = std::mem::take(&mut tab.pending_agent_response);
                    tab.messages.push(ChatMessage::Agent(prev));
                }
                tab.pending_user_replay.push_str(&text);
            }
            AppEvent::AgentMessageEnd { session_id } => {
                if let Some(summary) = self.session_completion_latency_summary(&session_id) {
                    self.push_execution_info(summary);
                }
                self.turn_close(&session_id);
                self.session_tab_mut(&session_id).scroll_to_bottom();
            }
            AppEvent::TimingMetric { session_id, note } => {
                self.session_tab_mut(&session_id).timing_note = Some(note);
            }
            AppEvent::ToolCall {
                session_id,
                id,
                title,
                status,
            } => {
                let tab = self.session_tab_mut(&session_id);
                if !tab.turn.is_in_flight() && !tab.loading_session {
                    return;
                }
                // Turn boundary during replay (see AgentMessageChunk).
                if tab.loading_session {
                    if !tab.pending_user_replay.is_empty() {
                        let text = std::mem::take(&mut tab.pending_user_replay);
                        tab.messages.push(ChatMessage::User(text));
                    }
                    if !tab.pending_agent_response.is_empty() {
                        let text = std::mem::take(&mut tab.pending_agent_response);
                        tab.messages.push(ChatMessage::Agent(text));
                    }
                }
                tab.tool_calls
                    .insert(id.clone(), (title.clone(), status.clone()));
                tab.messages
                    .push(ChatMessage::ToolCall { id, title, status });
                tab.scroll_to_bottom();
            }
            AppEvent::ToolCallUpdate {
                session_id,
                id,
                status,
            } => {
                let tab = self.session_tab_mut(&session_id);
                if !tab.turn.is_in_flight() && !tab.loading_session {
                    return;
                }
                if let Some(entry) = tab.tool_calls.get_mut(&id) {
                    entry.1 = status.clone();
                }
                // Update in-place in messages
                for msg in &mut tab.messages {
                    if let ChatMessage::ToolCall {
                        id: ref mid,
                        status: ref mut s,
                        ..
                    } = msg
                    {
                        if mid == &id {
                            *s = status.clone();
                        }
                    }
                }
            }
            AppEvent::Plan {
                session_id,
                entries,
            } => {
                let tab = self.session_tab_mut(&session_id);
                if !tab.turn.is_in_flight() && !tab.loading_session {
                    return;
                }
                if tab.loading_session {
                    if !tab.pending_user_replay.is_empty() {
                        let text = std::mem::take(&mut tab.pending_user_replay);
                        tab.messages.push(ChatMessage::User(text));
                    }
                    if !tab.pending_agent_response.is_empty() {
                        let text = std::mem::take(&mut tab.pending_agent_response);
                        tab.messages.push(ChatMessage::Agent(text));
                    }
                }
                tab.messages.push(ChatMessage::Plan(entries));
                tab.scroll_to_bottom();
            }
            AppEvent::PermissionRequest {
                session_id,
                description,
                options,
                responder,
            } => {
                let tab = self.session_tab_mut(&session_id);
                if !tab.turn.is_in_flight() && !tab.loading_session {
                    // Auto-deny if the user cancelled before the agent
                    // got around to asking. Dropping the responder yields
                    // a Cancelled outcome on the agent side.
                    return;
                }
                // FIFO push — never overwrite an in-flight request. The
                // user sees them one at a time (front of the queue is the
                // one rendered + key-handled); resolving the front pops
                // it and exposes the next.
                tab.permission.push_back(PermissionState {
                    description,
                    options,
                    selected: 0,
                    responder: Some(responder),
                });
            }
            AppEvent::SystemMessage(message) => {
                self.current_tab_mut()
                    .messages
                    .push(ChatMessage::System(message));
                self.scroll_to_bottom();
            }
            AppEvent::DebugPipeMessage(msg) => {
                self.debug_messages.push(msg);
                // Cap at 500 messages
                if self.debug_messages.len() > 500 {
                    self.debug_messages.remove(0);
                }
            }
            AppEvent::PreflightComplete(result) => {
                tracing::info!(
                    target: "preflight",
                    agent = %result.agent_id,
                    cli_status = ?result.cli_status,
                    auth_status = ?result.auth_status,
                    "preflight result received"
                );
                if !result.all_passed() {
                    let reason = SetupReason::AgentMissing;
                    let current_status = crate::agent_check::check_agent(&result.agent_id);
                    let options = build_setup_options(&reason, Some(&current_status));
                    let title = reason.title().to_string();
                    let subtitle = if current_status.can_auto_install() {
                        t!(
                            "setup.subtitle.copilot_missing",
                            agent = &result.display_name
                        )
                        .into_owned()
                    } else {
                        t!("setup.subtitle.agent_missing", agent = &result.display_name)
                            .into_owned()
                    };
                    self.mode = AppMode::Setup;
                    self.preflight_setup_active = true;
                    self.setup = Some(SetupState {
                        reason,

                        preflight: result,
                        selected_index: 0,
                        install_in_progress: false,
                        install_log: Vec::new(),
                        install_error: None,
                        options,
                        title,
                        subtitle,
                    });
                }
            }
            AppEvent::AgentSessionEvent(ev) => {
                tracing::debug!(
                    target: "agent_session_registry",
                    event = ?ev,
                    "AgentSessionEvent posted from background callback"
                );
                let hook_event = ev.clone();
                self.agent_sessions.apply(ev);
                self.publish_session_hook(hook_event);
            }
            AppEvent::AliveSnapshotLoaded(items) => {
                let count = items.len();
                tracing::info!(
                    target: "alive_mirror",
                    count,
                    "applied master alive-session bootstrap snapshot"
                );

                // B-9: eagerly snapshot `(sid, pane)` tuples and post
                // `AliveJoinUpgrade` so any already-loaded Historical rows
                // get upgraded to Live. Done before the async registry
                // write so we don't depend on the spawned task finishing
                // before the next event handler runs.
                if let Some(tx) = self.event_tx.clone() {
                    let tuples: Vec<(String, Option<String>)> = items
                        .iter()
                        .map(|i| (i.session_id.0.to_string(), i.pane_session_id.clone()))
                        .collect();
                    let _ = tx.send(AppEvent::AliveJoinUpgrade(tuples));
                }

                let reg = std::sync::Arc::clone(&self.alive);
                let loaded = std::sync::Arc::clone(&self.alive_loaded);
                // The registry is async; we cannot await here (sync
                // event-handler context). spawn_local matches the rest
                // of the helper's tokio LocalSet — the registry mutation
                // races nothing else because AliveSession{Added,Removed}
                // events are also serialized through this loop and the
                // bootstrap snapshot is invoked at most once.
                tokio::task::spawn_local(async move {
                    crate::session_registry::apply_snapshot(&*reg, &loaded, items).await;
                });
            }
            AppEvent::AliveSessionAdded(info) => {
                let sid = info.session_id.clone();
                tracing::debug!(
                    target: "alive_mirror",
                    session_id = %sid.0,
                    pane = ?info.pane_session_id,
                    "alive session added by master"
                );
                // Run the incremental join synchronously so a Historical
                // row (loaded from disk) becomes Live the moment master
                // tells us it's alive. Without this, only the bootstrap
                // `AliveSnapshotLoaded` join would upgrade rows — every
                // subsequent `session_added` broadcast would land only
                // in the mirror and the session management row would stay Historical.
                self.agent_sessions
                    .apply_alive_session_join([(sid.0.as_ref(), info.pane_session_id.as_deref())]);
                let reg = std::sync::Arc::clone(&self.alive);
                tokio::task::spawn_local(async move {
                    reg.upsert(info).await;
                });
            }
            AppEvent::AliveSessionRemoved(sid) => {
                tracing::debug!(
                    target: "alive_mirror",
                    session_id = %sid.0,
                    "alive session removed by master"
                );
                // Mirror PaneClosed's reducer for this sid synchronously,
                // before the async mirror update lands. Otherwise, the
                // session management row stays stuck on Live until the next
                // bootstrap, since
                // `apply_alive_pane_snapshot` is only called at startup
                // and `AliveSessionRemoved` had no path into the reducer
                // (the bug rubber-duck Finding 2 surfaced post-B-12).
                self.agent_sessions
                    .apply_master_session_ended(sid.0.as_ref());
                let reg = std::sync::Arc::clone(&self.alive);
                tokio::task::spawn_local(async move {
                    reg.remove(&sid).await;
                });
            }
            AppEvent::AliveJoinUpgrade(tuples) => {
                tracing::debug!(
                    target: "alive_mirror",
                    count = tuples.len(),
                    "running alive×history join (B-9)"
                );
                let pairs: Vec<(&str, Option<&str>)> = tuples
                    .iter()
                    .map(|(s, p)| (s.as_str(), p.as_deref()))
                    .collect();
                self.agent_sessions.apply_alive_session_join(pairs);
            }
            AppEvent::SessionsChanged => {
                self.schedule_agents_refetch_for_open_views();
            }
            AppEvent::AgentsSnapshotLoaded {
                request_id,
                sessions,
            } => {
                self.handle_agents_snapshot_loaded(request_id, sessions);
            }
            AppEvent::AgentsSnapshotFailed { request_id } => {
                self.handle_agents_snapshot_failed(request_id);
            }
            AppEvent::MasterMutationCompleted { request_id } => {
                tracing::debug!(target: "agents_view", request_id, "master mutation completed; refetching open views");
                self.schedule_agents_refetch_for_open_views();
            }
            AppEvent::WtEvent {
                method,
                pane_id,
                tab_id,
                params,
            } => {
                // Per-WT-event (every vt_sequence included) — trace-only; the
                // single per-event breadcrumb stays at debug in main.rs
                // (`wt_event_rx: received event`).
                tracing::trace!(target: "autofix", method = %method, pane_id = %pane_id, tab_id = ?tab_id, self_pane_id = ?self.pane_id, "WtEvent");

                // Hook bridge events: fire-and-forget into the agent registry
                // so the agent session view stays current. Unrelated to autofix /
                // tab routing; runs before the same-pane skip because we want
                // to record events from our own pane too.
                if method == "agent_event" {
                    let mut hook_events = Vec::new();
                    let _ = route_agent_event_to_registry_with_hook_sink(
                        &mut self.agent_sessions,
                        pane_id.as_str(),
                        &params,
                        |event| hook_events.push(event),
                    );
                    for event in hook_events {
                        self.publish_session_hook(event);
                    }
                    // Diagnostics aid: surface the raw event payload in the
                    // active tab's chat so a developer can correlate hook
                    // wire-format with registry behavior. Off by default.
                    if self.log_agent_events {
                        let detail = serde_json::to_string(&params)
                            .unwrap_or_else(|_| "<unserializable>".to_string());
                        self.current_tab_mut()
                            .messages
                            .push(ChatMessage::AgentEvent(detail));
                    }
                    return;
                }

                // autofix_execute is an inbound UI action ("run the armed
                // fix now") from TerminalPage. pane_id is the failing
                // pane — NOT our own — so this check must run before the
                // same-pane skip below. Ignore the event if we don't
                // actually have a cached autofix for that pane.
                if method == "autofix_execute" {
                    self.handle_autofix_execute_request(&pane_id);
                    return;
                }

                if method == "autofix_dismiss_suggestion" {
                    // User clicked the bar in Suggested state. The bar
                    // always projects the active tab, so clear that tab's
                    // suggested_pane_id and emit cleared.
                    let active = self.active_tab_key().to_string();
                    let suggested = self.current_tab_mut().autofix.suggested_pane_id.take();
                    if suggested.is_some() {
                        self.emit_autofix_state_cleared(&active);
                    }
                    return;
                }

                if method == "autofix_execute_from_detected" {
                    // User pressed the pill / hotkey in Detected state.
                    // Replay the trigger as if auto-suggest were on, so
                    // the LLM call fires and we transition to Pending.
                    self.handle_autofix_execute_from_detected();
                    return;
                }

                if method == "agent_prompt" {
                    // Command palette `?<prompt>` delegation. Not a WT
                    // notification — has nothing to do with banner/queue.
                    let prompt = params
                        .get("prompt")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    tracing::info!(target: "autofix", prompt_len = prompt.len(), "agent_prompt: delegating");
                    if !prompt.is_empty() {
                        self.delegate_to_tab_agent(prompt);
                    }
                    return;
                }

                if method == "agent_config_changed" {
                    // C++ pushes this when the user changes a hot-updatable
                    // agent setting (auto-suggest gate, acp-model, delegate
                    // agent/model) while WTA is already running. Unified
                    // dispatch: each field is optional and only present when
                    // it actually changed, so we apply exactly what's set
                    // — all in place, with NO agent-pane teardown/restart.
                    // (Agent *identity* changes go through a master respawn
                    // on the C++ side, not this event.)
                    if let Some(enabled) =
                        params.get("autofix_enabled").and_then(|v| v.as_bool())
                    {
                        tracing::info!(
                            target: "autofix",
                            old = self.autofix_enabled,
                            new = enabled,
                            "autofix_enabled hot-reloaded from settings change",
                        );
                        self.autofix_enabled = enabled;
                    }

                    // delegate_agent + delegate_model travel together so the
                    // delegate runtime table can be rebuilt in one shot.
                    if params.get("delegate_agent").is_some()
                        || params.get("delegate_model").is_some()
                    {
                        let delegate_agent = params
                            .get("delegate_agent")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let delegate_model = params
                            .get("delegate_model")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        self.apply_delegate_config(delegate_agent, delegate_model);
                    }

                    // acp-model: a global settings change is authoritative. It
                    // overrides every pane's local `/model` pick, redirects the
                    // shared current-model display, hot-swaps the model on all
                    // live sessions, and republishes status — so every pane
                    // visibly follows the new model (see apply_global_acp_model).
                    // Storing it also keeps future sessions (/new, lazy-first-
                    // prompt) on the new model via the SessionAttached re-apply.
                    if let Some(raw) = params.get("acp_model").and_then(|v| v.as_str()) {
                        tracing::info!(
                            target: "autofix",
                            model = raw,
                            "acp-model hot-update requested from settings change",
                        );
                        self.apply_global_acp_model(Some(raw.to_string()));
                    }
                    return;
                }

                if method == "tab_changed" {
                    // Window-scoped: WT broadcasts via shared COM, so every
                    // helper (across every window) receives every tab_changed.
                    // Without this filter, helper-A in window 1 would call
                    // switch_tab_session on a window-2 tab_id and start
                    // rendering tab_sessions[<window-2 tab>] in its TUI —
                    // detaching the agent pane content from its owner tab.
                    // Same shape as the `set_agent_state` window filter below:
                    // skip only when both ids are non-empty and differ.
                    let target_window = params
                        .get("window_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let our_window = self.window_id.as_deref().unwrap_or("");
                    if !target_window.is_empty()
                        && !our_window.is_empty()
                        && target_window != our_window
                    {
                        tracing::debug!(
                            target: "tab_session",
                            target_window,
                            our_window,
                            "ignoring tab_changed for different window"
                        );
                        return;
                    }
                    tracing::info!(
                        target: "tab_session",
                        raw_params = %params,
                        current_tab = ?self.tab_id,
                        "tab_changed event received"
                    );
                    if let Some(new_tab_id) = params.get("tab_id").and_then(|v| v.as_str()) {
                        // switch_tab_session calls project_active_tab_state
                        // at its end — that pushes the new tab's view AND
                        // autofix bar snapshot to C++ in one shot.
                        self.switch_tab_session(new_tab_id.to_string());
                    } else {
                        tracing::warn!(target: "tab_session", "tab_changed: missing tab_id in params");
                    }
                    return;
                }

                if method == "tab_closed" {
                    // Same window filter as tab_changed — drop_tab_session
                    // removes from `tab_sessions` and nulls `self.tab_id`
                    // when the closed tab is the active one, so a cross-
                    // window leak would wipe per-tab state of a tab the
                    // helper doesn't even own.
                    let target_window = params
                        .get("window_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let our_window = self.window_id.as_deref().unwrap_or("");
                    if !target_window.is_empty()
                        && !our_window.is_empty()
                        && target_window != our_window
                    {
                        tracing::debug!(
                            target: "tab_session",
                            target_window,
                            our_window,
                            "ignoring tab_closed for different window"
                        );
                        return;
                    }
                    if let Some(closed_tab_id) = params.get("tab_id").and_then(|v| v.as_str()) {
                        self.drop_tab_session(closed_tab_id);
                    } else {
                        tracing::warn!(target: "tab_session", "tab_closed: missing tab_id in params");
                    }
                    return;
                }

                if method == "tab_renamed" {
                    // Tab-drag rename: the user dragged this tab into
                    // another window so WT minted a fresh StableId. The
                    // helper process survives the drag; we just need to
                    // rekey our per-tab maps so events with the new id
                    // route to this tab's existing state. Route through
                    // the AppEvent::TabRenamed handler so the WtEvent
                    // inline path and any direct AppEvent posts share
                    // one implementation.
                    let old_tab_id = params
                        .get("old_tab_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let new_tab_id = params
                        .get("new_tab_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if old_tab_id.is_empty() || new_tab_id.is_empty() {
                        tracing::warn!(
                            target: "tab_session",
                            old_tab_id,
                            new_tab_id,
                            "tab_renamed: missing old_tab_id or new_tab_id in params"
                        );
                        return;
                    }
                    let new_window_id = params
                        .get("window_id")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string());
                    self.handle_event(AppEvent::TabRenamed {
                        old_tab_id: old_tab_id.to_string(),
                        new_tab_id: new_tab_id.to_string(),
                        new_window_id,
                    });
                    return;
                }

                if method == "reset_tab_session" {
                    if let Some(tab_id) = params.get("tab_id").and_then(|v| v.as_str()) {
                        self.reset_tab_session_for(tab_id);
                    } else {
                        tracing::warn!(target: "tab_session", "reset_tab_session: missing tab_id in params");
                    }
                    return;
                }

                // load_session: WT-side replay of WTA's
                // `resume_in_new_agent_tab` request. After WT creates a
                // new tab and reconciles the shared agent pane onto it,
                // it publishes this event with the new tab's StableId,
                // the historical session id, and the cwd. We forward to
                // the ACP client which calls `conn.load_session` and
                // binds the loaded session to the tab via
                // `SessionAttached`. Best-effort: if the agent doesn't
                // recognize the session id (e.g. CLI mismatch), the
                // client emits a `TabError` scoped to this tab. We also
                // pre-switch the target tab back to the Chat view, clear
                // its local chat, and post a "Resuming..." system note
                // so the user sees something even if the agent's
                // session/update replay is delayed or absent.
                if method == "load_session" {
                    let tab_id = params.get("tab_id").and_then(|v| v.as_str()).unwrap_or("");
                    let session_id = params
                        .get("session_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let cwd = params
                        .get("cwd")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .filter(|s| !s.is_empty());
                    tracing::info!(
                        target: "acp_load_session",
                        tab_id,
                        session_id,
                        "inbound load_session event from WT"
                    );
                    if tab_id.is_empty() || session_id.is_empty() {
                        tracing::warn!(
                            target: "acp_load_session",
                            "load_session: missing tab_id or session_id in params"
                        );
                        return;
                    }
                    // Defensive owner_tab_id filter: WT broadcasts
                    // `load_session` over shared COM, so every helper in
                    // every window receives it. Without this filter,
                    // helpers owning a different tab would respond to a
                    // load_session targeted at someone else's pane — the
                    // misroute that bug #1 was about (the legacy resume
                    // flow used to rely on this not filtering, but the
                    // boot-time `--initial-load-session-id` path
                    // (main.rs) is now the canonical way to drive
                    // resumes into a freshly-spawned helper, so a
                    // belt-and-suspenders filter here is safe).
                    if let Some(owner) = self.owner_tab_id.as_deref() {
                        if owner != tab_id {
                            tracing::debug!(
                                target: "acp_load_session",
                                owner,
                                tab_id,
                                "ignoring load_session for non-owner tab"
                            );
                            return;
                        }
                    }
                    {
                        let tab = self.tab_mut(tab_id);
                        tab.current_view = View::Chat;
                        tab.clear_chat_history();
                        tab.completed_turns.clear();
                        tab.selected_completed_turn_idx = None;
                        tab.session_id = None;
                        // Open the replay window: chunk handlers will
                        // now accept session/update events for this
                        // tab even though `turn` stays Idle. Closed by
                        // the SessionAttached handler when the attach
                        // event arrives for THIS specific session id
                        // (unrelated SessionAttached events — e.g. the
                        // bootstrap `session/new` racing with a
                        // boot-time Plan-C initial-load — must not
                        // close it).
                        tab.loading_session = true;
                        tab.loading_target_session_id = Some(session_id.to_string());
                        tab.messages.push(ChatMessage::System(
                            t!(
                                "system.resuming_session",
                                session_id = session_id
                            )
                            .into_owned(),
                        ));
                        tab.scroll_to_bottom();
                    }
                    // If the load_session target IS the active tab, push the
                    // (now Chat) view to C++ so the bar drops the "Agent
                    // sessions" label that the user was looking at when they
                    // hit Shift+Enter on a session row. When the target is a
                    // not-yet-active tab (e.g. WT just created a fresh tab
                    // and the `tab_changed` race still hasn't landed), the
                    // imminent `tab_changed` to that tab will project then.
                    if tab_id == self.active_tab_key() {
                        self.project_active_tab_state();
                    }
                    let _ = self.load_session_tx.send(LoadSessionForTab {
                        tab_id: tab_id.to_string(),
                        session_id: session_id.to_string(),
                        cwd,
                    });
                    return;
                }

                // set_agent_state: unified inbound request from C++ to
                // change one or more pieces of per-tab agent-pane UI state
                // for a specific tab. Every field under `params` is
                // optional — only specified ones are applied, the rest
                // are left untouched.
                //
                // Supported fields:
                //   * `tab_id`: optional WT StableId of the tab to mutate.
                //               Falls back to the active tab when absent.
                //               C++ should always include it: defends
                //               against `tab_changed`/`set_agent_state`
                //               ordering ambiguity (e.g. resume-in-new-tab
                //               creates a new tab and immediately requests
                //               pane_open=true; with `tab_id` we don't
                //               depend on `tab_changed` arriving first to
                //               route to the right TabSession).
                //   * `view`: "chat" | "sessions"
                //   * `pane_open`: bool
                //
                // **Projection rule**: if the target tab is the currently-
                // active one, immediately project the new snapshot back to
                // C++ (`agent_state_changed`). If the target is NOT active,
                // skip projection — the next `tab_changed` to that tab will
                // project the now-up-to-date state. C++ mirrors are global
                // per-pane so they only need refreshing when the active tab
                // changes (or when a mutation lands on the active tab).
                //
                // **Round-trip contract**: under the "wta is the sole owner
                // of agent-pane UI state" architecture, C++ does NOT update
                // its mirrors (`_agentSessionsViewActive`, `Tab.AgentPaneOpen`)
                // when it sends `set_agent_state`. It waits for the resulting
                // `agent_state_changed` emitted by `project_active_tab_state`
                // below. One IPC round-trip latency, in exchange for the
                // C++ flags having a single writer (`OnAgentStateChanged`),
                // which makes desync architecturally impossible.
                //
                // Window-scoped: WT includes its own window_id; we ignore
                // the event when our window_id is known and doesn't match,
                // so multi-window setups don't cross-talk. When window_id
                // is unknown on either side we apply (best-effort fallback).
                //
                // Processed BEFORE the own-pane skip below: this is a
                // global UI command, not a per-pane signal.
                if method == "set_agent_state" {
                    let target_window = params
                        .get("window_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let our_window = self.window_id.as_deref().unwrap_or("");
                    if !target_window.is_empty()
                        && !our_window.is_empty()
                        && target_window != our_window
                    {
                        tracing::debug!(
                            target: "set_agent_state",
                            target_window,
                            our_window,
                            "ignoring set_agent_state for different window"
                        );
                        return;
                    }

                    // Resolve target tab: explicit `tab_id` wins;
                    // otherwise fall back to the active tab. The explicit
                    // path is robust against `tab_changed` ordering races
                    // (e.g. resume-in-new-tab where C++ creates a tab and
                    // immediately fires `set_agent_state` for it).
                    let target_tab = params
                        .get("tab_id")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| self.active_tab_key().to_string());

                    // Apply `view` if present.
                    if let Some(view_str) = params.get("view").and_then(|v| v.as_str()) {
                        tracing::info!(
                            target: "set_agent_state",
                            tab = %target_tab,
                            view = view_str,
                            "applying view"
                        );
                        match view_str {
                            "sessions" | "agents" => {
                                // User entered session management (via shortcut or UI) —
                                // permanently dismiss the welcome hint.
                                if self.show_welcome_hint {
                                    self.show_welcome_hint = false;
                                    set_welcome_shown_in_state();
                                }
                                self.open_agents_view_for_tab(target_tab.clone());
                            }
                            "chat" => {
                                self.close_agents_view_for_tab(&target_tab);
                            }
                            other => {
                                tracing::warn!(
                                    target: "set_agent_state",
                                    view = other,
                                    "unknown view value — ignoring"
                                );
                            }
                        }
                    }

                    // Apply `pane_open` if present.
                    if let Some(open) = params.get("pane_open").and_then(|v| v.as_bool()) {
                        tracing::info!(
                            target: "set_agent_state",
                            tab = %target_tab,
                            pane_open = open,
                            "applying pane_open"
                        );
                        self.tab_mut(&target_tab).pane_open = open;
                        // If a result is waiting for review on this tab,
                        // re-project the bar: opening the pane makes the
                        // result visible (→ Idle, bar goes quiet), closing
                        // it brings the Review hint back. The open/closed →
                        // Idle/Review decision lives entirely here in the
                        // helper, not in C++.
                        if let Some(review_pane) = self
                            .tab_sessions
                            .get(&target_tab)
                            .and_then(|t| t.autofix.suggested_pane_id.clone())
                        {
                            self.emit_autofix_state_result(&target_tab, &review_pane);
                        }
                    }

                    // Always echo the mutation back — C++ routes
                    // `agent_state_changed` by `tab_id`, so per-tab state
                    // updates apply to the right AgentPaneContent
                    // regardless of which tab is currently focused.
                    self.project_tab_state(&target_tab);
                    return;
                }

                // Skip events from our own pane
                if self.pane_id.as_deref() == Some(pane_id.as_str()) {
                    tracing::debug!(target: "autofix", "skipped: own pane");
                    return;
                }

                // Bridge WT-native `connection_state` events into the agent
                // session registry so rows transition out of live states
                // (Idle/Working/...) when the underlying pane dies. The
                // hook-bridge path (`agent.session.end` → `SessionStopped`)
                // handles Claude/Copilot, but Gemini has no end-of-session
                // hook, so without this wire a Gemini row spawned via session management view
                // resume stays Idle forever after the user types `/exit`.
                //
                // Both event variants are no-ops in the registry when
                // `pane_id` isn't bound to any agent session, so this is
                // safe to apply unconditionally for non-own panes.
                if method == "connection_state" {
                    let state = params.get("state").and_then(|v| v.as_str()).unwrap_or("");
                    tracing::info!(
                        target: "helper_wt_event",
                        pane_id = %pane_id,
                        state,
                        self_pane = ?self.pane_id,
                        "helper observed WT connection_state event"
                    );
                    match state {
                        "closed" => {
                            // Capture the key BEFORE PaneClosed clears
                            // the pane→key binding, so the log can report
                            // which row was demoted.
                            let key_before = self
                                .agent_sessions
                                .key_for_pane(&pane_id);
                            let event = crate::agent_sessions::SessionEvent::PaneClosed {
                                pane_session_id: pane_id.clone(),
                            };
                            self.agent_sessions.apply(event.clone());
                            self.publish_session_hook(event);
                            tracing::info!(
                                target: "helper_wt_event",
                                pane_id = %pane_id,
                                key_before = ?key_before,
                                "helper applied PaneClosed locally + published to master"
                            );
                        }
                        "failed" => {
                            let reason = params
                                .get("reason")
                                .and_then(|v| v.as_str())
                                .unwrap_or("connection failed")
                                .to_string();
                            let event = crate::agent_sessions::SessionEvent::ConnectionFailed {
                                pane_session_id: pane_id.clone(),
                                reason,
                            };
                            self.agent_sessions.apply(event.clone());
                            self.publish_session_hook(event);
                        }
                        _ => {}
                    }
                }

                // Detect agent CLI exit when the pane stays alive (e.g. user
                // typed `gemini` inside their pwsh/cmd shell, then `/exit`):
                // the shell emits `osc:133;A` (FinalTerm prompt-start) when
                // it returns to its own prompt. If the pane is currently
                // bound to an agent session, treat that as the agent's
                // teardown signal and transition the row to Ended.
                //
                // We deliberately do NOT depend on the agent's own SessionEnd
                // hook here because:
                //   * Gemini has no reliable hook on `/exit`
                //     (`agent.session.end` is "Hook cancelled" most of the time)
                //   * Even when the hook fires, it races with our event loop
                //
                // The shell's prompt-start marker is the most reliable
                // cross-CLI signal that the agent process has released the
                // foreground.
                if method == "vt_sequence" {
                    let seq = params
                        .get("sequence")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    // Gate OSC 133;A → PaneClosed on the bound
                    // session's ORIGIN, not just "is_agent_pane".
                    //
                    // Background: this handler exists to detect agent
                    // exit in SHELL panes (user typed `gemini` in pwsh,
                    // agent ran, user `/exit`'d, shell returns to its
                    // prompt → OSC 133;A fires → we treat that as the
                    // agent's teardown signal). For those sessions
                    // origin is `Unknown`.
                    //
                    // For agent panes proper (origin AgentPane) there
                    // is NO shell underneath the conpty — the helper
                    // TUI is the direct child. Yet WT itself can
                    // emit OSC 133;A around focus/window-switch on
                    // arbitrary panes (observed in `wtcli focus-pane`
                    // round trips). The previous gate
                    // `is_agent_pane(pane_id)` matched any pane with
                    // ANY bound session and demoted the row to Ended
                    // even though the agent CLI was happily still
                    // streaming notifications — the user sees this as
                    // "session management Enter on a Live row spawned a new pane
                    // instead of focusing the existing one" because
                    // the row demoted between snapshot and Enter.
                    //
                    // Restrict to origin=Unknown so the heuristic
                    // keeps working for its original shell-pane use
                    // case without nuking agent panes.
                    let origin = self.agent_sessions.origin_for_pane(&pane_id);
                    let is_shell_agent = matches!(origin, Some(crate::agent_sessions::SessionOrigin::Unknown));
                    if seq == "osc:133;A" && is_shell_agent {
                        tracing::info!(
                            target: "agent_session_registry",
                            pane_id = %pane_id,
                            "shell prompt-start in agent-bound pane: treating as agent exit",
                        );
                        let event = crate::agent_sessions::SessionEvent::PaneClosed {
                            pane_session_id: pane_id.clone(),
                        };
                        self.agent_sessions.apply(event.clone());
                        self.publish_session_hook(event);
                    }
                }

                let notification = classify_wt_event(&method, &pane_id, tab_id.as_deref(), &params);
                // Per-WT-event classification — trace-only (vt_sequence volume).
                tracing::trace!(target: "autofix", severity = ?notification.severity, summary = %notification.summary, tab_id = ?notification.tab_id, "classified");

                // Per-tab filter. WT broadcasts pane-scoped events to every
                // helper in the window, but another tab's failures are not
                // this helper's concern. Drop notifications whose tab_id
                // doesn't match our owner_tab_id; empty/missing tab_id falls
                // through (no per-tab scope).
                if let (Some(event_tab), Some(self_tab)) = (
                    notification.tab_id.as_deref(),
                    self.owner_tab_id.as_deref(),
                ) {
                    if !event_tab.is_empty()
                        && !self_tab.is_empty()
                        && event_tab != self_tab
                    {
                        // Per-cross-tab-event (very high volume in multi-tab
                        // windows) — trace-only.
                        tracing::trace!(
                            target: "autofix",
                            event_tab,
                            self_tab,
                            method = %method,
                            "dropping cross-tab WT event"
                        );
                        return;
                    }
                }

                // Telemetry: emit ErrorDetected for any non-acknowledged
                // critical/actionable classification. Acknowledged events are
                // the auto-silenced "unknown"/"connected"/success cases.
                if !notification.acknowledged {
                    let severity_str = match notification.severity {
                        WtEventSeverity::Critical => Some("Critical"),
                        WtEventSeverity::Actionable => Some("Actionable"),
                        WtEventSeverity::Informational => None,
                    };
                    if let Some(severity_str) = severity_str {
                        crate::telemetry::log_error_detected(
                            severity_str,
                            &method,
                            &pane_id,
                        );
                    }
                }

                // Surface rule: WT events (connection_state, vt_sequence)
                // surface via the bottom bar / `wt_notifications` queue ONLY.
                // Chat is the agent dialogue surface — only user input and
                // agent responses go there.
                match notification.severity {
                    WtEventSeverity::Critical | WtEventSeverity::Actionable => {
                        self.show_notification_banner = true;
                        // Only OSC-133;D vt_sequence events have the exit
                        // code + live shell buffer needed to drive autofix.
                        // `connection_state: closed`/`failed` is just process
                        // termination — banner-only.
                        if method == "vt_sequence" {
                            self.maybe_trigger_autofix(&notification);
                        }
                    }
                    WtEventSeverity::Informational => {
                        // "User moved past this prompt" = dismiss. Two signals
                        // both count as "moved on":
                        //   * exit-zero (D;0): the user ran any successful
                        //     command in the failing pane.
                        //   * prompt-start (A): the shell drew a fresh prompt
                        //     line (user pressed Enter, switched away, etc.).
                        // For Pending/Armed/Detected we gate prompt-start on
                        // `trigger_echo_pane` so the immediate A that
                        // PowerShell emits ~1ms after every D doesn't
                        // dismiss the state we just established. Suggested
                        // fires asynchronously (after the LLM returns), so
                        // it has no echo to skip and dismisses on any A.
                        if method == "vt_sequence" {
                            let seq = params
                                .get("sequence")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            let is_exit_zero = seq
                                .strip_prefix("osc:133;")
                                .and_then(|rest| rest.strip_prefix("D;"))
                                .and_then(|code| code.trim().parse::<i32>().ok())
                                .map(|c| c == 0)
                                .unwrap_or(false);
                            let is_prompt_start = seq == "osc:133;A";
                            // Resolve the event's owning tab (added in Step 1).
                            // Older events without tab_id can't be cleanly
                            // routed; skip the per-tab clear for them.
                            let event_tab = tab_id.clone();
                            // Consume the trigger-echo flag if this A is the
                            // one PowerShell emits immediately after the
                            // triggering D. `effective_prompt_start` is the
                            // "user actually moved on" signal for D-synchronous
                            // states (Pending / Detected). Suggested uses raw
                            // `is_prompt_start` since it fires post-LLM.
                            let effective_prompt_start = if is_prompt_start {
                                if let Some(t) = event_tab.as_deref() {
                                    let echo = self
                                        .tab_mut(&t.to_string())
                                        .autofix
                                        .trigger_echo_pane
                                        .clone();
                                    if echo.as_deref() == Some(pane_id.as_str()) {
                                        self.tab_mut(&t.to_string())
                                            .autofix
                                            .trigger_echo_pane = None;
                                        false
                                    } else {
                                        true
                                    }
                                } else {
                                    true
                                }
                            } else {
                                false
                            };
                            let armed_in_event_tab = event_tab
                                .as_deref()
                                .and_then(|t| self.tab_sessions.get(t))
                                .and_then(|t| t.autofix.pane_id.as_deref())
                                .map(str::to_string);
                            if (is_exit_zero || effective_prompt_start)
                                && armed_in_event_tab.as_deref() == Some(pane_id.as_str())
                            {
                                let target_tab = event_tab
                                    .clone()
                                    .expect("armed_in_event_tab requires tab_id present");
                                // Telemetry: a fix was armed for this pane and the next
                                // command exited cleanly — the user's problem resolved.
                                // Elapsed is monotonic (`Instant::elapsed`) from arm to
                                // clean exit, not wall-clock.
                                if let Some(armed) = self
                                    .tab_mut(&target_tab)
                                    .autofix
                                    .armed_at
                                    .take()
                                {
                                    let elapsed_ms = armed.elapsed().as_secs_f64() * 1000.0;
                                    crate::telemetry::log_error_fix_resolved(
                                        pane_id.as_str(),
                                        elapsed_ms,
                                    );
                                }
                                // `turn_cancel` owns the full cleanup: bumps
                                // the tab's autofix_generation, emits cleared
                                // (resolving the pane from AutofixContext, or
                                // `autofix.pane_id` as a fallback), and
                                // resets `tab.turn` to Idle. Avoid duplicating
                                // its work.
                                let session_id = self
                                    .tab_sessions
                                    .get(&target_tab)
                                    .and_then(|t| t.session_id.clone());
                                if let Some(sid) = session_id {
                                    self.turn_cancel(&sid);
                                } else {
                                    // No ACP session bound — replicate the
                                    // minimum cleanup turn_cancel would do.
                                    let pane_to_clear = {
                                        let tab = self.tab_mut(&target_tab);
                                        tab.autofix.generation =
                                            tab.autofix.generation.wrapping_add(1);
                                        tab.clear_recommendations();
                                        tab.autofix.pane_id.take()
                                    };
                                    if pane_to_clear.is_some() {
                                        self.emit_autofix_state_cleared(&target_tab);
                                    }
                                }
                            }
                            // Suggested: dismiss on prompt activity (exit-zero
                            // or a fresh prompt-start) in the event's tab.
                            // Emit cleared so the bar's per-tab snapshot
                            // resets to Idle.
                            if is_exit_zero || is_prompt_start {
                                if let Some(t) = event_tab.as_deref() {
                                    let t_owned = t.to_string();
                                    let pane_to_clear =
                                        self.tab_mut(&t_owned).autofix.suggested_pane_id.take();
                                    if pane_to_clear.is_some() {
                                        self.emit_autofix_state_cleared(&t_owned);
                                    }
                                }
                            }
                            // Detected (suggest-mode pill): dismiss when the
                            // user moves on in the same pane — either a
                            // successful command (exit-zero) or a fresh
                            // prompt-start that isn't the trigger's echo.
                            // The Detected snapshot has no in-flight turn
                            // to cancel — just clear the bar.
                            if is_exit_zero || effective_prompt_start {
                                if let Some(t) = event_tab.as_deref() {
                                    let t_owned = t.to_string();
                                    let detected_matches = matches!(
                                        &self.tab_mut(&t_owned).autofix.bar_snapshot,
                                        AutofixBarSnapshot::Detected { pane_id: bar_pane, .. }
                                            if bar_pane == pane_id.as_str()
                                    );
                                    if detected_matches {
                                        self.emit_autofix_state_cleared(&t_owned);
                                    }
                                }
                            }
                        }
                    }
                }

                // Queue the notification (cap at 20)
                self.wt_notifications.push_back(notification);
                if self.wt_notifications.len() > 20 {
                    self.wt_notifications.pop_front();
                }
            }
            AppEvent::AgentInstallComplete => {
                // Check if the agent we were trying to install is now available.
                let agent_id = self
                    .setup
                    .as_ref()
                    .map(|s| s.preflight.agent_id.clone())
                    .unwrap_or_default();

                if !agent_id.is_empty() {
                    let status = crate::agent_check::check_agent(&agent_id);
                    if status.cli_found {
                        // Install succeeded → proceed to connect or auth
                        let profile = crate::agent_registry::lookup_profile_by_id(&agent_id);

                        if crate::agent_check::has_credential(&agent_id) {
                            // Has credential → connect directly
                            let new_cmd = self.build_agent_cmd(&agent_id);
                            let _ = self.restart_tx.send(RestartRequest {
                                agent_cmd: Some(new_cmd),
                            });
                            self.mode = AppMode::Chat;
                            self.state =
                                ConnectionState::Connecting(t!("connection.starting").into_owned());
                            let tab = self.current_tab_mut();
                            tab.messages.retain(|m| !matches!(m, ChatMessage::Error(_)));
                            tab.chat_scroll.reset();
                            self.setup = None;
                            let (enterprise_mode, enterprise_host) =
                                copilot_enterprise_prefill(&agent_id);
                            self.auth = Some(AuthState {
                                agent_id: agent_id.clone(),
                                agent_name: status.display_name.clone(),
                                login_command: crate::agent_check::build_login_cmd(&agent_id, None),
                                checking: false,
                                status_message: String::new(),
                                enterprise_mode,
                                enterprise_host,
                            });
                        } else if agent_id == "copilot" {
                            // Copilot installed but not authenticated → auth screen.
                            self.mode = AppMode::Auth;
                            self.setup = None;
                            let (enterprise_mode, enterprise_host) =
                                copilot_enterprise_prefill(&agent_id);
                            self.auth = Some(AuthState {
                                agent_id: agent_id.clone(),
                                agent_name: status.display_name.clone(),
                                login_command: crate::agent_check::build_login_cmd(&agent_id, None),
                                checking: false,
                                status_message: String::new(),
                                enterprise_mode,
                                enterprise_host,
                            });
                        } else {
                            // Future-proofing: only Copilot has an in-app auth
                            // screen. If another agent ever becomes
                            // auto-installable, keep it on the diagnostic setup
                            // retry path instead of entering Auth mode.
                            let reason = SetupReason::AgentError;
                            let options = build_setup_options(&reason, Some(&status));
                            self.mode = AppMode::Setup;
                            self.setup = Some(SetupState {
                                reason,
                                selected_index: 0,
                                preflight: PreflightResult {
                                    agent_id: agent_id.clone(),
                                    display_name: status.display_name.clone(),
                                    cli_status: CheckStatus::Passed,
                                    cli_path: status.cli_path.clone(),
                                    auth_status: CheckStatus::Failed(
                                        t!("system.authentication_failed").into_owned(),
                                    ),
                                    install_hint: profile.install_hint.to_string(),
                                    install_url: String::new(),
                                    auth_hint: profile.auth_hint.to_string(),
                                },
                                install_in_progress: false,
                                install_log: Vec::new(),
                                install_error: None,
                                options,
                                title: t!("setup.title.sign_in").into_owned(),
                                subtitle: t!(
                                    "setup.subtitle.agent_auth",
                                    agent = status.display_name.as_str()
                                )
                                .into_owned(),
                            });
                        }
                        return;
                    }
                }

                // Install didn't resolve the issue — stay on setup, refresh options
                if let Some(ref mut setup) = self.setup {
                    setup.install_in_progress = false;
                    let current_status = if !agent_id.is_empty() {
                        Some(crate::agent_check::check_agent(&agent_id))
                    } else {
                        None
                    };
                    setup.options = build_setup_options(&setup.reason, current_status.as_ref());
                }
            }
            AppEvent::LoginProgress {
                device_code,
                verify_url,
            } => {
                // Only reflect device-flow progress while an auth attempt is
                // actively checking. A late event after the user left the
                // screen (auth = None) must not write status or copy a device
                // code to the clipboard.
                if let Some(ref mut auth) = self.auth {
                    if auth.checking {
                        auth.status_message = t!(
                            "auth.device_code_prompt",
                            url = verify_url.as_str(),
                            code = device_code.as_str()
                        )
                        .into_owned();
                        // Copy device code to clipboard
                        let code_to_copy = device_code.clone();
                        tokio::task::spawn_blocking(move || {
                            if let Err(e) = crate::win32::copy_text_to_clipboard(&code_to_copy) {
                                tracing::warn!(
                                    target: "clipboard",
                                    error = %e,
                                    "failed to copy Copilot device code to clipboard"
                                );
                            }
                        });
                    }
                }
            }
            AppEvent::LoginComplete { success, error, agent_id } => {
                tracing::info!("LoginComplete received: success={} deferred_acp={}", success, self.deferred_acp.is_some());
                // Ignore stale/late completions: only act on a completion that
                // matches the currently active auth attempt. After the user
                // escapes the auth screen (auth = None) or switches agents, a
                // late background login must not force Chat mode, start ACP for
                // the wrong/empty agent, or rewrite another screen's status.
                let active = self
                    .auth
                    .as_ref()
                    .map(|a| a.agent_id == agent_id)
                    .unwrap_or(false);
                if !active {
                    tracing::info!(
                        "LoginComplete ignored (no matching active auth attempt) agent={}",
                        agent_id
                    );
                    return;
                }
                if success {
                    // Login succeeded → transition to Chat and start ACP
                    self.mode = AppMode::Chat;
                    self.setup = None;
                    self.state =
                        ConnectionState::Connecting(t!("connection.starting").into_owned());
                    self.update_deferred_acp_agent(&agent_id);
                    // If deferred_acp is None (helper mode — the initial
                    // ACP client already exited with auth error and dropped
                    // its channels), create a fresh DeferredAcpParams so
                    // try_start_acp can spawn a new ACP client.
                    if self.deferred_acp.is_none() {
                        let new_cmd = self.build_agent_cmd(&agent_id);
                        tracing::info!("LoginComplete: creating deferred_acp for reconnect cmd={}", new_cmd);
                        self.deferred_acp = Some(DeferredAcpParams {
                            agent_cmd: new_cmd,
                            acp_model: None,
                            prompt_rx: None, // try_start_acp will create fresh channels
                            cancel_rx: None,
                            new_session_rx: None,
                            load_session_rx: None,
                            drop_session_rx: None,
                            rename_session_rx: None,
                            restart_rx: None,
                            master_ext_rx: None,
                            shell_mgr: Arc::clone(&self.shell_mgr),
                            wt_connected: self.wt_connected,
                            master_pipe_name: None,
                            owner_tab_id: None,
                        });
                    }
                    self.pending_acp_start = true;
                    self.needs_post_login_authenticate = true;
                    self.auth = None;
                } else {
                    // Login failed — show auth screen again with feedback.
                    if let Some(ref mut auth) = self.auth {
                        auth.checking = false;
                        // Copilot device-flow failed (e.g. an unreachable
                        // GitHub Enterprise host) — surface the reason instead
                        // of silently returning to the form.
                        auth.status_message = error
                            .filter(|e| !e.trim().is_empty())
                            .unwrap_or_else(|| t!("system.authentication_failed").into_owned());
                    }
                }
            }
        }
    }

    fn event_requires_redraw(&self, event: &AppEvent) -> bool {
        match event {
            AppEvent::Tick => self.has_activity_indicator() || self.show_notification_banner,
            // The reveal animation only needs a frame while there is still
            // unrevealed pending text on the *visible* tab. When the reveal
            // has caught up (or nothing is streaming) this is a cheap no-op
            // tick that doesn't redraw — so idle/no-backlog costs nothing.
            AppEvent::RevealTick => self.has_reveal_backlog(),
            AppEvent::AgentMessageChunk { .. } => true,
            AppEvent::DebugPipeMessage(_) => self.show_debug_panel,
            _ => true,
        }
    }

    /// Number of *user-visible* characters in a tab's streaming buffer, i.e.
    /// the length of what the renderer would show in full. `None` when the
    /// tab is not streaming visible prose.
    fn tab_visible_stream_len(tab: &TabSession) -> Option<usize> {
        let buf = tab.turn.buffer()?;
        crate::ui::chat::user_visible_stream_text(buf).map(|t| t.chars().count())
    }

    /// True iff the current (visible) tab has streaming text that the reveal
    /// cursor hasn't caught up to yet. Used to gate `RevealTick` redraws.
    fn has_reveal_backlog(&self) -> bool {
        let tab = self.current_tab();
        matches!(Self::tab_visible_stream_len(tab), Some(len) if tab.reveal_chars < len)
    }

    /// Advance the typewriter reveal cursor on every streaming tab. The step
    /// is *adaptive*: it grows with the backlog so the reveal can never fall
    /// permanently behind a fast model — any backlog is drained within
    /// `REVEAL_CATCHUP_FRAMES` ticks. Combined with the fact that finalize
    /// commits the message in full (un-gated), this guarantees the smoothing
    /// never increases the total time for the response to appear: it only
    /// redistributes *when* characters show up within the streaming window.
    fn advance_reveal(&mut self) {
        // ~30fps tick. `REVEAL_MIN_STEP` is the floor so a slow trickle still
        // animates; the `backlog / REVEAL_CATCHUP_FRAMES` term speeds up to
        // match (and overtake) arrival, capping the visible lag at roughly
        // `REVEAL_CATCHUP_FRAMES` ticks (~130ms).
        const REVEAL_MIN_STEP: usize = 3;
        const REVEAL_CATCHUP_FRAMES: usize = 4;
        for tab in self.tab_sessions.values_mut() {
            let Some(len) = Self::tab_visible_stream_len(tab) else {
                continue;
            };
            if tab.reveal_chars >= len {
                // Clamp down if the visible text shrank (e.g. a fenced JSON
                // block replaced the streamed prose).
                tab.reveal_chars = len;
                continue;
            }
            let backlog = len - tab.reveal_chars;
            let step = REVEAL_MIN_STEP.max(backlog / REVEAL_CATCHUP_FRAMES);
            tab.reveal_chars = (tab.reveal_chars + step).min(len);
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        // Per-keystroke and carries the raw `KeyCode` (the typed character for
        // `Char` keys) — the user's prompt can be reconstructed from this
        // stream. Trace only so it never persists in shipping (info) or
        // default-debug logs.
        tracing::trace!(
            target: "input",
            code = ?key.code,
            modifiers = ?key.modifiers,
            input_empty = self.current_tab().input.is_empty(),
            recs = self.current_tab().turn.recommendations().is_some(),
            turns = self.current_tab().completed_turns.len(),
            selected_turn = ?self.current_tab().selected_completed_turn_idx,
            "key received"
        );

        // Any non-Ctrl+C key disarms the close-pane sequence. We allow plain
        // Ctrl presses (modifier-only events) through so the user can still
        // hold Ctrl while preparing to tap C the second time. The Ctrl+C
        // arm-or-fire transitions itself are handled in the match below.
        let is_ctrl_c =
            matches!(key.code, KeyCode::Char('c')) && key.modifiers.contains(KeyModifiers::CONTROL);
        if !is_ctrl_c {
            self.close_pane_armed_at = None;
            // Don't clear `transient_hint` here — it has its own deadline and
            // ui::render checks expiry on each draw. Clearing on every key
            // would steal too much of the hint's visible lifetime.
        }

        // Setup mode: diagnostic install/sign-in/retry flow.
        if self.mode == AppMode::Setup {
            self.handle_setup_key(key);
            return;
        }

        // Auth mode: Enter to sign in, Esc to go back
        if self.mode == AppMode::Auth {
            match key.code {
                // GitHub Enterprise sign-in (Copilot): [E] reveals a domain
                // input; while it's open, typed chars edit the domain and
                // Backspace deletes. (Esc collapses it — handled below.)
                KeyCode::Char('e') | KeyCode::Char('E')
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && !key.modifiers.contains(KeyModifiers::ALT)
                        && self
                            .auth
                            .as_ref()
                            .map(|a| !a.checking && a.agent_id == "copilot" && !a.enterprise_mode)
                            .unwrap_or(false) =>
                {
                    if let Some(ref mut auth) = self.auth {
                        auth.enterprise_mode = true;
                        // Starting a fresh enterprise attempt: drop any prior
                        // failure/progress text so it doesn't show in the domain
                        // input (e.g. a leftover github.com "Login failed").
                        auth.status_message.clear();
                    }
                }
                KeyCode::Char(c)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && !key.modifiers.contains(KeyModifiers::ALT)
                        && self
                            .auth
                            .as_ref()
                            .map(|a| !a.checking && a.enterprise_mode)
                            .unwrap_or(false) =>
                {
                    if !c.is_whitespace() {
                        if let Some(ref mut auth) = self.auth {
                            auth.enterprise_host.push(c);
                        }
                    }
                }
                KeyCode::Backspace
                    if self
                        .auth
                        .as_ref()
                        .map(|a| !a.checking && a.enterprise_mode)
                        .unwrap_or(false) =>
                {
                    if let Some(ref mut auth) = self.auth {
                        auth.enterprise_host.pop();
                    }
                }
                KeyCode::Enter => {
                    // Extract values before borrowing self again
                    let login_info = self.auth.as_ref().and_then(|a| {
                        if !a.checking && !a.login_command.is_empty() {
                            // In enterprise mode, a non-empty domain drives a
                            // `--host` sign-in; otherwise the default github.com.
                            let host = if a.enterprise_mode {
                                let h = a.enterprise_host.trim();
                                if h.is_empty() {
                                    None
                                } else {
                                    Some(h.to_string())
                                }
                            } else {
                                None
                            };
                            Some((a.agent_id.clone(), a.login_command.clone(), host))
                        } else {
                            None
                        }
                    });
                    if let Some((agent_id, _login_cmd, host)) = login_info {
                        if agent_id != "copilot" {
                            tracing::warn!(
                                target: "login",
                                agent = %agent_id,
                                "ignoring auth Enter for non-Copilot agent"
                            );
                            return;
                        }
                        // Copilot: auto device-flow sign-in via piped stdio.
                        // Rebuild the command with the (optional) GitHub
                        // Enterprise host and remember it for next time.
                        let login_cmd =
                            crate::agent_check::build_login_cmd(&agent_id, host.as_deref());
                        // Remember the last-used host for next time. Persist
                        // the *normalized* bare domain (or "" for github.com /
                        // empty) so a returning user is prefilled only for a
                        // real GHE domain — not stuck in the expanded
                        // enterprise input after a github.com fallback.
                        let normalized_host = host
                            .as_deref()
                            .and_then(crate::agent_check::normalize_enterprise_host);
                        crate::agent_check::save_copilot_enterprise_host(
                            normalized_host.as_deref().unwrap_or(""),
                        );
                        self.begin_auth_checking();
                        tracing::info!(
                            target: "login",
                            agent = %agent_id,
                            enterprise = host.is_some(),
                            host = host.as_deref().unwrap_or("github.com"),
                            cmd = %login_cmd,
                            "starting copilot device-flow login"
                        );
                        self.spawn_login(&agent_id, &login_cmd);
                    }
                }
                KeyCode::Esc => {
                    // In the GHE domain input, Esc collapses back to the
                    // github.com sign-in choice rather than leaving the screen.
                    if self
                        .auth
                        .as_ref()
                        .map(|a| a.enterprise_mode && !a.checking)
                        .unwrap_or(false)
                    {
                        if let Some(ref mut auth) = self.auth {
                            auth.enterprise_mode = false;
                            // Collapsing back to the github.com choice abandons
                            // the enterprise attempt — clear its failure/progress
                            // text so it doesn't linger on the collapsed screen.
                            auth.status_message.clear();
                        }
                        return;
                    }
                    if self.setup.is_some() {
                        // Go back to setup screen
                        self.mode = AppMode::Setup;
                    } else {
                        // No setup state to go back to (e.g. preflight auth failure) —
                        // rebuild setup as AgentMissing for this agent
                        let agent_id = self
                            .auth
                            .as_ref()
                            .map(|a| a.agent_id.clone())
                            .unwrap_or_default();
                        if !agent_id.is_empty() {
                            let agent_status = crate::agent_check::check_agent(&agent_id);
                            let profile = crate::agent_registry::lookup_profile_by_id(&agent_id);
                            let reason = SetupReason::AgentError;
                            let options = build_setup_options(&reason, Some(&agent_status));
                            self.mode = AppMode::Setup;
                            self.setup = Some(SetupState {
                                reason,

                                selected_index: 0,
                                preflight: PreflightResult {
                                    agent_id: agent_id.clone(),
                                    display_name: profile.display_name.to_string(),
                                    cli_status: CheckStatus::Passed,
                                    cli_path: None,
                                    auth_status: CheckStatus::Failed(
                                        t!("system.authentication_failed").into_owned(),
                                    ),
                                    install_hint: profile.install_hint.to_string(),
                                    install_url: String::new(),
                                    auth_hint: profile.auth_hint.to_string(),
                                },
                                install_in_progress: false,
                                install_log: Vec::new(),
                                install_error: None,
                                options,
                                title: t!("setup.title.sign_in").into_owned(),
                                subtitle: if profile.id == "copilot" {
                                    t!("setup.subtitle.copilot_auth", agent = profile.display_name)
                                        .into_owned()
                                } else {
                                    t!("setup.subtitle.agent_auth", agent = profile.display_name)
                                        .into_owned()
                                },
                            });
                        } else {
                            self.mode = AppMode::Chat;
                        }
                    }
                    self.auth = None;
                }
                _ => {}
            }
            return;
        }

        // agent session view: list navigation + Enter to focus pane + Delete
        // to evict an Ended/Historical row. Captures all input while open
        // — including Esc which closes the view. View open-state and the
        // selection cursor are per-tab on `TabSession` so each WT tab
        // keeps its own picker state across switches.
        if self.current_tab().current_view == View::Agents {
            let tab_id = self.active_tab_key().to_string();
            let rows = self.agents_rows_for_tab(&tab_id);
            let count = rows.len();
            match key.code {
                KeyCode::Down => {
                    let cur = self.current_tab().agents_list_state.selected().unwrap_or(0);
                    let next = if count == 0 {
                        0
                    } else {
                        (cur + 1).min(count - 1)
                    };
                    self.current_tab_mut().agents_list_state.select(Some(next));
                    self.update_agents_focus_for_tab(&tab_id);
                }
                KeyCode::Up => {
                    let cur = self.current_tab().agents_list_state.selected().unwrap_or(0);
                    self.current_tab_mut()
                        .agents_list_state
                        .select(Some(cur.saturating_sub(1)));
                    self.update_agents_focus_for_tab(&tab_id);
                }
                KeyCode::Enter => {
                    if let Some(idx) = self.current_tab().agents_list_state.selected() {
                        let selected = rows.get(idx).cloned();
                        if let Some(s) = selected {
                            // B-10: route through the unified
                            // state-machine dispatcher. Shift flips
                            // the default per-origin (see
                            // session_mgmt::decide_enter_action) —
                            // Live rows ignore Shift; dead rows use
                            // it as an escape hatch to the *other*
                            // resume style.
                            let shift = key.modifiers.contains(KeyModifiers::SHIFT);
                            self.activate_agent_session_with_shift(&s, shift);
                        }
                    }
                }
                KeyCode::Delete => {
                    if self.current_tab().agents_view.snapshot.is_some() {
                        return;
                    }
                    if let Some(idx) = self.current_tab().agents_list_state.selected() {
                        let target = rows.get(idx).map(|s| (s.key.clone(), s.status.clone()));
                        if let Some((key, status)) = target {
                            use crate::agent_sessions::AgentStatus::*;
                            // Evicting a live session would orphan its pane,
                            // so restrict Delete to terminal states. Live
                            // rows transition to Ended via SessionStopped.
                            if matches!(status, Ended | Historical) {
                                self.agent_sessions.remove(&key);
                                // Keep the cursor in-bounds after eviction.
                                // Re-query through the same filters so the
                                // selection clamp matches the rendered list
                                // (both cli + MVP origin filter).
                                let new_count = self
                                    .agent_sessions
                                    .iter_sorted_with_filters(
                                        self.current_cli_filter().as_ref(),
                                        self.sessions_origin_filter,
                                    )
                                    .len();
                                let tab = self.current_tab_mut();
                                if new_count == 0 {
                                    tab.agents_list_state.select(None);
                                } else if idx >= new_count {
                                    tab.agents_list_state.select(Some(new_count - 1));
                                }
                            }
                        }
                    }
                }
                KeyCode::Esc => {
                    let tab_id = self.active_tab_key().to_string();
                    // Restore the pane visibility the user had *before* they
                    // entered session management. Read before any mutation.
                    // Falls back to "stay open" (the legacy Esc behaviour) if
                    // nothing was captured.
                    let restore_open = self
                        .current_tab()
                        .agents_view_prev_pane_open
                        .unwrap_or(true);
                    if restore_open {
                        // Entered from an expanded chat pane → return to it:
                        // switch the TUI back to chat, leave the pane visible.
                        self.close_agents_view_for_tab(&tab_id);
                        self.tab_mut(&tab_id).pane_open = true;
                    } else {
                        // Entered from a folded (stashed) pane → re-fold.
                        // Deliberately do NOT switch to chat here: if we did,
                        // the helper would re-render the chat view for a frame
                        // while the pane is still on screen, so the user sees
                        // the agent pane flash before C++ stashes it. Keeping
                        // the session list rendered lets the pane stash
                        // straight from it. Clear the snapshot so a later
                        // re-entry re-captures; the lingering Agents view
                        // self-heals to chat on the next chat-toggle open.
                        let tab = self.tab_mut(&tab_id);
                        tab.pane_open = false;
                        tab.agents_view_prev_pane_open = None;
                    }
                    self.project_active_tab_state();
                }
                KeyCode::F(5) => {
                    // Refresh: ask master to re-scan the on-disk historical
                    // session logs (load_for_cli) like the startup seed, then
                    // re-list. The sticky pending_rescan flag is consumed when
                    // schedule actually dispatches, so it survives in-flight
                    // coalescing.
                    let tab_id = self.active_tab_key().to_string();
                    self.tab_mut(&tab_id).agents_view.pending_rescan = true;
                    self.schedule_agents_refetch_for_tab(&tab_id);
                }
                _ => {}
            }
            return;
        }

        // If permission card is showing, route keys there. Buttons are
        // rendered horizontally inside the embedded card (same chrome as
        // recommendations), so Left/Right move the focus; Up/Down kept as
        // aliases for muscle memory from the prior modal.
        if let Some(perm) = self.current_tab_mut().permission.front_mut() {
            match key.code {
                KeyCode::Left | KeyCode::Up => {
                    if perm.selected > 0 {
                        perm.selected -= 1;
                    }
                }
                KeyCode::Right | KeyCode::Down => {
                    if perm.selected < perm.options.len().saturating_sub(1) {
                        perm.selected += 1;
                    }
                }
                KeyCode::Enter => {
                    let option_id = perm.options[perm.selected].id.clone();
                    // Pop the resolved entry; the next queued request (if
                    // any) automatically becomes the new front and is
                    // rendered on the next frame.
                    if let Some(perm) = self.current_tab_mut().permission.pop_front() {
                        if let Some(responder) = perm.responder {
                            let _ = responder.send(option_id);
                        } else {
                            let _ = self.permission_tx.send(option_id);
                        }
                    }
                }
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    // Quick allow: find first allow option
                    if let Some(idx) = perm.allow_index() {
                        let option_id = perm.options[idx].id.clone();
                        if let Some(perm) = self.current_tab_mut().permission.pop_front() {
                            if let Some(responder) = perm.responder {
                                let _ = responder.send(option_id);
                            } else {
                                let _ = self.permission_tx.send(option_id);
                            }
                        }
                    }
                }
                KeyCode::Char('n') | KeyCode::Char('N') => {
                    // Quick deny: find first reject option
                    if let Some(idx) = perm.reject_index() {
                        let option_id = perm.options[idx].id.clone();
                        if let Some(perm) = self.current_tab_mut().permission.pop_front() {
                            if let Some(responder) = perm.responder {
                                let _ = responder.send(option_id);
                            } else {
                                let _ = self.permission_tx.send(option_id);
                            }
                        }
                    }
                }
                _ => {}
            }
            return;
        }

        // Model picker modal (`/model`): while it's up, arrows move the
        // highlight, Enter commits the pick, Esc dismisses. Swallow every
        // other key so nothing leaks into the input box behind the modal.
        if self.model_picker_visible() {
            match key.code {
                KeyCode::Up => self.model_picker_up(),
                KeyCode::Down => self.model_picker_down(),
                KeyCode::Enter => self.commit_model_pick(),
                KeyCode::Esc => self.close_model_picker(),
                _ => {}
            }
            return;
        }

        match key.code {
            KeyCode::Up if self.current_tab().turn.recommendations().is_some() =>
            {
                if self.current_tab_mut().selected_recommendation > 0 {
                    self.current_tab_mut().selected_recommendation -= 1;
                    self.current_tab_mut().selected_button = self.default_button_for_selected();
                    self.scroll_rec_to_selected(self.main_area_width());
                    // Selection moved — the new card may target a different
                    // pane (or have no Send action), so re-pin the chip.
                    let tab_id = self.active_tab_key().to_string();
                    self.recompute_chip_override(&tab_id);
                }
            }
            KeyCode::Down if self.current_tab().turn.recommendations().is_some() =>
            {
                let choices_len = self
                    .current_tab()
                    .turn
                    .recommendations()
                    .map(|r| r.choices.len())
                    .unwrap_or(0);
                if self.current_tab().selected_recommendation + 1 < choices_len {
                    let default_btn = self.default_button_for_selected();
                    self.current_tab_mut().selected_recommendation += 1;
                    self.current_tab_mut().selected_button = default_btn;
                    self.scroll_rec_to_selected(self.main_area_width());
                    let tab_id = self.active_tab_key().to_string();
                    self.recompute_chip_override(&tab_id);
                }
            }
            // Wheel-as-arrow scroll fallback: when the input is empty and no
            // recommendation card is active, ↑/↓ scroll the chat transcript.
            // The host terminal (WT, xterm, kitty, …) translates a mouse-wheel
            // notch into ~3 arrow keystrokes when mouse capture is OFF and the
            // app is in the alt-screen buffer, so by(1)/by(-1) per key matches
            // one wheel notch ≈ 3 lines, in line with the previous explicit
            // MouseScroll handler. Convention here mirrors PgUp/PgDn below:
            // positive delta = scroll up (toward older content).
            KeyCode::Up
                if self.current_tab().input.is_empty()
                    && self.current_tab().turn.recommendations().is_none()
                    && !self.command_popup_visible() =>
            {
                self.current_tab_mut().chat_scroll.by(1);
            }
            KeyCode::Down
                if self.current_tab().input.is_empty()
                    && self.current_tab().turn.recommendations().is_none()
                    && !self.command_popup_visible() =>
            {
                self.current_tab_mut().chat_scroll.by(-1);
            }
            KeyCode::Right | KeyCode::Tab
                if self.current_tab().turn.recommendations().is_some() =>
            {
                // Cycle button focus forward within the selected card.
                // Send: 0=Run, 1=Insert. OpenAndSend has only index 0.
                let button_count = self.button_count_for_selected();
                if button_count > 1 {
                    self.current_tab_mut().selected_button =
                        (self.current_tab_mut().selected_button + 1) % button_count;
                }
            }
            KeyCode::Tab
                if self.current_tab().input.is_empty()
                    && self.current_tab().turn.recommendations().is_none()
                    && !self.current_tab().completed_turns.is_empty() =>
            {
                self.current_tab_mut().select_older_completed_turn();
            }
            KeyCode::BackTab
                if self.current_tab().input.is_empty()
                    && self.current_tab().turn.recommendations().is_none()
                    && !self.current_tab().completed_turns.is_empty() =>
            {
                self.current_tab_mut().select_newer_completed_turn();
            }
            KeyCode::Esc if self.current_tab().selected_completed_turn_idx.is_some() => {
                // Esc clears the past-turn selection without any other side
                // effect. Lets the user back out of the history nav cleanly.
                self.current_tab_mut().selected_completed_turn_idx = None;
            }
            KeyCode::Left | KeyCode::BackTab
                if self.current_tab().turn.recommendations().is_some() =>
            {
                // Cycle button focus backward.
                let button_count = self.button_count_for_selected();
                if button_count > 1 {
                    self.current_tab_mut().selected_button =
                        (self.current_tab_mut().selected_button + button_count - 1) % button_count;
                }
            }
            KeyCode::F(12) => {
                self.show_debug_panel = !self.show_debug_panel;
                self.debug_capture_enabled
                    .store(self.show_debug_panel, Ordering::Relaxed);
                self.debug_scroll = 0;
                return;
            }
            KeyCode::PageUp
                if key.modifiers.contains(KeyModifiers::SHIFT) && self.show_debug_panel =>
            {
                self.debug_scroll = self.debug_scroll.saturating_add(10);
                return;
            }
            KeyCode::PageDown
                if key.modifiers.contains(KeyModifiers::SHIFT) && self.show_debug_panel =>
            {
                self.debug_scroll = self.debug_scroll.saturating_sub(10);
                return;
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                // In-flight: state is Submitted/Streaming or Surfaced{end_pending}.
                let in_flight = !self.current_tab().turn.is_idle()
                    && !matches!(
                        self.current_tab().turn,
                        TurnState::Surfaced {
                            end_pending: false,
                            ..
                        }
                    );
                if in_flight {
                    // Send a session/cancel to the ACP client. The client
                    // will fire the protocol notification and signal the
                    // per-prompt oneshot so the spawned task drops out of
                    // conn.prompt() immediately.
                    let session_id = self.current_tab().session_id.clone();
                    if let Some(sid) = session_id.clone() {
                        let _ = self.cancel_tx.send(CancelRequest { session_id: sid });
                    }
                    if let Some(sid) = session_id {
                        self.turn_cancel(&sid);
                    }
                    let tab = self.current_tab_mut();
                    tab.messages
                        .push(ChatMessage::System(t!("system.cancelled").into_owned()));
                    tab.scroll_to_bottom();
                    self.close_pane_armed_at = None;
                } else if !self.current_tab().input.is_empty() {
                    // Mirror bash readline: Ctrl+C clears the buffer.
                    self.current_tab_mut().clear_input();
                    self.close_pane_armed_at = None;
                } else {
                    // Idle + empty input. First press arms; second press
                    // within CLOSE_PANE_ARM_WINDOW asks WT to close the
                    // pane. We never set should_quit ourselves — the pane
                    // teardown will kill our ConPty, which is the only
                    // path that should terminate wta.
                    let now = std::time::Instant::now();
                    let armed = self
                        .close_pane_armed_at
                        .map(|t| now.duration_since(t) < CLOSE_PANE_ARM_WINDOW)
                        .unwrap_or(false);
                    if armed {
                        self.close_pane_armed_at = None;
                        self.transient_hint = None;
                        self.request_close_agent_pane();
                    } else {
                        self.close_pane_armed_at = Some(now);
                        self.transient_hint = Some((
                            "Press Ctrl+C again to close pane".to_string(),
                            now + CLOSE_PANE_ARM_WINDOW,
                        ));
                    }
                }
            }
            KeyCode::Esc if self.help_overlay_visible => {
                self.help_overlay_visible = false;
            }
            KeyCode::Esc if self.show_notification_banner => {
                self.dismiss_notifications();
            }
            KeyCode::Esc
                if self.current_tab().turn.recommendations().is_some()
                    || (self.current_tab().autofix.pane_id.is_some()
                        && !self.current_tab().turn.is_idle()) =>
            {
                // Dismiss armed fix card or cancel in-flight autofix request.
                // `turn_cancel` bumps generation, emits autofix_state_cleared,
                // and resets the state machine to Idle.
                let session_id = self.current_tab().session_id.clone();
                if let Some(sid) = session_id {
                    self.turn_cancel(&sid);
                } else {
                    // No session attached yet — fall back to manual cleanup
                    // (no chunks can be in flight in that case).
                    let pane_to_clear = {
                        let tab = self.current_tab_mut();
                        tab.autofix.generation = tab.autofix.generation.wrapping_add(1);
                        tab.autofix.armed_at = None;
                        tab.autofix.pane_id.take()
                    };
                    if pane_to_clear.is_some() {
                        let active = self.active_tab_key().to_string();
                        self.emit_autofix_state_cleared(&active);
                    }
                }
            }
            // Dismiss the bottom-bar Suggested indicator (autofix produced an
            // explanation, not an executable fix). Reachable only when the user
            // is interacting with this TUI — i.e. the agent pane is currently
            // visible. Other dismiss paths: clicking the bar (opens pane), or
            // any prompt activity in any pane (exit-zero or osc:133;A).
            //
            // NOTE: this only handles the default-tui (single-process) mode.
            // In shared-host attach mode `suggested_pane_id` lives on the host;
            // the attach client would need to send a HostCommand::DismissSuggestion.
            // TODO: wire that path when shared-host mode is exercised.
            KeyCode::Esc if self.current_tab().autofix.suggested_pane_id.is_some() => {
                self.current_tab_mut().autofix.suggested_pane_id = None;
                let active = self.active_tab_key().to_string();
                self.emit_autofix_state_cleared(&active);
            }
            KeyCode::Esc => {
                self.current_tab_mut().clear_input();
            }
            KeyCode::Up if self.command_popup_visible() => {
                self.current_tab_mut().command_popup_up();
            }
            KeyCode::Down if self.command_popup_visible() => {
                self.current_tab_mut().command_popup_down();
            }
            KeyCode::Tab if self.command_popup_visible() => {
                self.current_tab_mut().accept_command_popup_completion();
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                // Input editing only acts when the input is the live caret
                // target. While a recommendation/permission card or a past
                // turn is highlighted the input is locked (see Char below).
                if self.current_tab().input_has_nav_focus() {
                    self.current_tab_mut().insert_input_char('\n');
                }
            }
            KeyCode::Enter
                if self.current_tab().input.is_empty()
                    && self.current_tab().selected_completed_turn_idx.is_some()
                    && self.current_tab().turn.recommendations().is_none() =>
            {
                // A past turn is highlighted via Tab — Enter toggles its
                // expanded state instead of submitting / activating recs.
                self.current_tab_mut().toggle_selected_completed_turn();
            }
            KeyCode::Enter => {
                if self.current_tab().turn.recommendations().is_some() {
                    // Card is visible — it owns focus even when the input box
                    // already has draft text. Keep the draft intact and route
                    // Enter to the selected card action instead of submitting
                    // or slash-parsing the input.
                    if self.state == ConnectionState::Connected {
                        let session_id = self.current_tab().session_id.clone();
                        if let Some(session_id) = session_id {
                            let label_choice = self
                                .selected_recommendation_choice()
                                .map(|c| c.choice)
                                .unwrap_or(0);
                            let insert_only = self.current_tab().selected_button == 1
                                && self
                                    .selected_recommendation_choice()
                                    .map(|c| self.is_send_choice(c))
                                    .unwrap_or(false);
                            tracing::info!(
                                target: "autofix",
                                choice = label_choice,
                                insert_only,
                                "Executing choice",
                            );
                            let label = if insert_only {
                                "Inserting"
                            } else {
                                "Executing"
                            };
                            self.push_execution_info(format!("{} choice {}.", label, label_choice));
                            self.turn_execute_card(&session_id);
                        }
                    }
                    return;
                }

                // Slash-command intercept (popup selection, known command, or
                // unknown-command warning). Runs before the prompt path so
                // commands like /stop work even mid-flight, and /help / /clear
                // work even when the agent isn't Connected. Returns true when
                // the keystroke was consumed; an unknown command only warns and
                // falls through so the raw line still goes to the agent.
                if self.try_handle_slash_on_enter() {
                    return;
                }
                let _tab = self.current_tab();
                tracing::debug!(target: "autofix", input_empty = _tab.input.is_empty(), state = ?self.state, has_recs = _tab.turn.recommendations().is_some(), autofix_pane = ?_tab.autofix.pane_id, selected_idx = _tab.selected_recommendation, "Enter");
                if (!self.current_tab().input.is_empty()
                    || !self.current_tab().pending_images.is_empty())
                    && self.state == ConnectionState::Connected
                {
                    // Same-tab single-flight: refuse a new prompt if the
                    // turn isn't accepting one. The ACP transport rejects
                    // too, but bouncing here keeps the user's input intact.
                    if !self.current_tab().turn.accepts_new_prompt() {
                        let tab = self.current_tab_mut();
                        tab.messages
                            .push(ChatMessage::System(t!("system.agent_busy").into_owned()));
                        tab.scroll_to_bottom();
                        return;
                    }
                    let tab = self.current_tab_mut();
                    let text = std::mem::take(&mut tab.input);
                    // Drain any Alt+V images queued for this prompt.
                    let images = std::mem::take(&mut tab.pending_images);
                    tab.cursor_pos = 0;
                    tab.refresh_command_popup();
                    // `session_id` may be None on a brand-new tab whose ACP
                    // session is created lazily by `dispatch_prompt_body`.
                    // Fall back to a key that `session_tab_mut`'s
                    // `tab_for_session` resolves to the active tab — same
                    // trick as `maybe_trigger_autofix` — so the state
                    // machine still installs the turn on this tab. When
                    // `SessionAttached` later writes the real session id,
                    // subsequent chunks route here correctly.
                    let session_id = tab
                        .session_id
                        .clone()
                        .unwrap_or_else(|| DEFAULT_TAB_ID.to_string());
                    let pane_context = PaneContext {
                        pane_id: self.pane_id.clone(),
                        tab_id: self.tab_id.clone(),
                        window_id: self.window_id.clone(),
                        cwd: None,
                        source_pane_id: None,
                    };
                    // The echoed user message shows a marker for each queued
                    // image; the ACP text block stays raw (the image rides as a
                    // separate ContentBlock::Image).
                    let display_text = if images.is_empty() {
                        text.clone()
                    } else {
                        let items = images
                            .iter()
                            .enumerate()
                            .map(|(i, im)| format!("[{}] {}", i + 1, im.label))
                            .collect::<Vec<_>>()
                            .join(", ");
                        let marker = t!("input.image_attachments", items = items).into_owned();
                        if text.is_empty() {
                            marker
                        } else {
                            format!("{text}\n{marker}")
                        }
                    };
                    let prompt =
                        PromptSubmission::new(text.clone(), Some(pane_context)).with_images(images);
                    prompt_timing_log(
                        prompt.id,
                        prompt.submitted_at_unix_s,
                        "ui_submit",
                        &format!("preview={:?}", prompt.preview()),
                    );
                    if self.show_welcome_hint {
                        self.show_welcome_hint = false;
                        set_welcome_shown_in_state();
                    }
                    let submitted = SubmittedPrompt {
                        id: prompt.id,
                        text: display_text,
                        submitted_at_unix_s: prompt.submitted_at_unix_s,
                        autofix: None,
                    };
                    self.turn_submit_prompt(&session_id, submitted);
                    let _ = self.prompt_tx.send(prompt);
                }
            }
            KeyCode::Backspace if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.current_tab().input_has_nav_focus() {
                    self.current_tab_mut().delete_word_before_cursor();
                }
            }
            KeyCode::Backspace => {
                if self.current_tab().input_has_nav_focus() {
                    self.current_tab_mut().delete_before_cursor();
                }
            }
            KeyCode::Delete => {
                if self.current_tab().input_has_nav_focus() {
                    self.current_tab_mut().delete_at_cursor();
                }
            }
            KeyCode::Left if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.current_tab_mut().move_cursor_word_left();
            }
            KeyCode::Right if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.current_tab_mut().move_cursor_word_right();
            }
            KeyCode::Left => {
                self.current_tab_mut().move_cursor_left();
            }
            KeyCode::Right => {
                self.current_tab_mut().move_cursor_right();
            }
            KeyCode::Home => {
                self.current_tab_mut().move_cursor_home();
            }
            KeyCode::End => {
                self.current_tab_mut().move_cursor_end();
            }
            KeyCode::PageUp => {
                self.current_tab_mut().chat_scroll.by(10);
            }
            KeyCode::PageDown => {
                self.current_tab_mut().chat_scroll.by(-10);
            }
            KeyCode::Char('v') | KeyCode::Char('V')
                if key.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.handle_paste_image();
            }
            KeyCode::Char(c) => {
                // Only type into the input when it is the live caret target.
                // When a recommendation/permission card or a past turn is
                // highlighted the input is locked: keystrokes are ignored so
                // the buffer can't fill invisibly (no caret) and strand the
                // user (a non-empty buffer disables Tab/↑ history nav). Press
                // Esc, or Tab/Shift+Tab back past the ends, to return focus.
                if self.current_tab().input_has_nav_focus() {
                    self.current_tab_mut().insert_input_char(c);
                }
            }
            _ => {}
        }
    }

    fn scroll_to_bottom(&mut self) {
        self.current_tab_mut().scroll_to_bottom();
    }

    /// Alt+V: capture an image from the Windows clipboard and queue it to send
    /// with the next prompt. Gated on the input being the live caret target and
    /// on the agent advertising the `image` prompt capability — otherwise the
    /// user gets a clear system message instead of a silently-rejected image.
    fn handle_paste_image(&mut self) {
        if !self.current_tab().input_has_nav_focus() {
            return;
        }
        if !self.agent_supports_image {
            let tab = self.current_tab_mut();
            tab.messages.push(ChatMessage::System(
                t!("system.image_not_supported").into_owned(),
            ));
            tab.scroll_to_bottom();
            return;
        }
        match crate::clipboard_image::read_clipboard_image() {
            Some(image) => {
                let label = image.label.clone();
                let tab = self.current_tab_mut();
                tab.pending_images.push(image);
                tab.messages.push(ChatMessage::System(
                    t!("system.image_pasted", label = label).into_owned(),
                ));
                tab.scroll_to_bottom();
            }
            None => {
                let tab = self.current_tab_mut();
                tab.messages.push(ChatMessage::System(
                    t!("system.image_clipboard_empty").into_owned(),
                ));
                tab.scroll_to_bottom();
            }
        }
    }

    /// True while the open agents view should show the loading shimmer: either
    /// waiting on its first `session/list` reply from master (empty placeholder
    /// snapshot + refetch in flight) or while an F5 rescan is in flight. Drives
    /// the shimmer animation tick so a refresh is visible.
    fn agents_view_awaiting_snapshot(&self) -> bool {
        let tab = self.current_tab();
        if tab.current_view != View::Agents {
            return false;
        }
        // First-snapshot OR an F5 rescan; a normal 5s poll keeps
        // rescan_in_flight false so it doesn't flash the shimmer.
        tab.agents_view.refetch_in_flight
            && (tab
                .agents_view
                .snapshot
                .as_deref()
                .map(|s| s.is_empty())
                .unwrap_or(false)
                || tab.agents_view.rescan_in_flight)
    }

    fn has_activity_indicator(&self) -> bool {
        if self.mode == AppMode::Setup || self.mode == AppMode::Auth {
            return true; // spinner always ticks in setup/auth mode
        }
        if self.agents_view_awaiting_snapshot() {
            return true; // agents-view "Loading" shimmer
        }
        let tab = self.current_tab();
        tab.turn.spinner_label().is_some() || tab.progress_status.is_some()
    }

    /// Get the most recent unacknowledged notification (for the banner).
    #[allow(dead_code)]
    pub fn active_notification(&self) -> Option<&WtNotification> {
        self.wt_notifications.iter().rev().find(|n| !n.acknowledged)
    }

    /// Count of unacknowledged actionable/critical notifications.
    #[allow(dead_code)]
    pub fn unacknowledged_count(&self) -> usize {
        self.wt_notifications
            .iter()
            .filter(|n| !n.acknowledged && n.severity != WtEventSeverity::Informational)
            .count()
    }

    /// Dismiss the notification banner and mark all current notifications as acknowledged.
    pub fn dismiss_notifications(&mut self) {
        self.show_notification_banner = false;
        for n in self.wt_notifications.iter_mut() {
            n.acknowledged = true;
        }
    }

    /// Get the latest status-bar badge text (if any unacknowledged notification exists).
    #[allow(dead_code)]
    pub fn notification_badge(&self) -> Option<(&str, &WtEventSeverity)> {
        // Show the most severe unacknowledged notification
        self.wt_notifications
            .iter()
            .rev()
            .find(|n| !n.acknowledged)
            .map(|n| (n.summary.as_str(), &n.severity))
    }

    /// Visible popup state for the renderer. Returns `None` when the
    /// popup should not be drawn this frame. Reads from the active tab.
    pub fn command_popup_state(&self) -> Option<crate::ui::PopupState<'_>> {
        let tab = self.current_tab();
        if tab.command_popup_candidates.is_empty() {
            return None;
        }
        // When the transport to master is lost, only /restart can run — so the
        // popup simply doesn't show the other commands (rather than greying
        // them). Collapse the candidate list to /restart if it's among the
        // prefix matches; otherwise show nothing (the typed prefix excludes
        // it, e.g. "/new"), and the Enter handler surfaces the reconnect hint.
        // Normal path borrows the tab's list (no per-frame allocation on the
        // render hot path); only the degraded filter allocates.
        let candidates: std::borrow::Cow<'_, [&'static crate::commands::CommandSpec]> =
            if self.transport_lost {
                let filtered: Vec<&'static crate::commands::CommandSpec> = tab
                    .command_popup_candidates
                    .iter()
                    .copied()
                    .filter(|s| s.kind == crate::commands::CommandKind::Restart)
                    .collect();
                if filtered.is_empty() {
                    return None;
                }
                std::borrow::Cow::Owned(filtered)
            } else {
                std::borrow::Cow::Borrowed(tab.command_popup_candidates.as_slice())
            };
        Some(crate::ui::PopupState {
            candidates,
            selected: tab.command_popup_selected,
            current_model: self.current_model_display(),
        })
    }

    /// Display label for the active pane's effective model — its per-pane
    /// `/model` override if set, else the global `current_model_id` — using
    /// the agent's friendly name when known and falling back to the raw id.
    /// `None` when no model is known yet (nothing to append).
    fn current_model_display(&self) -> Option<String> {
        let id = self
            .current_tab()
            .model_override
            .clone()
            // Prefer the per-pane override, then the agent's reported active
            // model, and finally the global `acpModel` setting as a hint for
            // the window before the agent reports `current_model_id` (or when
            // only the global override is in effect). Empty acp_model means
            // "agent default" and contributes nothing.
            .or_else(|| self.current_model_id.clone())
            .or_else(|| self.acp_model.clone())
            .filter(|s| !s.trim().is_empty())?;
        let name = self
            .available_models
            .iter()
            .find(|m| m.id == id)
            .map(|m| m.name.clone())
            .unwrap_or(id);
        Some(name)
    }

    /// Whether the command popup is *effectively* visible — i.e. actually
    /// rendered. This is the same condition `command_popup_state()` uses to
    /// decide whether to draw, so key handlers gate on the real on-screen
    /// state: in degraded mode the candidate list is filtered to `/restart`,
    /// so when the typed prefix excludes it (e.g. `/new`) nothing is drawn and
    /// this returns false — the Up/Down/Tab/Enter arms then fall through to
    /// their normal behavior instead of swallowing the key against an
    /// invisible popup.
    fn command_popup_visible(&self) -> bool {
        if !self.current_tab().command_popup_visible() {
            return false;
        }
        if self.transport_lost {
            // Only /restart is offered; if the prefix excludes it the popup
            // isn't drawn.
            return self
                .current_tab()
                .command_popup_candidates
                .iter()
                .any(|s| s.kind == crate::commands::CommandKind::Restart);
        }
        true
    }

    /// Per-frame state for the `/model` picker modal, or `None` when it's not
    /// open on the active tab. Sources the list from the agent's advertised
    /// `available_models` and marks the pane's currently-effective model.
    pub fn model_popup_state(&self) -> Option<crate::ui::ModelPopupState<'_>> {
        let tab = self.current_tab();
        if !tab.model_picker_open || self.available_models.is_empty() {
            return None;
        }
        // Same precedence as `current_model_display`: override → agent's
        // reported model → global `acpModel`, so the picker marks the pane's
        // effective model even before the agent reports `current_model_id`.
        let current_id = tab
            .model_override
            .as_deref()
            .or(self.current_model_id.as_deref())
            .or(self.acp_model.as_deref());
        Some(crate::ui::ModelPopupState {
            models: &self.available_models,
            selected: tab.model_picker_selected,
            current_id,
        })
    }

    /// Handle Enter for the slash-command system. Centralizes all three
    /// intents in one place so the giant `handle_key` match has a single
    /// guard instead of an inline block plus a separate popup arm:
    ///
    /// 1. Autocomplete popup open → run the highlighted command.
    /// 2. No popup → [`commands::classify`] the committed line:
    ///    - known command → dispatch it,
    ///    - unknown `/foo` → warn but leave the input for the prompt path,
    ///    - plain prompt → do nothing.
    ///
    /// Returns `true` when the keystroke is fully consumed (a command ran or
    /// the popup swallowed Enter); `false` means the caller should continue to
    /// the normal prompt-submission path with the input intact.
    fn try_handle_slash_on_enter(&mut self) -> bool {
        // 1. Popup open: Enter commits the highlighted command (`/`, `/h`,
        //    `/he` → /help) and never submits the raw text as a prompt, so
        //    this arm is always consumed even if there is no selection.
        if self.command_popup_visible() {
            // When the transport to master is lost, only /restart is runnable
            // (everything else would hit the dead pipe). Pick the /restart
            // spec if it's in the filtered candidate list; otherwise there's
            // nothing to run, so consume Enter and show the reconnect hint.
            let spec = if self.transport_lost {
                self.current_tab()
                    .command_popup_candidates
                    .iter()
                    .copied()
                    .find(|s| s.kind == CommandKind::Restart)
            } else {
                self.current_tab().selected_command_spec()
            };
            match spec {
                Some(spec) => {
                    let parsed = ParsedCommand {
                        kind: spec.kind,
                        spec,
                        rest: String::new(),
                    };
                    self.current_tab_mut().clear_input();
                    self.handle_slash_command(parsed);
                }
                None => {
                    self.current_tab_mut().clear_input();
                    if self.transport_lost {
                        self.push_degraded_command_hint();
                    }
                }
            }
            return true;
        }

        // 2. No popup: classify the committed line.
        if self.current_tab().input.is_empty() {
            return false;
        }
        match commands::classify(&self.current_tab().input) {
            ParseOutcome::Command(cmd) => {
                // Degraded: a typed command other than /restart can't run
                // against the dead pipe — swallow it with the reconnect hint.
                if self.transport_lost && cmd.kind != CommandKind::Restart {
                    self.current_tab_mut().clear_input();
                    self.push_degraded_command_hint();
                    return true;
                }
                self.current_tab_mut().clear_input();
                self.handle_slash_command(cmd);
                true
            }
            ParseOutcome::Unknown(name) => {
                // Warn but fall through: the raw line (leading `/` intact) is
                // still sent so the user doesn't lose what they typed.
                let tab = self.current_tab_mut();
                tab.messages.push(ChatMessage::System(
                    t!("system.unknown_command", command = name.as_str()).into_owned(),
                ));
                false
            }
            ParseOutcome::NotCommand => false,
        }
    }

    /// Append the localized "connection to the agent was lost — /restart to
    /// reconnect" line to the active tab. Shown when the user invokes any
    /// slash command other than /restart while the transport to master is
    /// down (reuses the existing `connection.lost` string).
    fn push_degraded_command_hint(&mut self) {
        let msg = t!("connection.lost").into_owned();
        self.current_tab_mut().messages.push(ChatMessage::System(msg));
    }

    /// Dispatch a parsed slash-command. The Enter handler is responsible
    /// for clearing the input and cursor before calling this.
    fn handle_slash_command(&mut self, cmd: ParsedCommand) {
        let in_flight = self.current_tab().turn.is_in_flight();
        tracing::info!(
            target: "slash_cmd",
            name = cmd.spec.name,
            in_flight,
            "dispatch"
        );

        // Transport to master is lost — only /restart can recover (it routes
        // via wtcli→COM, not the dead pipe). Refuse everything else with the
        // reconnect hint so a command can never silently fail against a dead
        // connection. This is the defensive backstop; the Enter handler and
        // greyed popup already steer the user here.
        if self.transport_lost && cmd.kind != CommandKind::Restart {
            self.push_degraded_command_hint();
            return;
        }

        // Thin dispatch: each arm's logic lives in a `cmd_*` method so a
        // single command can be read and unit-tested in isolation. `in_flight`
        // is computed once here and threaded to the commands that branch on it.
        match cmd.kind {
            CommandKind::Help => self.cmd_help(),
            CommandKind::Clear => self.cmd_clear(),
            CommandKind::Stop => self.cmd_stop(in_flight),
            CommandKind::New => self.cmd_new(in_flight),
            CommandKind::Fix => self.cmd_fix(in_flight, cmd.rest),
            CommandKind::Sessions => self.cmd_sessions(),
            CommandKind::Restart => self.cmd_restart(),
            CommandKind::Model => self.cmd_model(cmd.rest),
        }
    }

    /// `/help` — toggle the help overlay.
    fn cmd_help(&mut self) {
        self.help_overlay_visible = !self.help_overlay_visible;
    }

    /// `/clear` — wipe the active tab's chat history and completed turns.
    fn cmd_clear(&mut self) {
        let tab = self.current_tab_mut();
        tab.clear_chat_history();
        tab.completed_turns.clear();
        tab.selected_completed_turn_idx = None;
        tab.scroll_to_bottom();
    }

    /// `/stop` — cancel the in-flight turn, or note that there is nothing to
    /// stop. `in_flight` is the active tab's turn state, captured by the
    /// dispatcher before any mutation.
    fn cmd_stop(&mut self, in_flight: bool) {
        if in_flight {
            let session_id = self.current_tab().session_id.clone();
            if let Some(sid) = session_id.clone() {
                let _ = self.cancel_tx.send(CancelRequest { session_id: sid });
            }
            if let Some(sid) = session_id {
                self.turn_cancel(&sid);
            }
            let tab = self.current_tab_mut();
            tab.messages
                .push(ChatMessage::System(t!("system.cancelled").into_owned()));
            tab.scroll_to_bottom();
        } else {
            let tab = self.current_tab_mut();
            tab.messages.push(ChatMessage::System(
                t!("system.no_prompt_in_flight").into_owned(),
            ));
            tab.scroll_to_bottom();
        }
    }

    /// `/new` — start a fresh session on the active tab. Refuses while a turn
    /// is in flight (the user should `/stop` first).
    fn cmd_new(&mut self, in_flight: bool) {
        if in_flight {
            let tab = self.current_tab_mut();
            tab.messages.push(ChatMessage::System(
                t!("system.busy_use_stop").into_owned(),
            ));
            tab.scroll_to_bottom();
            return;
        }
        let tab_id = self
            .tab_id
            .clone()
            .unwrap_or_else(|| DEFAULT_TAB_ID.to_string());
        let _ = self
            .new_session_tx
            .send(NewSessionForTab { tab_id, cwd: None });
        let tab = self.current_tab_mut();
        tab.clear_chat_history();
        tab.completed_turns.clear();
        tab.selected_completed_turn_idx = None;
        tab.session_id = None;
        tab.scroll_to_bottom();
    }

    /// `/fix [hint]` — run the auto-fix prompt on demand against the active
    /// terminal pane. Reuses the error-triggered autofix pipeline
    /// (`PromptSubmission::is_autofix`): the agent receives the `auto-fix.md`
    /// template plus the working pane's recent output, and any `hint` typed
    /// after `/fix` is appended as an extra steer.
    ///
    /// Differences from auto-triggered autofix (`maybe_trigger_autofix`):
    /// there is no failing-pane notification, so (1) the source pane is
    /// resolved in the ACP client task — `PaneContext.source_pane_id` is left
    /// `None` and `build_prompt_text` falls back to WT's active pane, which
    /// GetActivePane maps from the agent pane to the user's working pane; and
    /// (2) `target_pane_id` starts empty and is late-bound once the client task
    /// resolves that working pane (`AppEvent::AutofixTargetResolved` →
    /// `apply_autofix_target_resolved`), so `turn_execute_card` fills
    /// `Send.parent` with a real pane. The bottom-bar Pending pill is *not*
    /// armed — that UI is tied to a specific failing pane, and a command typed
    /// into the agent pane surfaces its result there directly.
    ///
    /// Refuses while a turn is in flight; the user should `/stop` first.
    fn cmd_fix(&mut self, in_flight: bool, hint: String) {
        if in_flight {
            let tab = self.current_tab_mut();
            tab.messages
                .push(ChatMessage::System(t!("system.busy_use_stop").into_owned()));
            tab.scroll_to_bottom();
            return;
        }

        let target_tab_id = self
            .tab_id
            .clone()
            .unwrap_or_else(|| DEFAULT_TAB_ID.to_string());

        // Bump generation so any stale in-flight autofix response is dropped,
        // and clear a leftover suggestion — mirrors `maybe_trigger_autofix`.
        let generation = {
            let tab = self.tab_mut(&target_tab_id);
            tab.autofix.generation = tab.autofix.generation.wrapping_add(1);
            tab.autofix.suggested_pane_id = None;
            tab.autofix.generation
        };

        let pane_context = PaneContext {
            pane_id: self.pane_id.clone(),
            tab_id: Some(target_tab_id.clone()),
            window_id: self.window_id.clone(),
            cwd: None,
            // None → the client task resolves the active working pane itself.
            source_pane_id: None,
        };

        let hint = hint.trim().to_string();
        let prompt = PromptSubmission::new_autofix(hint.clone(), Some(pane_context));
        let submitted = SubmittedPrompt {
            id: prompt.id,
            text: prompt.text.clone(),
            submitted_at_unix_s: prompt.submitted_at_unix_s,
            autofix: Some(AutofixContext {
                // Placeholder — the working pane isn't known synchronously here.
                // The ACP client task resolves it and `apply_autofix_target_resolved`
                // late-binds it (matched by prompt id) before the card surfaces,
                // so `turn_execute_card` fills `Send.parent` with a real pane.
                target_pane_id: String::new(),
                generation,
            }),
        };
        tracing::info!(
            target: "slash_cmd",
            tab_id = %target_tab_id,
            generation,
            has_hint = !hint.is_empty(),
            "dispatching /fix",
        );
        self.turn_submit_prompt_for_tab(&target_tab_id, submitted);
        let _ = self.prompt_tx.send(prompt);
    }

    /// Late-bind a manual `/fix`'s target pane. The working pane is resolved
    /// in the ACP client task (it isn't known when `cmd_fix` submits) and
    /// plumbed back via [`AppEvent::AutofixTargetResolved`]. We patch the
    /// matching in-flight turn's `AutofixContext.target_pane_id` so that
    /// `turn_execute_card` fills `Send.parent` with a real pane — without it,
    /// the host's send has no destination ("SendInput failed: no parent").
    ///
    /// Routed by `prompt_id`: a superseded turn (the user fired a newer `/fix`)
    /// won't match, so a stale resolution is dropped. The event is emitted
    /// before the agent responds, so the patch lands while the turn is still
    /// `Submitted` — well before the fix card surfaces or the user executes it.
    fn apply_autofix_target_resolved(
        &mut self,
        tab_id: Option<String>,
        prompt_id: u64,
        pane_id: String,
    ) {
        if pane_id.is_empty() {
            return;
        }
        let key = tab_id.unwrap_or_else(|| self.active_tab_key().to_string());
        let Some(tab) = self.tab_sessions.get_mut(&key) else {
            return;
        };
        let Some(prompt) = tab.turn.prompt_mut() else {
            return;
        };
        if prompt.id != prompt_id {
            return;
        }
        let Some(autofix) = prompt.autofix.as_mut() else {
            return;
        };
        autofix.target_pane_id = pane_id.clone();
        tracing::info!(
            target: "slash_cmd",
            tab = %key,
            prompt_id,
            pane = %pane_id,
            "bound /fix target pane",
        );
    }

    /// `/sessions` — open the Agents picker for the active tab.
    fn cmd_sessions(&mut self) {
        // Mirror the Ctrl+Shift+/ keybinding's open path: jump straight to
        // the Agents picker and seed a selection so Enter/Up/Down
        // are immediately useful. Esc / Ctrl+Shift+/ still close the view.
        // Per-tab — only flips the active tab's view state.
        let tab_id = self.active_tab_key().to_string();
        self.open_agents_view_for_tab(tab_id);
        self.project_active_tab_state();
    }

    /// `/restart` — reset the agent CLI subprocess. Behavior depends on which
    /// transport this App is running on:
    ///
    /// * Standalone mode: the ACP client owns the agent CLI child.
    ///   `restart_tx` triggers an in-process tear-down + respawn;
    ///   subsequent prompts get a fresh session on each tab. The
    ///   `Connecting("Restarting agent...")` state lasts until the
    ///   new `initialize` round-trip lands.
    ///
    /// * Helper mode: master owns the agent CLI lifetime, so a
    ///   single helper cannot restart it in-process. The helper's
    ///   `restart_rx` arm asks the C++ side to force-restart the
    ///   whole agent stack (`restart_agent_stack` SendEvent →
    ///   TerminalPage tears down every agent pane,
    ///   `SharedWta::Restart` respawns master on the same stable
    ///   pipe name, then the active tab's pane is re-opened). The
    ///   user briefly sees the agent pane flash closed and reopen
    ///   with a clean session. The `Connecting("Restarting...")`
    ///   state set below is short-lived — this helper process is
    ///   on its way out as part of the pane teardown.
    fn cmd_restart(&mut self) {
        self.state = ConnectionState::Connecting("Restarting agent...".to_string());
        self.session_to_tab.clear();
        self.session_id.clear();
        for (_, tab) in self.tab_sessions.iter_mut() {
            tab.clear_chat_history();
            tab.completed_turns.clear();
            tab.selected_completed_turn_idx = None;
            tab.session_id = None;
        }
        let _ = self.restart_tx.send(RestartRequest { agent_cmd: None });
        self.publish_agent_status();
    }

    /// Width of the main area (chat / recs / perm / input) — matches the
    /// 60/40 horizontal split in `ui::layout::render` when the debug panel is
    /// open. All card/wrap calculations must root here, not `terminal_cols`.
    pub fn main_area_width(&self) -> u16 {
        if self.show_debug_panel {
            self.terminal_cols.saturating_mul(60) / 100
        } else {
            self.terminal_cols
        }
    }

    /// Height of the recommendations panel — grows to fit content, capped so
    /// input and chat still have room, but floored at the tallest card's
    /// height so any card is fully renderable when scrolled to. Using the
    /// tallest (not just the recommended) means Down/Up navigation never
    /// lands on a card too tall for the panel.
    ///
    /// `panel_width` is the actual render width (`main_area.width` after the
    /// debug-panel split), not `terminal_cols` — passing the wrong one
    /// under-counts wrap rows and clips the bottom card when the debug panel
    /// is open.
    pub fn rec_panel_height(&self, panel_width: u16) -> u16 {
        let Some(recs) = self.current_tab().turn.recommendations() else {
            return 0;
        };
        let card_heights = recs
            .choices
            .iter()
            .map(|c| rec_card_height(c, panel_width) as u16);
        let total = card_heights.clone().sum::<u16>();
        let floor = card_heights.max().unwrap_or(ui::card::CARD_MIN_SIZE);
        // Reserve: input(3) + chat_min(1) + rec_hint(1) = 5.
        let ceiling = self.terminal_rows.saturating_sub(5);
        total.min(ceiling).max(floor)
    }

    /// Height reserved for the embedded permission card. Returns 0 only when
    /// no permission is pending — when one *is* pending, the user must be
    /// able to see it (the agent flow is blocked until they answer), so we
    /// fall back to a 1-row compact strip when the full card can't fit.
    /// `permission::render` reads the actual reserved height and switches
    /// between full and compact rendering.
    ///
    /// `panel_width` is the actual render width (`main_area.width` after the
    /// debug-panel split), not `terminal_cols`.
    pub fn permission_panel_height(&self, panel_width: u16) -> u16 {
        let Some(perm) = self.current_tab().permission.front() else {
            return 0;
        };
        let card_h = permission_card_height(perm, panel_width) as u16;
        // Permission is modal — only hard-reserve input(3).
        let ceiling = self.terminal_rows.saturating_sub(3);
        let h = card_h.min(ceiling);
        if h >= ui::card::CARD_MIN_SIZE {
            h
        } else {
            1
        }
    }

    /// Recompute `rec_scroll.max` from the current card heights and the
    /// panel's available cards region. Called from layout.rs before
    /// `recommendations::render` so the renderer stays `&App` and any
    /// wheel-driven over-scroll is clamped before paint.
    pub fn sync_rec_scroll_max(&mut self, panel_width: u16) {
        let panel_cards_h = self.rec_panel_height(panel_width) as usize;
        let Some(recs) = self.current_tab().turn.recommendations() else {
            return;
        };
        let total: usize = recs
            .choices
            .iter()
            .map(|c| rec_card_height(c, panel_width))
            .sum();
        self.current_tab_mut()
            .rec_scroll
            .set_max(total.saturating_sub(panel_cards_h));
    }

    fn clear_recommendations(&mut self) {
        self.current_tab_mut().clear_recommendations();
    }

    /// Scroll the rec panel so the selected card's top sits at the panel top.
    fn scroll_rec_to_selected(&mut self, panel_width: u16) {
        let panel_height = self.rec_panel_height(panel_width) as usize;
        let Some(recs) = self.current_tab().turn.recommendations().cloned() else {
            return;
        };

        let mut line_top = 0usize;
        for (idx, choice) in recs.choices.iter().enumerate() {
            let card_h = rec_card_height(choice, panel_width);
            if idx == self.current_tab().selected_recommendation {
                let tab = self.current_tab_mut();
                if line_top < tab.rec_scroll.offset
                    || line_top + card_h > tab.rec_scroll.offset + panel_height
                {
                    tab.rec_scroll.set(line_top);
                }
                return;
            }
            line_top += card_h;
        }
    }

    /// Switch the active tab. Per-tab state lives in `tab_sessions`, so all
    /// this does is materialize the destination entry (if missing) and
    /// update `tab_id`. No swapping or copying — the previous tab's state
    /// stays exactly where it was.
    ///
    /// Owner-lock: when `self.owner_tab_id` is set (i.e. this is a per-tab
    /// helper spawned for a specific agent pane), `tab_changed` events for
    /// a *different* tab are no-ops. The helper's TUI / per-tab state /
    /// autofix bar are anchored to the owner tab; without this guard, two
    /// helpers in the same window both process every tab switch and the
    /// non-owner's stale `tab_sessions[<other tab>]` default snapshot
    /// (created via `.or_default()` below) clobbers the owner's real
    /// snapshot when both call `project_active_tab_state` — the pane
    /// appears to "disappear" on tab switch because the loser emits
    /// `pane_open=false` after the winner emitted `pane_open=true`.
    /// Helpers without an owner (delegate path, legacy `wta` runs) still
    /// follow the active tab.
    fn switch_tab_session(&mut self, new_tab_id: String) {
        if let Some(owner) = self.owner_tab_id.as_deref() {
            if owner != new_tab_id {
                tracing::debug!(
                    target: "tab_session",
                    owner,
                    new_tab_id = %new_tab_id,
                    "switch_tab_session: ignoring tab_changed for non-owner tab"
                );
                return;
            }
        }

        let old_tab = self.tab_id.clone();
        let entry = self.tab_sessions.entry(new_tab_id.clone()).or_default();
        tracing::info!(
            target: "tab_session",
            from = ?old_tab,
            to = %new_tab_id,
            target_completed_turns = entry.completed_turns.len(),
            target_messages = entry.messages.len(),
            "switch_tab_session"
        );
        self.tab_id = Some(new_tab_id);

        // The new active tab's `current_view` (and autofix bar) is now
        // authoritative for the shared C++ agent pane. Re-emit so the bar
        // title and bottom-bar highlight match the tab we just switched to;
        // without this, C++'s global flag stays on the previous tab's view
        // and the agent bar shows "Agent sessions" while the TUI below
        // actually renders chat (or vice versa).
        self.project_active_tab_state();

        // Push the new active tab's chip-target (or release it) so the C++
        // side stops drawing the previous tab's override. Helpers are
        // per-tab and the owner-lock guard above means we only reach here
        // for our own owner tab, so this is just a re-publish — not a
        // cross-tab decision.
        let to_recompute = self.tab_id.clone();
        if let Some(t) = to_recompute {
            self.recompute_chip_override(&t);
        }
    }

    /// Drop the per-tab state for a tab that WT has just destroyed. Removes
    /// the matching `TabSession` and prunes any `session_to_tab` entries
    /// that pointed at it (so a future SessionId reuse can't route into the
    /// dead tab's slot). Refuses to drop `DEFAULT_TAB_ID` since the App
    /// always needs at least one materialized tab to render.
    fn drop_tab_session(&mut self, closed_tab_id: &str) {
        if closed_tab_id == DEFAULT_TAB_ID {
            tracing::warn!(
                target: "tab_session",
                "tab_closed: refusing to drop default tab"
            );
            return;
        }
        let removed = self.tab_sessions.remove(closed_tab_id);
        self.session_to_tab.retain(|_, tab| tab != closed_tab_id);

        // Tell the ACP client to release the binding for this tab so
        // the agent process can `session/cancel` the orphaned session.
        // Without this, every closed tab leaves a live ACP session
        // behind on the CLI side — `tab_sessions` and `session_to_tab`
        // are cleaned above but the ACP layer's own `tab_to_session`
        // map and the agent's session state are not.
        let _ = self.drop_session_tx.send(DropSessionRequest {
            tab_id: closed_tab_id.to_string(),
        });

        if self.tab_id.as_deref() == Some(closed_tab_id) {
            // Active tab is gone; the next focused tab's tab_changed will
            // arrive imminently, but in the meantime `current_tab()` must
            // not panic. `active_tab_key()` falls back to DEFAULT_TAB_ID
            // when tab_id is None, so re-materialize that slot. The
            // fallback session is empty by design; renders during the gap
            // just show nothing.
            self.tab_id = None;
            self.tab_sessions
                .entry(DEFAULT_TAB_ID.to_string())
                .or_default();
        }
        tracing::info!(
            target: "tab_session",
            tab_id = closed_tab_id,
            had_session = removed.is_some(),
            remaining_tabs = self.tab_sessions.len(),
            "drop_tab_session"
        );
    }

    /// Rekey per-tab state after a tab-drag rename. WT mints a fresh
    /// StableId when the user drags a tab into another window; the
    /// underlying helper process survives the drag (conpty + TermControl
    /// reattach via WT's ContentId mechanism) but the tab key WT uses to
    /// address us has changed. Without this, autofix / set_agent_state /
    /// any other event WT broadcasts with the new id would miss every
    /// entry keyed under the old id.
    ///
    /// Concretely re-keys: `self.tab_id`, `self.tab_sessions` (HashMap key),
    /// `self.session_to_tab` (values), and any cached
    /// `wt_notifications.tab_id` matching the old id. Triggers a
    /// re-projection so the bottom-bar autofix snapshot, agent-pane view,
    /// and pane_open flag are republished under the new identity.
    ///
    /// No-op when `new_tab_id == old_tab_id`. If the old tab id is unknown,
    /// still updates `self.tab_id` when it pointed there — this defends
    /// against a missed `tab_changed` race where WTA's view of the active
    /// tab and tab_sessions disagree.
    fn rename_tab_session(
        &mut self,
        old_tab_id: &str,
        new_tab_id: &str,
        new_window_id: Option<&str>,
    ) {
        if old_tab_id == new_tab_id {
            tracing::debug!(
                target: "helper",
                old_tab_id,
                new_tab_id,
                "tab_renamed no-op: ids identical"
            );
            return;
        }
        let had_session = if let Some(mut entry) = self.tab_sessions.remove(old_tab_id) {
            // Preserve target slot's TabSession if one was lazily
            // created under the new id before this event arrived — but
            // in normal flow that shouldn't happen (WT mints the new
            // id atomically with the drag). Defensive only: prefer the
            // entry that already has conversation state.
            if let Some(existing) = self.tab_sessions.remove(new_tab_id) {
                if !existing.messages.is_empty() && entry.messages.is_empty() {
                    entry = existing;
                }
            }
            self.tab_sessions.insert(new_tab_id.to_string(), entry);
            true
        } else {
            false
        };

        if self.tab_id.as_deref() == Some(old_tab_id) {
            self.tab_id = Some(new_tab_id.to_string());
        }
        // owner_tab_id is the helper's anchor for outbound per-pane events
        // (agent_status / autofix_state). Follow the rename so subsequent
        // events route to the new tab id on the C++ side. Without this,
        // a cross-window drag leaves the helper publishing tab_id=old —
        // C++'s _FindTabByStableId(old) misses (old tab is gone from the
        // source window, new id is in target), drops the event, and the
        // title bar / bottom bar never picks up the helper's state.
        let owner_matched = self.owner_tab_id.as_deref() == Some(old_tab_id);
        if owner_matched {
            self.owner_tab_id = Some(new_tab_id.to_string());
            // This helper owns the dragged tab. The conpty/TermControl
            // moved to the dest window — point `self.window_id` at it so
            // subsequent set_agent_state / tab_changed events from the new
            // window pass the per-window filter. Without this, the helper
            // stays bound to the source window's id and ignores its own
            // tab's events in the new window.
            if let Some(wid) = new_window_id {
                let old = self.window_id.clone();
                self.window_id = Some(wid.to_string());
                tracing::info!(
                    target: "helper",
                    old_window_id = ?old,
                    new_window_id = wid,
                    "tab_renamed: updated self.window_id (dragged helper)"
                );
            }
        }

        // session_to_tab values point at tab ids — rewrite any that
        // matched. Iterating + collecting keys to avoid holding the
        // borrow while we mutate.
        let mut rebound_sessions = 0usize;
        for tab in self.session_to_tab.values_mut() {
            if tab == old_tab_id {
                *tab = new_tab_id.to_string();
                rebound_sessions += 1;
            }
        }

        // wt_notifications carry the originating tab id so a later
        // dismiss / re-emit targets the right tab. Rewrite cached ones.
        let mut rebound_notifications = 0usize;
        for n in self.wt_notifications.iter_mut() {
            if n.tab_id.as_deref() == Some(old_tab_id) {
                n.tab_id = Some(new_tab_id.to_string());
                rebound_notifications += 1;
            }
        }

        tracing::info!(
            target: "helper",
            old_tab_id,
            new_tab_id,
            had_session,
            rebound_sessions,
            rebound_notifications,
            "tab renamed via drag"
        );

        // Re-publish the (now-renamed) active tab so the bottom-bar
        // autofix snapshot, agent-pane view, and pane_open flag are
        // republished under the new identity. Without this, C++'s
        // mirrored state would still be tagged with the old id on the
        // next event round-trip.
        if self.tab_id.as_deref() == Some(new_tab_id) {
            self.project_active_tab_state();
        }

        // Cross-window drag rebuilds the target window's AgentPaneContent
        // from scratch — `_agentName/_agentVersion/_agentModel` all start
        // empty, and nothing on the C++ side re-requests them. Re-emit
        // `agent_status` tagged with the new tab id so the new
        // AgentPaneContent's `UpdateAgentStatus` fires and the XAML bar
        // (label + logo) repopulates. Only the owning helper has
        // meaningful state to publish — other helpers' status events
        // for the dragged tab id would be wrong.
        if owner_matched {
            self.publish_agent_status();
        }

        // Tell the ACP client task to rekey its tab→SessionId map so the
        // next prompt on this tab finds the existing ACP session instead
        // of falling through to the lazy-create branch. The map lives
        // behind `Arc<Mutex<…>>` in the ACP task and can't be touched
        // from `&mut App` directly — mirror the DropSessionRequest plumb.
        // Send-failure means the ACP task is gone; logged for traces but
        // not actionable.
        if let Err(e) = self.rename_session_tx.send(RenameSessionRequest {
            old_tab_id: old_tab_id.to_string(),
            new_tab_id: new_tab_id.to_string(),
        }) {
            tracing::warn!(
                target: "helper",
                old_tab_id,
                new_tab_id,
                error = ?e,
                "rename_session_tx send failed (ACP client task closed?)"
            );
        }
    }

    /// Wipe per-tab state in place while keeping the `TabSession` slot
    /// alive. Called when WT sends `reset_tab_session` (the Ctrl+C×2 hide
    /// path): the WT tab itself isn't going anywhere, but the user asked
    /// for a clean slate on this tab. After this:
    ///   - Conversation history, completed turns, in-flight state are gone.
    ///   - `session_to_tab` entries pointing at this tab are pruned so any
    ///     late ACP events for the old SessionId can't route back in.
    ///   - The ACP client task is asked to drop the binding in
    ///     `tab_to_session` and cancel any in-flight prompt for the old
    ///     SessionId; the next prompt on this tab lazily creates a fresh
    ///     ACP session.
    /// Unlike `drop_tab_session`, this preserves the HashMap key — the
    /// next tab_changed back into this tab finds an empty-but-present
    /// `TabSession` and just renders an empty chat.
    fn reset_tab_session_for(&mut self, tab_id: &str) {
        // Same wipe as the `/clear` slash command: clear in-flight chat state
        // via `clear_chat_history` AND the completed-turn history that
        // `clear_chat_history` deliberately leaves alone.
        if let Some(tab) = self.tab_sessions.get_mut(tab_id) {
            tab.clear_chat_history();
            tab.completed_turns.clear();
            tab.selected_completed_turn_idx = None;
            tab.scroll_to_bottom();
            tab.session_id = None;
        }

        // Prune the reverse SessionId → tab routing so late ACP chunks for
        // the dropped session can't land on this tab's slot.
        self.session_to_tab.retain(|_, t| t != tab_id);

        // Ask the ACP client task to release the binding for this tab.
        let _ = self.drop_session_tx.send(DropSessionRequest {
            tab_id: tab_id.to_string(),
        });

        tracing::info!(
            target: "tab_session",
            tab_id = tab_id,
            "reset_tab_session_for done"
        );
    }

    fn session_completion_latency_summary(&self, session_id: &str) -> Option<String> {
        let mut parts = Vec::new();
        let tab = self.session_tab(session_id);

        if let Some(prompt) = tab.turn.prompt() {
            let total_s = (now_unix_s() - prompt.submitted_at_unix_s).max(0.0);
            parts.push(format!("total {:.3}s", total_s));
        }

        if let Some(note) = tab.timing_note.as_deref().filter(|note| !note.is_empty()) {
            parts.push(note.to_string());
        }

        if parts.is_empty() {
            None
        } else {
            Some(format!("Latency: {}", parts.join(" | ")))
        }
    }

    /// Delegate a prompt to a new tab agent by spawning `wta delegate` subprocess.
    /// This is the same path used by the command palette — single code path for
    /// context capture, prompt building, and tab creation.
    pub fn delegate_to_tab_agent(&self, prompt: &str) {
        tracing::info!(target: "autofix", prompt_len = prompt.len(), "delegate_to_tab_agent called");
        let exe = match std::env::current_exe() {
            Ok(p) => p,
            Err(_) => return,
        };
        let mut cmd = std::process::Command::new(exe);
        cmd.arg("delegate").arg(prompt);
        // The delegate child inherits WT_COM_CLSID from our env; no explicit pass needed.

        // Fire-and-forget: spawn hidden, don't wait.
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }
        let _ = cmd.spawn();
    }


    /// Ask WT to tear down this agent pane. Wired to the second tap of the
    /// double-Ctrl+C close sequence. WT closes the Pane, which causes its
    /// ConPty to SIGKILL us — so the natural side effect of pane teardown
    /// is that wta exits and the in-process `tab_to_session` map dies with
    /// it. The next time the user toggles the agent pane open, WT spawns a
    /// fresh wta whose map is empty: per-tab ACP sessions get re-bound to
    /// the new wta's keyspace, which is the "clean session" semantics we
    /// want without any explicit per-entry cleanup.
    ///
    /// We do NOT set `should_quit` here. If WT's close path is delayed or
    /// the event is dropped, wta keeps running and the user can try again
    /// (or use the WT-side close-pane keybinding). Self-quitting would
    /// race the close request and produce a "process exited" pane that
    /// the next toggle can't recover from cleanly.
    fn request_close_agent_pane(&self) {
        let mut params = serde_json::Map::new();
        if let Some(ref p) = self.pane_id {
            params.insert("pane_id".to_string(), serde_json::Value::String(p.clone()));
        }
        if let Some(ref t) = self.tab_id {
            params.insert("tab_id".to_string(), serde_json::Value::String(t.clone()));
        }
        let evt = serde_json::json!({
            "type": "event",
            "method": "close_agent_pane",
            "params": serde_json::Value::Object(params),
        });
        tracing::info!(target: "close_pane", "double-Ctrl+C → asking WT to close agent pane");
        send_wt_protocol_event(evt.to_string());
    }


    /// Recompute the chip-target override for the tab and, if it changed
    /// since the last emit, publish a `set_agent_chip_target` event so the
    /// C++ side pins the "Agent" chip on the right pane (or releases it,
    /// returning to source-of-agent driven rendering). Hooked at every
    /// state-mutation point that could affect the result: surfacing a
    /// recommendation, navigating between cards, executing/cancelling a
    /// card, switching the active tab.
    fn recompute_chip_override(&mut self, tab_id: &str) {
        let new_target = self.tab_mut(tab_id).compute_chip_card_target();
        let tab = self.tab_mut(tab_id);
        if tab.last_emitted_chip_override == new_target {
            return;
        }
        tab.last_emitted_chip_override = new_target.clone();
        emit_agent_chip_target(tab_id, new_target.as_deref());
    }

    /// Publish the chip-target state for this tab unconditionally, even
    /// when it matches the last value we emitted. Used at helper startup
    /// (right after `tab_id` is seeded from `--owner-tab-id`) so the C++
    /// side runs `_UpdateAgentChipVisibility` against the now-current
    /// pane tree. Without this kick, the first-launch race where the
    /// chip-visibility hook runs *before* `IsSourceOfAgentPane` is set
    /// leaves the chip hidden until the user induces another transition.
    pub fn recompute_chip_override_initial(&mut self, tab_id: &str) {
        let new_target = self.tab_mut(tab_id).compute_chip_card_target();
        self.tab_mut(tab_id).last_emitted_chip_override = new_target.clone();
        emit_agent_chip_target(tab_id, new_target.as_deref());
    }

    fn push_execution_info(&mut self, _message: String) {}

    fn selected_recommendation_choice(&self) -> Option<&RecommendationChoice> {
        let tab = self.current_tab();
        tab.turn
            .recommendations()
            .and_then(|recs| recs.choices.get(tab.selected_recommendation))
    }

    /// Returns the number of buttons for the currently selected choice card.
    /// Send actions have 2 buttons (Run, Insert); OpenAndSend has 1 button.
    fn button_count_for_selected(&self) -> usize {
        self.selected_recommendation_choice()
            .map(|c| if self.is_send_choice(c) { 2 } else { 1 })
            .unwrap_or(1)
    }

    /// Default focused button index when landing on a card. Always 0 — the
    /// leftmost button (Run for Send cards, the sole button for OpenAndSend).
    fn default_button_for_selected(&self) -> usize {
        0
    }

    /// Returns true if the choice's primary action is Send (shell command).
    fn is_send_choice(&self, choice: &RecommendationChoice) -> bool {
        choice
            .actions
            .iter()
            .any(|a| matches!(a, crate::coordinator::RecommendedAction::Send { .. }))
    }

    fn log_selection_phase_for(&self, session_id: &str, phase: &str, details: &str) {
        // log against the in-flight tab so traces stay coherent with where
        // the prompt was submitted, even after the user switches tabs.
        let tab = self.session_tab(session_id);
        if let Some(prompt) = tab.turn.prompt() {
            prompt_timing_log(prompt.id, prompt.submitted_at_unix_s, phase, details);
        }
    }

    fn log_selection_visible_if_needed(&mut self) {
        let tab = self.current_tab();
        if !tab.selection_visible_pending || tab.turn.recommendations().is_none() {
            return;
        }
        let details = format!(
            "choice_count={} selected_index={}",
            tab.turn
                .recommendations()
                .map(|set| set.choices.len())
                .unwrap_or(0),
            tab.selected_recommendation
        );
        let session_id = tab.session_id.clone();
        if let Some(sid) = session_id {
            self.log_selection_phase_for(&sid, "selection_visible", &details);
        }
        self.current_tab_mut().selection_visible_pending = false;
    }
}

/// Return the cwd to hand to `wsl --cd` — only when it's an absolute
/// Linux path (starts with `/`). A Windows path, empty cwd, or a path
/// containing a double-quote (which would break the quoted `--cd "…"`
/// argument) yields `None`, so WSL falls back to the distro's `$HOME`.
fn linux_cwd_arg(cwd: &std::path::Path) -> Option<String> {
    let s = cwd.to_string_lossy();
    let s = s.trim();
    (s.starts_with('/') && !s.contains('"')).then(|| s.to_string())
}

// ─────────────────────────────────────────────────────────────────────────
// TurnState transition methods
//
// Source of truth for the per-turn lifecycle (see
// `doc/specs/turn-state-refactor.md`). Every event handler — chunk arrival,
// end-of-turn, Enter on a card, Esc / Ctrl+C cancel, autofix trigger — goes
// through one of these methods.
// ─────────────────────────────────────────────────────────────────────────

impl App {
    /// Transition `tab.turn` into `Submitted` for a new prompt and perform
    /// the side effects: clear stale in-flight chat state (messages, tool
    /// calls, permission, scroll), push the user bubble, log
    /// `prompt_received`. Caller is responsible for actually dispatching the
    /// prompt over ACP (so this method stays free of async / channel
    /// concerns).
    pub fn turn_submit_prompt(&mut self, session_id: &str, prompt: SubmittedPrompt) {
        let tab_key = self.tab_for_session(session_id);
        self.turn_submit_prompt_for_tab(&tab_key, prompt);
    }

    /// Identical to `turn_submit_prompt` but takes the target tab's id
    /// directly, bypassing the `session_id → tab_id` lookup. Used by the
    /// autofix path so a failure in a background tab installs the turn on
    /// that tab even when its ACP session hasn't been created yet (the ACP
    /// layer lazy-creates one when the prompt is dispatched).
    pub fn turn_submit_prompt_for_tab(&mut self, tab_id: &str, prompt: SubmittedPrompt) {
        prompt_timing_log(
            prompt.id,
            prompt.submitted_at_unix_s,
            "prompt_received",
            &format!(
                "autofix={} text_chars={}",
                prompt.autofix.is_some(),
                prompt.text.chars().count()
            ),
        );
        let is_autofix = prompt.autofix.is_some();
        let user_text = prompt.text.clone();
        let tab = self.tab_mut(tab_id);
        // Per Decision #3, every Idle→Submitted transition explicitly clears
        // these orthogonal fields rather than relying on side effects from a
        // grab-bag helper.
        tab.messages.clear();
        tab.tool_calls.clear();
        // Dropping any in-flight responders signals Cancelled back to
        // the agent — appropriate when the user starts a new turn.
        tab.permission.clear();
        tab.chat_scroll.reset();
        tab.selection_visible_pending = false;
        // Any leftover card from the previous turn's
        // `Surfaced{end_pending:false}` is dismissed by the new submit.
        tab.selected_recommendation = 0;
        tab.selected_button = 0;
        tab.rec_scroll.reset();
        // Autofix prompts are synthesized by the system; they don't render
        // as a User bubble (the user already sees the error line in the
        // failing pane).
        if !is_autofix {
            tab.messages.push(ChatMessage::User(user_text));
        }
        tab.scroll_to_bottom();
        tab.progress_status = None;
        tab.activity_frame = 0;
        tab.timing_note = None;
        tab.turn = TurnState::Submitted(prompt);

        // Submitting a new prompt dismisses any prior leftover card (the
        // `selected_recommendation = 0` + turn reset above). If the helper
        // had pinned the chip onto that card's pane, release it now so the
        // chip falls back to source-of-agent while the new turn is in
        // flight. Note: this only matters for the new-turn case; the
        // freshly-submitted autofix path overrides chip via the eventual
        // `turn_surface_*` callback once recommendations arrive.
        let owned_tab = tab_id.to_string();
        self.recompute_chip_override(&owned_tab);
    }

    /// Observe a streamed chunk. Thought chunks only advance the state
    /// (Submitted→Streaming with empty buffer); message chunks append to the
    /// streaming buffer. Returns true if the buffer changed (so the caller
    /// can decide whether to attempt an eager surface).
    pub fn turn_observe_chunk(&mut self, session_id: &str, kind: ChunkKind, text: &str) -> bool {
        // Stale-autofix check: if the chunk belongs to an autofix turn whose
        // generation no longer matches the tab's counter, drop it.
        let tab = self.session_tab_mut(session_id);
        let current_gen = tab.autofix.generation;
        if let Some(gen) = tab.turn.autofix_generation() {
            if gen != current_gen {
                tracing::debug!(
                    target: "autofix",
                    inflight_gen = gen,
                    current_gen,
                    "dropping stale autofix chunk",
                );
                return false;
            }
        }

        // `progress_status` (agent-supplied "Reading foo.rs" etc.) is left
        // alone here — its natural lifetime is the whole turn. It's cleared
        // at turn close (`turn_clear_agent_progress`) and overwritten by
        // future `ProgressStatus` events. The old per-chunk wipe erased
        // the value the moment a streaming agent would have it set.
        match (&mut tab.turn, kind) {
            // First message chunk: transition Submitted → Streaming.
            (TurnState::Submitted(_), ChunkKind::Message) => {
                let TurnState::Submitted(prompt) =
                    std::mem::replace(&mut tab.turn, TurnState::Idle)
                else {
                    unreachable!();
                };
                tab.turn = TurnState::Streaming {
                    prompt,
                    buf: text.to_string(),
                };
                // New turn: restart the typewriter reveal from the top.
                tab.reveal_chars = 0;
                true
            }
            // Thought chunk while Submitted: enter Streaming with empty buf.
            (TurnState::Submitted(_), ChunkKind::Thought) => {
                let TurnState::Submitted(prompt) =
                    std::mem::replace(&mut tab.turn, TurnState::Idle)
                else {
                    unreachable!();
                };
                tab.turn = TurnState::Streaming {
                    prompt,
                    buf: String::new(),
                };
                tab.reveal_chars = 0;
                false
            }
            // Streaming → Streaming, append message chunks only.
            (TurnState::Streaming { buf, .. }, ChunkKind::Message) => {
                buf.push_str(text);
                true
            }
            // Thought chunks during Streaming: no buffer change.
            (TurnState::Streaming { .. }, ChunkKind::Thought) => false,
            // Trailing chunks after the card has surfaced: drop them.
            (TurnState::Surfaced { .. }, _) => false,
            // Chunks while Idle: shouldn't happen; defensive drop.
            (TurnState::Idle, _) => false,
        }
    }

    /// Attempt to parse the streaming buffer and surface a card / chat turn
    /// without waiting for `AgentMessageEnd`. No-op if state isn't
    /// `Streaming`, buffer hasn't opened a fence yet, or parsing fails.
    pub fn turn_try_eager_surface(&mut self, session_id: &str) {
        let tab = self.session_tab(session_id);
        let TurnState::Streaming { buf, .. } = &tab.turn else {
            return;
        };
        if !buf.contains("```") {
            return;
        }
        let buf = buf.clone();
        let is_autofix = tab.turn.is_autofix();

        if is_autofix {
            match parse_autofix_response(&buf) {
                AutofixDecision::Fix(recommendations) => {
                    self.turn_surface_fix(session_id, recommendations, "autofix_fix_eager");
                }
                AutofixDecision::Explain { title, explanation } => {
                    self.turn_surface_explain(
                        session_id,
                        title,
                        explanation,
                        "autofix_explain_eager",
                    );
                }
                AutofixDecision::Ignore => {}
            }
        } else {
            let parsed = parse_recommendation_set(&buf).and_then(|r| {
                validate_recommendation_set_for_coordinator_target(&r, self.pane_id.as_deref())
            });
            if let Ok(recommendations) = parsed {
                self.turn_surface_recommendation(
                    session_id,
                    recommendations,
                    "selection_ready_eager",
                );
            }
        }
    }

    /// Close the in-flight turn on `AgentMessageEnd`. Dispatches across
    /// four termination paths:
    ///
    /// 1. Stale-autofix discard (newer trigger or Esc cancelled this turn).
    /// 2. Eager surface already fired — just release the UI gate.
    /// 3. `Submitted` with no chunks — model returned nothing.
    /// 4. `Streaming` with a buffer — final parse via the autofix or
    ///    planner finalize helper.
    pub fn turn_close(&mut self, session_id: &str) {
        // (1) Stale-autofix discard.
        let current_gen = self.session_tab(session_id).autofix.generation;
        if let Some(gen) = self.session_tab(session_id).turn.autofix_generation() {
            if gen != current_gen {
                tracing::info!(
                    target: "autofix",
                    inflight_gen = gen,
                    current_gen,
                    "discarding stale autofix turn at close",
                );
                self.turn_clear_agent_progress(session_id);
                self.session_tab_mut(session_id).turn = TurnState::Idle;
                return;
            }
        }

        // (2) Eager surface already fired.
        if let TurnState::Surfaced {
            end_pending: true, ..
        } = &self.session_tab(session_id).turn
        {
            self.turn_release_end_pending_logged(session_id, "via=eager+end");
            self.turn_clear_agent_progress(session_id);
            return;
        }

        // (3) Submitted, no chunks. For autofix this would leave the bar
        //     stuck in Pending; clear it explicitly.
        let (buf, is_autofix) = match &self.session_tab(session_id).turn {
            TurnState::Streaming { buf, prompt } => (buf.clone(), prompt.autofix.is_some()),
            TurnState::Submitted(_) => {
                self.turn_close_no_chunks(session_id);
                return;
            }
            // Idle / already-surfaced+end_done — nothing to do.
            _ => return,
        };

        // (4) Final parse on the streaming buffer.
        if is_autofix {
            self.turn_close_finalize_autofix(session_id, &buf);
        } else {
            self.turn_close_finalize_planner(session_id, buf);
        }
        self.turn_clear_agent_progress(session_id);
    }

    /// Path (3): close a turn that received `AgentMessageEnd` with no
    /// streamed content. Emits `autofix_state_cleared` if it was an
    /// autofix turn so the bottom bar doesn't stick in Pending.
    fn turn_close_no_chunks(&mut self, session_id: &str) {
        let target_tab = self.tab_for_session(session_id);
        let tab = self.session_tab_mut(session_id);
        let prompt = tab.turn.prompt().cloned().expect("prompt set");
        // Empty `target_pane_id` (manual `/fix`) is not a real pane — filter
        // it out so an empty-response turn doesn't emit a bottom-bar event.
        let autofix_pane = prompt
            .autofix
            .as_ref()
            .map(|a| a.target_pane_id.clone())
            .filter(|s| !s.is_empty());
        tab.turn = TurnState::Surfaced {
            prompt,
            outcome: TurnOutcome::Empty,
            end_pending: true,
        };
        if autofix_pane.is_some() {
            self.emit_autofix_state_cleared(&target_tab);
            let autofix = &mut self.session_tab_mut(session_id).autofix;
            autofix.pane_id = None;
            autofix.armed_at = None;
        }
        self.turn_release_end_pending(session_id);
        self.turn_clear_agent_progress(session_id);
    }

    /// Path (4a): autofix Streaming buffer reached `AgentMessageEnd` with
    /// no eager surface. Parse and route to Fix / Explain / Ignore.
    fn turn_close_finalize_autofix(&mut self, session_id: &str, buf: &str) {
        match parse_autofix_response(buf) {
            AutofixDecision::Fix(recommendations) => {
                self.turn_surface_fix(session_id, recommendations, "autofix_fix");
                self.turn_release_end_pending(session_id);
            }
            AutofixDecision::Explain { title, explanation } => {
                self.turn_surface_explain(session_id, title, explanation, "autofix_explain");
                self.turn_release_end_pending(session_id);
            }
            AutofixDecision::Ignore => {
                let target_tab = self.tab_for_session(session_id);
                let pane_id = self.session_tab(session_id).autofix.pane_id.clone();
                self.log_selection_phase_for(
                    session_id,
                    "autofix_ignore",
                    &format!("pane={:?}", pane_id),
                );
                if pane_id.is_some() {
                    self.emit_autofix_state_cleared(&target_tab);
                }
                let autofix = &mut self.session_tab_mut(session_id).autofix;
                autofix.pane_id = None;
                autofix.armed_at = None;
                let tab = self.session_tab_mut(session_id);
                let prompt = tab.turn.prompt().cloned().expect("prompt set");
                // Preserve only what the user actually saw streaming (prose
                // or extracted `explanation`) — not the raw JSON wrapper.
                // Any tool calls / plans that streamed during the turn are
                // included regardless; an empty-buf+prose ignore still
                // records them so they don't get stranded on screen.
                let visible = ui::chat::user_visible_stream_text(buf).map(|c| c.into_owned());
                let mut details = tab.current_turn_details();
                if let Some(visible) = visible {
                    details.push(ChatMessage::Agent(visible));
                }
                if !details.is_empty() {
                    tab.completed_turns.push(CompletedTurn {
                        prompt: t!("chat.autofix_prompt_label").into_owned(),
                        details,
                        expanded: true,
                        trailing_marker: None,
                    });
                }
                // Always clear in-flight UI state on Ignore — even if there
                // was nothing to commit, lingering tool-call rows would look
                // like an active turn.
                tab.messages.clear();
                tab.tool_calls.clear();
                tab.scroll_to_bottom();
                tab.turn = TurnState::Surfaced {
                    prompt,
                    outcome: TurnOutcome::Empty,
                    end_pending: false,
                };
            }
        }
    }

    /// Path (4b): non-autofix Streaming buffer. Try `RecommendationSet`
    /// parse first; on failure, commit as a chat turn (chat-mode answer).
    fn turn_close_finalize_planner(&mut self, session_id: &str, buf: String) {
        let parsed = parse_recommendation_set(&buf).and_then(|r| {
            validate_recommendation_set_for_coordinator_target(&r, self.pane_id.as_deref())
        });
        match parsed {
            Ok(recommendations) => {
                self.turn_surface_recommendation(session_id, recommendations, "selection_ready");
                self.turn_release_end_pending(session_id);
            }
            Err(err) => {
                let chars = buf.chars().count();
                let error_text = format!("{:#}", err).replace('\n', " | ");
                self.log_selection_phase_for(
                    session_id,
                    "selection_parse_failed",
                    &format!("response_chars={} error={:?}", chars, error_text),
                );
                let tab = self.session_tab_mut(session_id);
                let prompt = tab.turn.prompt().cloned().expect("prompt set");
                let mut details = tab.current_turn_details();
                details.push(ChatMessage::Agent(buf));
                tab.completed_turns.push(CompletedTurn {
                    prompt: prompt.text.clone(),
                    details,
                    expanded: true,
                    trailing_marker: None,
                });
                tab.messages.clear();
                tab.tool_calls.clear();
                tab.scroll_to_bottom();
                // Route through `turn_release_end_pending` so
                // `prompt_complete` fires on this terminal path too.
                tab.turn = TurnState::Surfaced {
                    prompt,
                    outcome: TurnOutcome::ChatTurn,
                    end_pending: true,
                };
                self.turn_release_end_pending(session_id);
            }
        }
    }

    /// Variant of `turn_release_end_pending` with a custom `via=` log tag
    /// for the eager-surface path. `turn_release_end_pending` uses
    /// `via=end_only`; `via=eager+end` lets `prompt_timing` consumers
    /// distinguish.
    fn turn_release_end_pending_logged(&mut self, session_id: &str, via: &str) {
        let tab = self.session_tab_mut(session_id);
        if let TurnState::Surfaced {
            end_pending,
            prompt,
            ..
        } = &mut tab.turn
        {
            if *end_pending {
                *end_pending = false;
                let prompt_id = prompt.id;
                let submitted_at = prompt.submitted_at_unix_s;
                prompt_timing_log(prompt_id, submitted_at, "prompt_complete", via);
            }
        }
    }

    /// Helper called at every turn-close path. Clears the agent-supplied
    /// progress override and the shimmer animation phase; the UI spinner
    /// otherwise drives off `tab.turn.spinner_label()`.
    fn turn_clear_agent_progress(&mut self, session_id: &str) {
        let tab = self.session_tab_mut(session_id);
        tab.progress_status = None;
        tab.activity_frame = 0;
    }

    /// User pressed Enter while a card was visible — dispatch the selected
    /// choice to the coordinator and transition to `Surfaced { Empty, .. }`
    /// while preserving the ACP single-flight gate.
    pub fn turn_execute_card(&mut self, session_id: &str) {
        let Some(mut choice) = self.selected_recommendation_choice().cloned() else {
            return;
        };
        let tab = self.session_tab(session_id);
        let TurnState::Surfaced {
            outcome: TurnOutcome::Recommendation(_),
            ..
        } = &tab.turn
        else {
            return;
        };
        // Snapshot the title before `choice` is moved into ChoiceExecution,
        // so we can stamp the chat history with an "executed" marker after
        // dispatch.
        let executed_title = choice.title.clone();
        let insert_only =
            self.session_tab(session_id).selected_button == 1 && self.is_send_choice(&choice);
        // Autofill parent for Send actions when this is an autofix turn.
        if let Some(pane_id) = self
            .session_tab(session_id)
            .turn
            .prompt()
            .and_then(|p| p.autofix.as_ref())
            .map(|a| a.target_pane_id.clone())
        {
            for action in &mut choice.actions {
                if let crate::coordinator::RecommendedAction::Send { ref mut parent, .. } = action {
                    if parent.is_empty() {
                        *parent = pane_id.clone();
                    }
                }
            }
        }
        let target_tab = self.tab_for_session(session_id);
        let armed_pane = self
            .session_tab(session_id)
            .turn
            .prompt()
            .and_then(|p| p.autofix.as_ref())
            .map(|a| a.target_pane_id.clone());
        let _ = self
            .recommendation_tx
            .send(crate::coordinator::ChoiceExecution {
                choice,
                insert_only,
            });
        if armed_pane.is_some() {
            self.emit_autofix_state_cleared(&target_tab);
        }
        let autofix = &mut self.session_tab_mut(session_id).autofix;
        autofix.pane_id = None;
        autofix.armed_at = None;
        let tab = self.session_tab_mut(session_id);
        let TurnState::Surfaced {
            prompt,
            end_pending,
            ..
        } = std::mem::replace(&mut tab.turn, TurnState::Idle)
        else {
            unreachable!()
        };
        tab.selected_recommendation = 0;
        tab.selected_button = 0;
        tab.rec_scroll.reset();
        // Stamp the matching completed_turn (pushed during surface) with an
        // "executed" marker so chat history reflects the user's choice.
        if let Some(last) = tab.completed_turns.last_mut() {
            let marker = t!("chat.turn_executed", title = &executed_title).into_owned();
            last.trailing_marker = Some(marker);
        }
        // commit pending turn (in case eager surface staged one).
        tab.turn = TurnState::Surfaced {
            prompt,
            outcome: TurnOutcome::Empty,
            end_pending,
        };

        // Exiting Surfaced{Recommendation} — release any chip override the
        // card had pinned. The C++ side falls back to source-of-agent.
        let target_tab = self.tab_for_session(session_id);
        self.recompute_chip_override(&target_tab);
    }

    /// User pressed Esc — cancel the in-flight turn. Bumps
    /// `autofix_generation` so any chunks that arrive after this point are
    /// dropped by the stale-check in `turn_observe_chunk`.
    pub fn turn_cancel(&mut self, session_id: &str) {
        let target_tab = self.tab_for_session(session_id);
        let pane_id = {
            let tab = self.session_tab_mut(session_id);
            tab.autofix.generation = tab.autofix.generation.wrapping_add(1);
            tab.turn
                .prompt()
                .and_then(|p| p.autofix.as_ref())
                .map(|a| a.target_pane_id.clone())
                .or_else(|| tab.autofix.pane_id.clone())
        };
        if pane_id.is_some() {
            self.emit_autofix_state_cleared(&target_tab);
        }
        let tab = self.session_tab_mut(session_id);
        tab.autofix.armed_at = None;
        let canceled_marker = t!("chat.turn_canceled").into_owned();
        // Three paths into cancel:
        //   - Submitted / Streaming → commit a fresh completed_turn (prompt +
        //     whatever streamed + canceled marker) so the user always sees
        //     that this turn happened and that they cancelled it.
        //   - Surfaced{Recommendation}: turn_surface_* already pushed a
        //     completed_turn; just append the canceled marker to its details.
        //   - Other states (Idle / Surfaced{Empty / ChatTurn}) → no-op.
        let new_turn_data: Option<(String, Option<String>)> = match &tab.turn {
            TurnState::Submitted(prompt) => {
                let label = match prompt.autofix.as_ref() {
                    Some(_) => t!("chat.autofix_prompt_label").into_owned(),
                    None => prompt.text.clone(),
                };
                Some((label, None))
            }
            TurnState::Streaming { prompt, buf } => {
                let label = match prompt.autofix.as_ref() {
                    Some(_) => t!("chat.autofix_prompt_label").into_owned(),
                    None => prompt.text.clone(),
                };
                let visible = ui::chat::user_visible_stream_text(buf).map(|c| c.into_owned());
                Some((label, visible))
            }
            _ => None,
        };
        let annotate_card = matches!(
            &tab.turn,
            TurnState::Surfaced {
                outcome: TurnOutcome::Recommendation(_),
                ..
            }
        );
        if let Some((prompt_label, visible)) = new_turn_data {
            let mut details = tab.current_turn_details();
            if let Some(v) = visible {
                details.push(ChatMessage::Agent(v));
            }
            tab.completed_turns.push(CompletedTurn {
                prompt: prompt_label,
                details,
                expanded: true,
                trailing_marker: Some(canceled_marker),
            });
            tab.messages.clear();
            tab.tool_calls.clear();
            tab.scroll_to_bottom();
        } else if annotate_card {
            if let Some(last) = tab.completed_turns.last_mut() {
                last.trailing_marker = Some(canceled_marker);
            }
        }
        tab.autofix.pane_id = None;
        tab.selected_recommendation = 0;
        tab.selected_button = 0;
        tab.rec_scroll.reset();
        tab.progress_status = None;
        tab.activity_frame = 0;
        tab.turn = TurnState::Idle;

        // Esc on a Send card or in-flight autofix exits the chip-override
        // state; release whatever the helper had pinned. C++ falls back to
        // source-of-agent driven rendering.
        self.recompute_chip_override(&target_tab);
    }

    // ── Internal surface helpers (shared between eager and end-of-turn). ──

    /// Surface a planner-mode recommendation card.
    fn turn_surface_recommendation(
        &mut self,
        session_id: &str,
        recommendations: RecommendationSet,
        phase_name: &str,
    ) {
        let rec_idx = recommended_choice_index(&recommendations);
        let choice_count = recommendations.choices.len();
        let recommended_choice = recommendations.recommended_choice;
        let summary = format_recommendations_for_chat(&recommendations);
        self.log_selection_phase_for(
            session_id,
            phase_name,
            &format!(
                "choice_count={} recommended_choice={:?}",
                choice_count, recommended_choice
            ),
        );
        let tab = self.session_tab_mut(session_id);
        let prompt = tab.turn.prompt().cloned().expect("prompt set");
        let mut details = tab.current_turn_details();
        details.push(ChatMessage::Agent(summary));
        tab.completed_turns.push(CompletedTurn {
            prompt: prompt.text.clone(),
            details,
            expanded: true,
            trailing_marker: None,
        });
        tab.messages.clear();
        tab.tool_calls.clear();
        tab.scroll_to_bottom();
        tab.selected_recommendation = rec_idx;
        tab.selected_button = 0;
        tab.rec_scroll.reset();
        tab.selection_visible_pending = true;
        tab.selected_completed_turn_idx = None;
        tab.progress_status = None;
        tab.activity_frame = 0;
        tab.turn = TurnState::Surfaced {
            prompt,
            outcome: TurnOutcome::Recommendation(recommendations),
            end_pending: true,
        };

        // Entering Surfaced{Recommendation} with a Send card selected is
        // the typing→card transition; ask C++ to pin the chip onto that
        // card's target pane (or release it when the selected card has no
        // Send action).
        let target_tab = self.tab_for_session(session_id);
        self.recompute_chip_override(&target_tab);
    }

    /// Surface an autofix Fix recommendation as an Armed card.
    fn turn_surface_fix(
        &mut self,
        session_id: &str,
        recommendations: RecommendationSet,
        phase_name: &str,
    ) {
        let target_pane_id = self
            .session_tab(session_id)
            .turn
            .prompt()
            .and_then(|p| p.autofix.as_ref())
            .map(|a| a.target_pane_id.clone());
        // Defensive: only autofix turns surface a fix card here.
        let Some(target_pane_id) = target_pane_id else {
            return;
        };
        // An empty `target_pane_id` is a manually-invoked `/fix` with no
        // concrete failing pane. Still surface the card below, but skip the
        // bottom-bar / suggested-pane side effects — they key off a real
        // failing pane (the Review pill, the Ctrl+Alt+. hotkey target).
        let bar_pane = (!target_pane_id.is_empty()).then_some(target_pane_id);
        self.log_selection_phase_for(
            session_id,
            phase_name,
            &format!(
                "pane={bar_pane:?} title={:?}",
                recommendations.choices.first().map(|c| &c.title)
            ),
        );
        let target_tab = self.tab_for_session(session_id);
        // Analysis produced a fix recommendation. Record it as a result
        // pending review and surface the bar accordingly (Review when the
        // pane is closed, Idle when it's already open). The recommendation
        // card still lives in the turn below so the user can act on it
        // inside the pane — autofix no longer auto-executes.
        if let Some(pane_id) = bar_pane.as_ref() {
            {
                let autofix = &mut self.tab_mut(&target_tab).autofix;
                autofix.suggested_pane_id = Some(pane_id.clone());
                autofix.pane_id = None;
                autofix.armed_at = None;
            }
            self.emit_autofix_state_result(&target_tab, pane_id);
        }
        let rec_idx = recommended_choice_index(&recommendations);
        let summary = format_recommendations_for_chat(&recommendations);
        let turn_prompt_label = t!("chat.autofix_prompt_label").into_owned();
        let tab = self.session_tab_mut(session_id);
        let prompt = tab.turn.prompt().cloned().expect("prompt set");
        let mut details = tab.current_turn_details();
        details.push(ChatMessage::Agent(summary));
        tab.completed_turns.push(CompletedTurn {
            prompt: turn_prompt_label,
            details,
            expanded: true,
            trailing_marker: None,
        });
        tab.messages.clear();
        tab.tool_calls.clear();
        tab.scroll_to_bottom();
        tab.selected_recommendation = rec_idx;
        tab.selection_visible_pending = true;
        tab.progress_status = None;
        tab.activity_frame = 0;
        tab.turn = TurnState::Surfaced {
            prompt,
            outcome: TurnOutcome::Recommendation(recommendations),
            end_pending: true,
        };

        // Same handoff as `turn_surface_recommendation`: a fresh Send card
        // is now selectable, pin the chip onto its target pane.
        let target_tab = self.tab_for_session(session_id);
        self.recompute_chip_override(&target_tab);
    }

    /// Surface an autofix Explain answer as a chat turn + bottom-bar
    /// Suggested indicator.
    fn turn_surface_explain(
        &mut self,
        session_id: &str,
        title: String,
        explanation: String,
        phase_name: &str,
    ) {
        let target_pane_id = self
            .session_tab(session_id)
            .turn
            .prompt()
            .and_then(|p| p.autofix.as_ref())
            .map(|a| a.target_pane_id.clone());
        // Defensive: only autofix turns surface an explain answer here.
        let Some(target_pane_id) = target_pane_id else {
            return;
        };
        // Empty `target_pane_id` = a manually-invoked `/fix` with no concrete
        // failing pane: surface the explanation, but skip the bottom-bar /
        // suggested-pane side effects below.
        let bar_pane = (!target_pane_id.is_empty()).then_some(target_pane_id);
        self.log_selection_phase_for(
            session_id,
            phase_name,
            &format!(
                "pane={bar_pane:?} title={title:?} chars={}",
                explanation.chars().count()
            ),
        );

        let turn_prompt_label = t!("chat.autofix_prompt_label").into_owned();
        {
            let tab = self.session_tab_mut(session_id);
            let mut details = tab.current_turn_details();
            details.push(ChatMessage::Agent(explanation));
            // Auto-expand the auto-diagnosed-error turn: when the user
            // clicks the Suggested pill they came here specifically to
            // read the explanation, so showing the collapsed preview
            // would force a second click.
            tab.completed_turns.push(CompletedTurn {
                prompt: turn_prompt_label,
                details,
                expanded: true,
                trailing_marker: None,
            });
            tab.messages.clear();
            tab.tool_calls.clear();
            tab.scroll_to_bottom();
        }

        let target_tab = self.tab_for_session(session_id);
        // Explanation lives in the chat above; mark the tab as having a
        // result pending review and surface the bar (Review when the pane
        // is closed, Idle when already open).
        if let Some(pane_id) = bar_pane.as_ref() {
            {
                let tab = self.session_tab_mut(session_id);
                tab.autofix.suggested_pane_id = Some(pane_id.clone());
                tab.autofix.pane_id = None;
                tab.autofix.armed_at = None;
            }
            self.emit_autofix_state_result(&target_tab, pane_id);
        }

        let tab = self.session_tab_mut(session_id);
        let prompt = tab.turn.prompt().cloned().expect("prompt set");
        tab.selected_recommendation = 0;
        tab.selected_button = 0;
        tab.rec_scroll.reset();
        tab.progress_status = None;
        tab.activity_frame = 0;
        tab.turn = TurnState::Surfaced {
            prompt,
            outcome: TurnOutcome::ChatTurn,
            end_pending: true,
        };
    }

    /// Flip `end_pending=false` after a final-path surface. Mirrors the
    /// `prompt_complete` log used by the eager path.
    fn turn_release_end_pending(&mut self, session_id: &str) {
        let tab = self.session_tab_mut(session_id);
        if let TurnState::Surfaced {
            end_pending,
            prompt,
            ..
        } = &mut tab.turn
        {
            if *end_pending {
                *end_pending = false;
                let prompt_id = prompt.id;
                let submitted_at = prompt.submitted_at_unix_s;
                prompt_timing_log(prompt_id, submitted_at, "prompt_complete", "via=end_only");
            }
        }
    }
}

/// Computes the rendered height (in terminal rows) of a recommendation card.
/// Includes one trailing row used as the inter-card gap in the rec panel.
pub(crate) fn rec_card_height(choice: &RecommendationChoice, panel_width: u16) -> usize {
    use crate::coordinator::RecommendedAction;
    let inner_width = ui::card::card_content_width(panel_width);

    let text = choice
        .actions
        .iter()
        .find_map(|action| match action {
            RecommendedAction::Send { input, .. } => Some(input.clone()),
            RecommendedAction::OpenAndSend { agent, input, .. } => {
                let label = agent.as_deref().unwrap_or("agent");
                Some(format!("{}: {}", label, input))
            }
            RecommendedAction::Open {
                target, cwd, title, ..
            } => {
                use crate::coordinator::OpenTarget;
                let kind = match target {
                    OpenTarget::Tab => "tab",
                    OpenTarget::Panel => "panel",
                };
                Some(match (title.as_deref(), cwd.as_deref()) {
                    (Some(t), Some(c)) if !t.is_empty() && !c.is_empty() => {
                        format!("New {} ({}) in {}", kind, t, c)
                    }
                    (Some(t), _) if !t.is_empty() => format!("New {} ({})", kind, t),
                    (_, Some(c)) if !c.is_empty() => format!("New {} in {}", kind, c),
                    _ => format!("New {} (empty)", kind),
                })
            }
        })
        .unwrap_or_else(|| choice.title.clone());

    let content_lines: usize = text
        .lines()
        .map(|line| {
            let chars = line.chars().count();
            if chars == 0 {
                1
            } else {
                chars.div_ceil(inner_width)
            }
        })
        .sum::<usize>()
        .max(1);

    // CARD_MIN_SIZE counts 1 content row; add the wrap-extra rows + 1 gap.
    ui::card::CARD_MIN_SIZE as usize + content_lines.saturating_sub(1) + 1
}

/// Computes the rendered height (in terminal rows) of the embedded
/// permission card. No inter-card gap — only one card is ever shown.
pub(crate) fn permission_card_height(perm: &PermissionState, panel_width: u16) -> usize {
    let inner_width = ui::card::card_content_width(panel_width);
    let content_lines: usize = perm
        .description
        .lines()
        .map(|line| {
            let chars = line.chars().count();
            if chars == 0 {
                1
            } else {
                chars.div_ceil(inner_width)
            }
        })
        .sum::<usize>()
        .max(1);
    // CARD_MIN_SIZE counts 1 content row; add the wrap-extra rows.
    ui::card::CARD_MIN_SIZE as usize + content_lines.saturating_sub(1)
}

/// Render a parsed `RecommendationSet` as the agent's "reply" text in chat.
///
/// Recommendation responses arrive as JSON; storing the raw JSON in a completed
/// turn means re-expanding the prompt header reveals raw JSON instead of a
/// CLI-style answer. This builds a single line per choice that mirrors what the
/// recommendation cards show, prefixed with `✓` for the recommended one.
fn format_recommendations_for_chat(set: &RecommendationSet) -> String {
    use crate::coordinator::{OpenTarget, RecommendedAction};

    let header = if set.choices.len() == 1 {
        "Suggested 1 option:".to_string()
    } else {
        format!("Suggested {} options:", set.choices.len())
    };
    let mut out = header;

    for choice in &set.choices {
        let action_text = choice
            .actions
            .iter()
            .find_map(|action| match action {
                RecommendedAction::Send { input, .. } => Some(format!("Run: {}", input)),
                RecommendedAction::OpenAndSend {
                    target,
                    input,
                    agent,
                    ..
                } => {
                    let where_ = match target {
                        OpenTarget::Tab => "new tab",
                        OpenTarget::Panel => "new panel",
                    };
                    let label = agent.as_deref().unwrap_or("agent");
                    Some(format!("Open {} and run {}: {}", where_, label, input))
                }
                RecommendedAction::Open {
                    target, cwd, title, ..
                } => {
                    let kind = match target {
                        OpenTarget::Tab => "tab",
                        OpenTarget::Panel => "panel",
                    };
                    Some(match (title.as_deref(), cwd.as_deref()) {
                        (Some(t), Some(c)) if !t.is_empty() && !c.is_empty() => {
                            format!("Open new {} ({}) in {}", kind, t, c)
                        }
                        (Some(t), _) if !t.is_empty() => format!("Open new {} ({})", kind, t),
                        (_, Some(c)) if !c.is_empty() => format!("Open new {} in {}", kind, c),
                        _ => format!("Open new empty {}", kind),
                    })
                }
            })
            .unwrap_or_else(|| choice.title.clone());

        let marker = if set.recommended_choice == Some(choice.choice) {
            "✓"
        } else {
            " "
        };
        out.push('\n');
        out.push_str(&format!("  {} {}. {}", marker, choice.choice, action_text));
    }

    out
}

impl App {
    /// Push the current agent status (name / version / model / connection state)
    /// to the host so a XAML-rendered agent bar can update itself. The COM
    /// server special-cases `method == "agent_status"` and dispatches it
    /// straight to TerminalPage, parallel to the existing `autofix_state`
    /// path. Cheap to call on every state change — the publisher serializes
    /// `wtcli publish` invocations, and an extra one per state transition is
    /// negligible compared to chat traffic.
    fn publish_agent_status(&mut self) {
        let state_str = match &self.state {
            ConnectionState::Connecting(_) => "connecting",
            ConnectionState::Connected => "connected",
            ConnectionState::Failed(_) => "failed",
            ConnectionState::Disconnected => "disconnected",
        };
        // Include selected_agent only once — when connected after user selection.
        // This avoids triggering _RebuildAgentStack mid-FRE.
        let selected = if self.state == ConnectionState::Connected {
            self.pending_agent_selection.take()
        } else {
            None
        };
        let mut params = serde_json::json!({
            "name": self.agent_name,
            "version": self.agent_version,
            "model": self.agent_model,
            "state": state_str,
            "available_models": self.available_models,
            "current_model_id": self.current_model_id,
        });
        if let Some(agent_id) = selected {
            params["selected_agent"] = serde_json::Value::String(agent_id);
        }
        // Tag with the helper's owned tab so C++ routes the title-bar
        // update to the right AgentPaneContent. Without this, OnAgentStatusChanged
        // fans the event out to every agent pane in every window — fine
        // for single-pane setups, broken once multiple helpers each
        // publish their own status (cross-tab title-bar clobber).
        if let Some(ref tab) = self.owner_tab_id {
            params["tab_id"] = serde_json::Value::String(tab.clone());
        }
        let evt = serde_json::json!({
            "type": "event",
            "method": "agent_status",
            "params": params,
        });
        send_wt_protocol_event(evt.to_string());
    }

    /// Single outbound projection of the active tab's agent-pane UI state.
    ///
    /// **Architecture contract**: per-tab agent-pane UI state lives in wta.
    /// C++ has one shared agent pane and one set of XAML flags per window,
    /// so anything that varies across WT tabs must be re-asserted on every
    /// tab switch or local mutation. Emits one unified `agent_state_changed`
    /// snapshot — adding a new piece of per-tab UI state in the future is
    /// a matter of putting another field in the payload, no new IDL route
    /// or new C++ handler.
    ///
    /// Payload shape (mirror of the inbound `set_agent_state` request):
    /// ```json
    /// {
    ///   "type": "event",
    ///   "method": "agent_state_changed",
    ///   "params": {
    ///     "view":      "chat" | "sessions",
    ///     "pane_open": true | false
    ///   }
    /// }
    /// ```
    ///
    /// On the C++ side this lands in `TerminalPage::OnAgentStateChanged`,
    /// which is the single writer of `_agentSessionsViewActive` and
    /// `Tab.AgentPaneOpen` for the active tab.
    ///
    /// Also re-emits the autofix bar snapshot (orthogonal domain — bottom
    /// bar autofix indicator — kept on its own `autofix_state` route).
    ///
    /// Call sites:
    ///   - `switch_tab_session` end — covers WT `tab_changed`.
    ///   - `set_agent_state` handler end — echoes C++'s request back so C++
    ///     mirrors it (the round-trip the new architecture is built on).
    ///   - `load_session` after the per-tab mutation.
    ///   - Esc out of agent session view, `/sessions` slash command, Ctrl+C×2
    ///     multi-tab reset.
    ///   - Once at startup (after `--initial-view` has been applied) so
    ///     the bar and the agent-pane-open flag both pick up the spawn
    ///     intent.
    ///
    /// Idempotent — safe to call multiple times in a row.
    pub fn project_active_tab_state(&self) {
        let active = self.active_tab_key().to_string();
        self.project_tab_state(&active);
    }

    /// Project the given tab's state to C++ regardless of whether it is the
    /// active tab. Used by `set_agent_state` so a mutation targeting a
    /// non-active tab still echoes back — under per-tab routing C++ can
    /// apply state changes to any tab, not just the focused one, so
    /// the old "defer until next tab_changed" gate was wrong.
    pub fn project_tab_state(&self, target_tab: &str) {
        let Some(tab) = self.tab_sessions.get(target_tab) else {
            tracing::warn!(
                target: "project_tab_state",
                tab_id = %target_tab,
                "no tab_session for target — skipping echo"
            );
            return;
        };
        let view = match tab.current_view {
            View::Agents => "sessions",
            View::Chat => "chat",
        };
        let evt = serde_json::json!({
            "type": "event",
            "method": "agent_state_changed",
            "params": {
                "tab_id":    target_tab,
                "view":      view,
                "pane_open": tab.pane_open,
            }
        });
        send_wt_protocol_event(evt.to_string());

        // Autofix bar is window-level (single bottom bar reflecting the
        // active tab), so only re-emit when we're projecting the active
        // tab. A non-active mutation does not change the visible bar.
        if target_tab == self.active_tab_key() {
            send_bar_event(&tab.autofix.bar_snapshot, Some(target_tab));
        }
    }
}

/// Publish a raw JSON event via `wtcli publish`. The event flows through
/// IProtocolServer::SendEvent; our modified COM server special-cases
/// method=="autofix_state" and dispatches directly to TerminalPage.
///
/// Events are funnelled through a single background thread that waits
/// for each `wtcli publish` subprocess to exit before launching the next.
/// Without this, two rapid emits (e.g. armed → cleared) could race at
/// the OS process-scheduling layer and arrive at WT out of order,
/// leaving the bottom-bar stuck in the earlier state.
pub fn send_wt_protocol_event(json_payload: String) {
    let tx = publisher_sender();
    let _ = tx.send(json_payload);
}


/// Tell WT which pane in `tab_id` should display the blue "Agent" chip.
/// `pane_session_id = None` releases the override and lets the C++ side
/// fall back to its source-of-agent driven default. Fires per-tab; multiple
/// helpers can publish independently and C++ routes each event by tab id.
fn emit_agent_chip_target(tab_id: &str, pane_session_id: Option<&str>) {
    let evt = serde_json::json!({
        "type": "event",
        "method": "set_agent_chip_target",
        "params": {
            "tab_id": tab_id,
            "pane_session_id": pane_session_id,
        }
    });
    send_wt_protocol_event(evt.to_string());
}

fn publisher_sender() -> &'static std::sync::mpsc::Sender<String> {
    static SENDER: std::sync::OnceLock<std::sync::mpsc::Sender<String>> =
        std::sync::OnceLock::new();
    SENDER.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        std::thread::Builder::new()
            .name("wt-event-publisher".into())
            .spawn(move || {
                while let Ok(payload) = rx.recv() {
                    publish_event_blocking(&payload);
                }
            })
            .expect("spawn wt-event-publisher thread");
        tx
    })
}

fn publish_event_blocking(json_payload: &str) {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("wtcli.exe")))
        .filter(|p| p.exists())
        .unwrap_or_else(|| std::path::PathBuf::from("wtcli.exe"));
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("publish").arg(json_payload);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    cmd.stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null());
    match cmd.spawn() {
        Ok(mut child) => {
            // Block the publisher thread until this publish finishes so
            // the next event's subprocess can't overtake it.
            let _ = child.wait();
        }
        Err(_) => {}
    }
}

/// Resolve an agent command like "copilot --acp --stdio" to use the full
/// path if the bare executable isn't on PATH (common in packaged apps).
fn resolve_agent_cmd(cmd: &str) -> String {
    let parts: Vec<&str> = cmd.splitn(2, ' ').collect();
    let exe = parts[0];
    let rest = parts.get(1).copied().unwrap_or("");

    // Already a full path?
    if exe.contains('\\') || exe.contains('/') {
        return cmd.to_string();
    }

    // Use agent_check::find_exe which reads fresh PATH from registry
    let profile = crate::agent_registry::lookup_profile(exe);
    if let Some(full_path) = crate::agent_check::find_exe(profile.id) {
        return if rest.is_empty() {
            full_path
        } else {
            format!("{} {}", full_path, rest)
        };
    }

    // Legacy fallback: check known directories
    let search_dirs: Vec<std::path::PathBuf> = [
        std::env::var("LOCALAPPDATA").ok().map(|l| {
            std::path::PathBuf::from(l)
                .join("Microsoft")
                .join("WinGet")
                .join("Links")
        }),
        std::env::var("APPDATA")
            .ok()
            .map(|a| std::path::PathBuf::from(a).join("npm")),
        std::env::var("USERPROFILE").ok().map(|h| {
            std::path::PathBuf::from(h)
                .join(".claude-cli")
                .join("CurrentVersion")
        }),
    ]
    .into_iter()
    .flatten()
    .collect();

    for dir in &search_dirs {
        for ext in &[".exe", ".cmd"] {
            let full = dir.join(format!("{}{}", exe, ext));
            if full.exists() {
                return if rest.is_empty() {
                    full.to_string_lossy().to_string()
                } else {
                    format!("{} {}", full.to_string_lossy(), rest)
                };
            }
        }
    }

    // Fallback: return as-is
    cmd.to_string()
}

/// Read `agentWelcomeShown` from the packaged app's state.json.
fn welcome_shown_in_state() -> bool {
    find_state_json()
        .and_then(|path| std::fs::read_to_string(&path).ok())
        .map(|content| {
            content.contains("\"agentWelcomeShown\" : true")
                || content.contains("\"agentWelcomeShown\":true")
        })
        .unwrap_or(false)
}

/// Set `agentWelcomeShown` to true in state.json using string replacement
/// to preserve formatting and other fields.
fn set_welcome_shown_in_state() {
    let Some(path) = find_state_json() else {
        return;
    };
    let Ok(content) = std::fs::read_to_string(&path) else {
        return;
    };

    let updated = if content.contains("\"agentWelcomeShown\"") {
        // Replace existing value
        content
            .replace(
                "\"agentWelcomeShown\" : false",
                "\"agentWelcomeShown\" : true",
            )
            .replace(
                "\"agentWelcomeShown\":false",
                "\"agentWelcomeShown\" : true",
            )
    } else if let Some(pos) = content.find('{') {
        // Insert after opening brace
        let (before, after) = content.split_at(pos + 1);
        format!("{}\n\t\"agentWelcomeShown\" : true,{}", before, after)
    } else {
        return;
    };
    let _ = std::fs::write(&path, &updated);
}

/// Find the packaged WT app's state.json.
///
/// Delegates to `runtime_paths::wt_state_json_path`, which:
/// - prefers `GetCurrentPackageFamilyName` when wta itself is packaged
///   (production: dev-sideload **or** store family — both resolve correctly), and
/// - falls back to scanning the `Packages` subdirectory under
///   `%LOCALAPPDATA%` (or `%APPDATA%` when `%LOCALAPPDATA%` is unset)
///   for either known WT family prefix when wta is unpackaged (dev tree
///   launched by packaged WT via `TerminalPage::_DetectWtaPath`).
fn find_state_json() -> Option<std::path::PathBuf> {
    crate::runtime_paths::wt_state_json_path()
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}

fn now_unix_s() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn clamp_cursor_to_boundary(input: &str, cursor_pos: usize) -> usize {
    let mut clamped = cursor_pos.min(input.len());
    while clamped > 0 && !input.is_char_boundary(clamped) {
        clamped -= 1;
    }
    clamped
}

fn prev_char_boundary(input: &str, cursor_pos: usize) -> usize {
    let cursor_pos = clamp_cursor_to_boundary(input, cursor_pos);
    if cursor_pos == 0 {
        return 0;
    }

    input[..cursor_pos]
        .char_indices()
        .last()
        .map(|(idx, _)| idx)
        .unwrap_or(0)
}

fn next_char_boundary(input: &str, cursor_pos: usize) -> usize {
    let cursor_pos = clamp_cursor_to_boundary(input, cursor_pos);
    if cursor_pos >= input.len() {
        return input.len();
    }

    input[cursor_pos..]
        .chars()
        .next()
        .map(|ch| cursor_pos + ch.len_utf8())
        .unwrap_or(input.len())
}

fn is_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

fn next_word_boundary(input: &str, cursor_pos: usize) -> usize {
    let cursor_pos = clamp_cursor_to_boundary(input, cursor_pos);
    if cursor_pos >= input.len() {
        return input.len();
    }

    let mut i = cursor_pos;
    while i < input.len() {
        let ch = input[i..].chars().next().unwrap();
        if is_word_char(ch) {
            break;
        }
        i += ch.len_utf8();
    }
    while i < input.len() {
        let ch = input[i..].chars().next().unwrap();
        if !is_word_char(ch) {
            break;
        }
        i += ch.len_utf8();
    }
    i
}

fn prev_word_boundary(input: &str, cursor_pos: usize) -> usize {
    let cursor_pos = clamp_cursor_to_boundary(input, cursor_pos);
    if cursor_pos == 0 {
        return 0;
    }

    let mut i = cursor_pos;
    while i > 0 {
        let prev = prev_char_boundary(input, i);
        let ch = input[prev..].chars().next().unwrap();
        if is_word_char(ch) {
            break;
        }
        i = prev;
    }
    while i > 0 {
        let prev = prev_char_boundary(input, i);
        let ch = input[prev..].chars().next().unwrap();
        if !is_word_char(ch) {
            break;
        }
        i = prev;
    }
    i
}

// Slash-command behavior tests live in their own file. Declared as a child
// of `app` (not the crate root) so they can reach `App`'s private dispatch
// methods, and `#[path]` keeps the file flat in `src/` like the rest.
#[cfg(test)]
#[path = "slash_command_tests.rs"]
mod slash_command_tests;

// Autofix-trigger reducer tests. Same `#[path]` child-of-`app` pattern as
// slash_command_tests so they can reach `App`'s private dispatch methods and
// the `pub(super)` autofix state fields.
#[cfg(test)]
#[path = "autofix_tests.rs"]
mod autofix_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Custom-agent preflight regression: when the user's `acpAgent` is a
    /// `custom:*` id, the preflight must NOT gate the TUI into Setup mode.
    /// Previously `check_agent("custom:foo")` walked PATH for a literal
    /// `custom:foo.exe`, always failed, and dropped the TUI into Setup with
    /// the misleading `DEFAULT_PROFILE` "Agent" display name — blocking
    /// `/restart` and other chat input until a re-save lifecycle-raced the
    /// preflight failure.
    #[test]
    fn passed_for_custom_agent_never_triggers_setup_mode() {
        let r = PreflightResult::passed_for_custom_agent("custom:foo");
        // Identity preserved on the canonical id (downstream retry/auth
        // paths still see `custom:foo`, not the bare exe name).
        assert_eq!(r.agent_id, "custom:foo");
        // Display name comes from the canonical id stripped of the
        // `custom:` prefix — never the generic `DEFAULT_PROFILE` "Agent".
        assert_eq!(r.display_name, "foo");
        // `all_passed()` must return true so the PreflightComplete handler
        // does NOT enter `AppMode::Setup` ("Agent not installed" banner).
        assert!(r.all_passed());
        assert_eq!(r.cli_status, CheckStatus::Passed);
        assert!(matches!(r.auth_status, CheckStatus::Skipped));
    }

    /// Defensive: a bare `custom:` (empty name) or a non-`custom:` unknown id
    /// must not produce an empty display name. Falls back to the canonical id.
    #[test]
    fn passed_for_custom_agent_falls_back_when_no_custom_suffix() {
        let r = PreflightResult::passed_for_custom_agent("custom:");
        assert_eq!(r.display_name, "custom:");
        assert!(r.all_passed());

        let r2 = PreflightResult::passed_for_custom_agent("some-unknown-id");
        assert_eq!(r2.display_name, "some-unknown-id");
        assert!(r2.all_passed());
    }

    // Helper to create an App for testing (avoids needing real channels for simple state tests).
    // `pub(super)` so the sibling `slash_command_tests` module (see the
    // `#[path]` mod in app.rs) can reuse it instead of duplicating App::new.
    pub(super) fn test_app() -> App {
        let (prompt_tx, _prompt_rx) = tokio::sync::mpsc::unbounded_channel();
        let (recommendation_tx, _recommendation_rx) = tokio::sync::mpsc::unbounded_channel();
        let (permission_tx, _permission_rx) = tokio::sync::mpsc::unbounded_channel();
        let (cancel_tx, _cancel_rx) = tokio::sync::mpsc::unbounded_channel();
        let (new_session_tx, _new_session_rx) = tokio::sync::mpsc::unbounded_channel();
        let (load_session_tx, _load_session_rx) = tokio::sync::mpsc::unbounded_channel();
        let (drop_session_tx, _drop_session_rx) = tokio::sync::mpsc::unbounded_channel();
        let (rename_session_tx, _rename_session_rx) = tokio::sync::mpsc::unbounded_channel();
        let (restart_tx, _restart_rx) = tokio::sync::mpsc::unbounded_channel();
        let debug_capture = Arc::new(AtomicBool::new(false));
        let (master_tx, _master_rx) = tokio::sync::mpsc::unbounded_channel();
        App::new(
            prompt_tx,
            recommendation_tx,
            permission_tx,
            cancel_tx,
            new_session_tx,
            load_session_tx,
            drop_session_tx,
            rename_session_tx,
            restart_tx,
            master_tx,
            debug_capture,
            true,
            false,
            Arc::new(crate::shell::ShellManager::new()),
        )
    }

    /// Bug-1 fix (PR #73 follow-up): an `agent.notification` hook event
    /// arrives with neither `agent_session_id` nor a `pane_session_id`
    /// resolving to a live session — exactly the shape Copilot CLI's
    /// `Notification` hook emits (no `session_id` field in the JSON
    /// payload AND no `WT_SESSION` inherited by the hook subprocess).
    ///
    /// Before the fix, `resolve_or_synthesize_key` produces `pane:<x>`,
    /// the reducer no-ops (synthetic session unknown) AND the synthetic
    /// key gates the event out of the master publish path, so the row
    /// stays at `Working` from the prior `tool.starting`.
    ///
    /// After the fix, the routing layer falls back to the most-recently-
    /// active live session for the same `cli_source` — the row flips to
    /// `Attention` locally AND a real-key event is published to master.
    #[test]
    fn sessionless_notification_falls_back_to_recent_live_cli_session() {
        use crate::agent_sessions::{
            AgentSessionRegistry, AgentStatus, CliSource, SessionEvent,
        };
        let mut reg = AgentSessionRegistry::new();
        // One live Copilot session bound to a known pane.
        reg.apply(SessionEvent::SessionStarted {
            key: "real-copilot-sid".into(),
            cli_source: CliSource::Copilot,
            pane_session_id: "11111111-1111-1111-1111-111111111111".into(),
            cwd: std::path::PathBuf::from("/work"),
            title: "live copilot".into(),
        });
        reg.take_dirty();

        // Notification arrives with an UNRELATED active-pane GUID
        // (user focused on a different pane) and no agent_session_id —
        // mirrors the WT_SESSION-less Copilot hook trace.
        let unrelated_pane = "99999999-9999-9999-9999-999999999999";
        let params = json!({
            "event": "agent.notification",
            "cli_source": "copilot",
            "agent_session_id": "",  // missing — the bug shape
            "payload": { "message": "approve: rm -rf foo" }
        });

        let mut published: Vec<SessionEvent> = Vec::new();
        route_agent_event_to_registry_with_hook_sink(
            &mut reg,
            unrelated_pane,
            &params,
            |ev| published.push(ev),
        );

        // Local reducer flipped the real row to Attention.
        let s = reg.get(&"real-copilot-sid".to_string()).expect("row preserved");
        assert_eq!(
            s.status,
            AgentStatus::Attention,
            "fallback must route the Notification to the live Copilot row",
        );
        assert_eq!(s.attention_reason.as_deref(), Some("approve: rm -rf foo"));

        // Master got a real-key (not synthetic `pane:`) Notification.
        let notif_to_master = published.iter().find_map(|ev| match ev {
            SessionEvent::Notification { key, .. } => Some(key.clone()),
            _ => None,
        });
        assert_eq!(
            notif_to_master.as_deref(),
            Some("real-copilot-sid"),
            "Notification must be published to master keyed by the real session id; \
             synthetic `pane:` keys are dropped from the publish path",
        );
        assert!(
            !published.iter().any(|ev| matches!(
                ev,
                SessionEvent::Notification { key, .. } if key.starts_with("pane:")
            )),
            "no synthetic-key Notification should leak to master",
        );
    }

    /// Turn-based hook status (multi-tool turn bug): Copilot/Gemini fire a
    /// `tool.finished` per tool — several per turn, in parallel batches — but
    /// the agent keeps working until `agent.stop`. A `tool.finished` must NOT
    /// demote the row to Idle (only `agent.stop` ends the turn); otherwise a
    /// multi-tool turn flickers to (and sits at) Idle while the agent is busy.
    #[test]
    fn copilot_tool_finished_keeps_working_only_agent_stop_idles() {
        use crate::agent_sessions::{
            AgentSessionRegistry, AgentStatus, CliSource, SessionEvent,
        };
        let mut reg = AgentSessionRegistry::new();
        let pane = "11111111-1111-1111-1111-111111111111";
        let sid = "copilot-sid";
        reg.apply(SessionEvent::SessionStarted {
            key: sid.into(),
            cli_source: CliSource::Copilot,
            pane_session_id: pane.into(),
            cwd: std::path::PathBuf::from("/work"),
            title: "copilot".into(),
        });
        reg.take_dirty();

        let route = |reg: &mut AgentSessionRegistry, event: &str| {
            let params = json!({
                "event": event,
                "cli_source": "copilot",
                "agent_session_id": sid,
                "payload": { "tool_name": "read_file" }
            });
            route_agent_event_to_registry_with_hook_sink(reg, pane, &params, |_| {});
        };

        // User prompt → Working (turn start).
        route(&mut reg, "agent.prompt.submit");
        assert_eq!(reg.get(&sid.to_string()).unwrap().status, AgentStatus::Working);

        // A parallel batch: three starts, then three finishes.
        route(&mut reg, "agent.tool.starting");
        route(&mut reg, "agent.tool.starting");
        route(&mut reg, "agent.tool.starting");
        assert_eq!(reg.get(&sid.to_string()).unwrap().status, AgentStatus::Working);
        route(&mut reg, "agent.tool.finished");
        assert_eq!(
            reg.get(&sid.to_string()).unwrap().status,
            AgentStatus::Working,
            "first tool.finished must NOT demote while siblings run / the turn continues",
        );
        route(&mut reg, "agent.tool.finished");
        route(&mut reg, "agent.tool.finished");
        assert_eq!(
            reg.get(&sid.to_string()).unwrap().status,
            AgentStatus::Working,
            "tool completions never end the turn",
        );

        // Only agent.stop ends the turn → Idle.
        route(&mut reg, "agent.stop");
        assert_eq!(
            reg.get(&sid.to_string()).unwrap().status,
            AgentStatus::Idle,
            "agent.stop owns the turn-end → Idle",
        );
    }

    /// Counterpart guard: when the event carries a real `agent_session_id`,
    /// the fallback must NOT replace it — the explicit session id always
    /// wins over the heuristic.
    #[test]
    fn notification_with_real_session_id_skips_fallback() {
        use crate::agent_sessions::{
            AgentSessionRegistry, AgentStatus, CliSource, SessionEvent,
        };
        let mut reg = AgentSessionRegistry::new();
        // Two Copilot sessions; `target` is the explicit one in the hook,
        // `other` is the most-recently-active and would win the fallback.
        reg.apply(SessionEvent::SessionStarted {
            key: "target".into(),
            cli_source: CliSource::Copilot,
            pane_session_id: "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".into(),
            cwd: std::path::PathBuf::from("/work"),
            title: "target".into(),
        });
        std::thread::sleep(std::time::Duration::from_millis(5));
        reg.apply(SessionEvent::SessionStarted {
            key: "other".into(),
            cli_source: CliSource::Copilot,
            pane_session_id: "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".into(),
            cwd: std::path::PathBuf::from("/work"),
            title: "other".into(),
        });
        reg.take_dirty();

        let params = json!({
            "event": "agent.notification",
            "cli_source": "copilot",
            "agent_session_id": "target",
            "payload": { "message": "explicit" }
        });
        let unrelated_pane = "99999999-9999-9999-9999-999999999999";
        route_agent_event_to_registry_with_hook_sink(
            &mut reg, unrelated_pane, &params, |_| {},
        );

        assert_eq!(
            reg.get(&"target".to_string()).unwrap().status,
            AgentStatus::Attention,
            "explicit session id must win over the fallback heuristic",
        );
        assert_ne!(
            reg.get(&"other".to_string()).unwrap().status,
            AgentStatus::Attention,
            "fallback target must NOT be touched when explicit sid was supplied",
        );
    }

    /// The fallback must refuse to act when `cli_source` is `Unknown`
    /// (no trustworthy CLI hint); otherwise a sessionless event from an
    /// unknown source could land on whichever live session happened to be
    /// the most recent across ALL CLIs.
    #[test]
    fn sessionless_notification_with_unknown_cli_does_not_fall_back() {
        use crate::agent_sessions::{
            AgentSessionRegistry, AgentStatus, CliSource, SessionEvent,
        };
        let mut reg = AgentSessionRegistry::new();
        reg.apply(SessionEvent::SessionStarted {
            key: "copilot".into(),
            cli_source: CliSource::Copilot,
            pane_session_id: "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".into(),
            cwd: std::path::PathBuf::from("/work"),
            title: "live".into(),
        });

        // No cli_source field at all → CliSource::Unknown — fallback
        // must NOT pick the only live row.
        let params = json!({
            "event": "agent.notification",
            "agent_session_id": "",
            "payload": { "message": "approve?" }
        });
        let _ = route_agent_event_to_registry_with_hook_sink(
            &mut reg,
            "99999999-9999-9999-9999-999999999999",
            &params,
            |_| {},
        );

        assert_ne!(
            reg.get(&"copilot".to_string()).unwrap().status,
            AgentStatus::Attention,
            "fallback must require a trustworthy cli_source hint to avoid \
             routing sessionless events into unrelated CLIs",
        );
    }

    #[test]
    fn session_info_to_agent_session_preserves_live_agent_pane_session_fields() {
        // Regression: master's new_session/load_session handlers stamp
        // status=Idle, cli_source=<resolved>, origin=AgentPane on the
        // SessionInfo so helper-side session management routing sees a Live row. Without
        // this stamping the row would land with all fields None, the
        // converter would map status=None -> AgentStatus::Historical (its
        // documented default), and Enter would fall through to the resume
        // path and fail with "unknown CLI" since cli_source is also None.
        let mut info = crate::session_registry::SessionInfo::new(
            agent_client_protocol::SessionId::new("sid-live"),
            std::path::PathBuf::from("/repo"),
        );
        info.pane_session_id = Some("pane-live".to_string());
        info.status = Some(crate::agent_sessions::AgentStatus::Idle);
        info.cli_source = Some(crate::agent_sessions::CliSource::Copilot);
        info.origin = Some(crate::agent_sessions::SessionOrigin::AgentPane);
        let s = crate::app::session_info_to_agent_session(&info);
        assert_eq!(s.status, crate::agent_sessions::AgentStatus::Idle);
        assert_eq!(s.cli_source, crate::agent_sessions::CliSource::Copilot);
        assert_eq!(s.origin, crate::agent_sessions::SessionOrigin::AgentPane);
        assert_eq!(s.pane_session_id.as_deref(), Some("pane-live"));
    }

    #[test]
    fn session_info_to_agent_session_unstamped_row_falls_to_historical() {
        // Defensive: SessionInfo with all metadata None (the master-side
        // bug we're guarding against) deliberately maps status -> Historical
        // and cli_source -> Unknown(""). This is the WRONG end-state for a
        // Live row but matches the documented fallback. If we ever change
        // the fallback (e.g. to Idle/None) update the docstring on
        // session_info_to_agent_session AND on the master handler
        // comments — silently flipping defaults will mask future bugs.
        let info = crate::session_registry::SessionInfo::new(
            agent_client_protocol::SessionId::new("sid-bare"),
            std::path::PathBuf::from("/repo"),
        );
        let s = crate::app::session_info_to_agent_session(&info);
        assert_eq!(s.status, crate::agent_sessions::AgentStatus::Historical);
        assert!(matches!(
            s.cli_source,
            crate::agent_sessions::CliSource::Unknown(ref v) if v.is_empty()
        ));
    }

    #[test]
    fn helper_agent_event_queues_session_hook_while_updating_local_registry() {
        let mut app = test_app();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        app.set_session_hook_tx(tx);

        app.handle_event(AppEvent::WtEvent {
            method: "agent_event".to_string(),
            pane_id: "pane-hook".to_string(),
            tab_id: Some("tab-1".to_string()),
            params: json!({
                "event": "agent.session.started",
                "cli_source": "copilot",
                "agent_session_id": "sid-hook",
                "payload": {
                    "cwd": r#"C:\repo\hook"#,
                }
            }),
        });

        let queued = rx.try_recv().expect("session_hook event queued");
        assert_eq!(
            queued,
            crate::agent_sessions::SessionEvent::SessionStarted {
                key: "sid-hook".to_string(),
                cli_source: crate::agent_sessions::CliSource::Copilot,
                pane_session_id: "pane-hook".to_string(),
                cwd: std::path::PathBuf::from(r#"C:\repo\hook"#),
                title: "hook".to_string(),
            }
        );
        assert!(
            app.agent_sessions.has_session(&"sid-hook".to_string()),
            "local registry mutation remains in place"
        );
    }

    #[test]
    fn helper_agent_event_queues_synthetic_start_and_followup_hook() {
        let mut app = test_app();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        app.set_session_hook_tx(tx);

        app.handle_event(AppEvent::WtEvent {
            method: "agent_event".to_string(),
            pane_id: "pane-tool".to_string(),
            tab_id: Some("tab-1".to_string()),
            params: json!({
                "event": "agent.tool.starting",
                "cli_source": "copilot",
                "agent_session_id": "sid-tool",
                "payload": {
                    "cwd": r#"C:\repo\tool"#,
                    "tool_name": "edit"
                }
            }),
        });

        assert!(matches!(
            rx.try_recv().expect("synthetic SessionStarted queued"),
            crate::agent_sessions::SessionEvent::SessionStarted { ref key, .. } if key == "sid-tool"
        ));
        assert_eq!(
            rx.try_recv().expect("ToolStarting queued"),
            crate::agent_sessions::SessionEvent::ToolStarting {
                key: "sid-tool".to_string(),
                tool_name: "edit".to_string(),
            }
        );
    }

    #[test]
    fn helper_agent_event_without_agent_session_id_does_not_publish_synthetic_to_master() {
        // Regression for the user-reported duplicate session management row:
        //   "system32  Error                          29 minutes ago"
        //   "Agent pane session b832a8d3: system32  Active · copilot"
        //
        // When an agent_event arrives with no agent_session_id (broken
        // hook, race, or hook from a workspace shell pane that doesn't
        // own an ACP session), the helper used to synthesize a
        // `pane:<guid>` placeholder, apply it locally, AND publish it to
        // master. Master then surfaced the placeholder as a separate
        // session management row alongside the real session, both pointing
        // at the same
        // underlying pane — hence the duplicate.
        //
        // Fix: keep the synthetic placeholder local for helper
        // bookkeeping (is_agent_pane / OSC handler), but DO NOT publish
        // events with `pane:<guid>` keys to master.
        let mut app = test_app();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        app.set_session_hook_tx(tx);

        // Tool event with NO agent_session_id, NO existing pane binding
        // → resolve_or_synthesize_key returns "pane:<guid>", synthetic
        // placeholder created locally, but nothing published to master.
        app.handle_event(AppEvent::WtEvent {
            method: "agent_event".to_string(),
            pane_id: "pane-orphan".to_string(),
            tab_id: Some("tab-1".to_string()),
            params: json!({
                "event": "agent.tool.starting",
                "cli_source": "copilot",
                "payload": {
                    "cwd": r#"C:\repo\hook"#,
                    "tool_name": "edit"
                }
            }),
        });

        assert!(
            rx.try_recv().is_err(),
            "synthetic pane:<guid> events must NOT be published to master"
        );
        // Local registry still has the placeholder for helper-side
        // is_agent_pane / OSC handler bookkeeping.
        assert!(app.agent_sessions.is_agent_pane("pane-orphan"));
    }

    #[test]
    fn helper_agent_event_with_real_agent_session_id_still_publishes_to_master() {
        // Defense against overcorrection: the synthetic-key gate above
        // must not block legitimate events with real agent_session_ids.
        let mut app = test_app();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        app.set_session_hook_tx(tx);

        app.handle_event(AppEvent::WtEvent {
            method: "agent_event".to_string(),
            pane_id: "pane-real".to_string(),
            tab_id: Some("tab-1".to_string()),
            params: json!({
                "event": "agent.tool.starting",
                "cli_source": "copilot",
                "agent_session_id": "real-sid-deadbeef",
                "payload": {
                    "cwd": r#"C:\repo\hook"#,
                    "tool_name": "edit"
                }
            }),
        });

        // Should publish at least one event (likely synthetic
        // SessionStarted + ToolStarting). Both must have the REAL key.
        let mut count = 0;
        while let Ok(evt) = rx.try_recv() {
            match evt {
                crate::agent_sessions::SessionEvent::SessionStarted { key, .. } => {
                    assert_eq!(key, "real-sid-deadbeef", "real session id preserved");
                    count += 1;
                }
                crate::agent_sessions::SessionEvent::ToolStarting { key, .. } => {
                    assert_eq!(key, "real-sid-deadbeef", "real session id preserved");
                    count += 1;
                }
                other => panic!("unexpected event: {:?}", other),
            }
        }
        assert!(count >= 1, "at least one real-keyed event must reach master");
    }

    fn test_app_with_master_rx() -> (
        App,
        tokio::sync::mpsc::UnboundedReceiver<crate::protocol::acp::client::MasterExtRequest>,
    ) {
        let (prompt_tx, _prompt_rx) = tokio::sync::mpsc::unbounded_channel();
        let (recommendation_tx, _recommendation_rx) = tokio::sync::mpsc::unbounded_channel();
        let (permission_tx, _permission_rx) = tokio::sync::mpsc::unbounded_channel();
        let (cancel_tx, _cancel_rx) = tokio::sync::mpsc::unbounded_channel();
        let (new_session_tx, _new_session_rx) = tokio::sync::mpsc::unbounded_channel();
        let (load_session_tx, _load_session_rx) = tokio::sync::mpsc::unbounded_channel();
        let (drop_session_tx, _drop_session_rx) = tokio::sync::mpsc::unbounded_channel();
        let (rename_session_tx, _rename_session_rx) = tokio::sync::mpsc::unbounded_channel();
        let (restart_tx, _restart_rx) = tokio::sync::mpsc::unbounded_channel();
        let (master_tx, master_rx) = tokio::sync::mpsc::unbounded_channel();
        let debug_capture = Arc::new(AtomicBool::new(false));
        let app = App::new(
            prompt_tx,
            recommendation_tx,
            permission_tx,
            cancel_tx,
            new_session_tx,
            load_session_tx,
            drop_session_tx,
            rename_session_tx,
            restart_tx,
            master_tx,
            debug_capture,
            true,
            false,
            Arc::new(crate::shell::ShellManager::new()),
        );
        (app, master_rx)
    }

    // ─── word boundary helpers ──────────────────────────────────────────────

    #[test]
    fn next_word_jumps_to_end_of_current_then_next_word() {
        let s = "hello world";
        // Start of input → end of "hello".
        assert_eq!(next_word_boundary(s, 0), 5);
        // Inside "hello" → end of "hello".
        assert_eq!(next_word_boundary(s, 2), 5);
        // On the space → end of "world".
        assert_eq!(next_word_boundary(s, 5), 11);
        // End of input → stays.
        assert_eq!(next_word_boundary(s, 11), 11);
    }

    #[test]
    fn prev_word_jumps_to_start_of_current_then_previous_word() {
        let s = "hello world";
        // End of input → start of "world".
        assert_eq!(prev_word_boundary(s, 11), 6);
        // On 'w' → start of "hello".
        assert_eq!(prev_word_boundary(s, 6), 0);
        // Inside "world" → start of "world".
        assert_eq!(prev_word_boundary(s, 9), 6);
        // Start of input → stays.
        assert_eq!(prev_word_boundary(s, 0), 0);
    }

    #[test]
    fn word_boundary_skips_punctuation_runs() {
        let s = "foo --bar baz";
        // After "foo" → skip space + "--", land at end of "bar".
        assert_eq!(next_word_boundary(s, 3), 9);
        // From end of "bar" backwards → start of "bar".
        assert_eq!(prev_word_boundary(s, 9), 6);
    }

    #[test]
    fn word_boundary_handles_multibyte_chars() {
        // "你好 world" — each Chinese char is 3 bytes in UTF-8.
        let s = "你好 world";
        assert_eq!(s.len(), 12);
        // Start → end of "你好" (after 2 CJK chars = byte 6).
        assert_eq!(next_word_boundary(s, 0), 6);
        // From end → start of "world" at byte 7.
        assert_eq!(prev_word_boundary(s, 12), 7);
        // From byte 7 (start of "world") → start of "你好" at byte 0.
        assert_eq!(prev_word_boundary(s, 7), 0);
    }

    #[test]
    fn word_boundary_handles_newlines() {
        let s = "foo\nbar";
        // From start → end of "foo".
        assert_eq!(next_word_boundary(s, 0), 3);
        // On '\n' → end of "bar".
        assert_eq!(next_word_boundary(s, 3), 7);
        // From end → start of "bar".
        assert_eq!(prev_word_boundary(s, 7), 4);
    }

    // ─── classify_wt_event ──────────────────────────────────────────────────

    #[test]
    fn classify_connection_failed_is_critical() {
        let params = json!({"session_id": "3", "state": "failed"});
        let n = classify_wt_event("connection_state", "3", None, &params);
        assert_eq!(n.severity, WtEventSeverity::Critical);
        assert!(n.summary.contains("failed"));
        assert!(!n.acknowledged);
    }

    #[test]
    fn classify_connection_closed_is_actionable() {
        let params = json!({"session_id": "5", "state": "closed"});
        let n = classify_wt_event("connection_state", "5", None, &params);
        assert_eq!(n.severity, WtEventSeverity::Actionable);
        assert!(n.summary.contains("exited"));
    }

    #[test]
    fn classify_connection_connected_is_informational() {
        let params = json!({"session_id": "1", "state": "connected"});
        let n = classify_wt_event("connection_state", "1", None, &params);
        assert_eq!(n.severity, WtEventSeverity::Informational);
        assert!(n.summary.contains("connected"));
    }

    #[test]
    fn classify_osc133_command_failed_is_actionable() {
        let params = json!({"session_id": "2", "sequence": "osc:133;D;1"});
        let n = classify_wt_event("vt_sequence", "2", None, &params);
        assert_eq!(n.severity, WtEventSeverity::Actionable);
        assert!(n.summary.contains("Command failed"));
        assert!(n.summary.contains("exit 1"));
    }

    #[test]
    fn classify_osc133_command_success_is_silent() {
        let params = json!({"session_id": "2", "sequence": "osc:133;D;0"});
        let n = classify_wt_event("vt_sequence", "2", None, &params);
        assert!(n.acknowledged); // auto-dismissed
    }

    #[test]
    fn classify_osc133_high_exit_code() {
        let params = json!({"session_id": "2", "sequence": "osc:133;D;127"});
        let n = classify_wt_event("vt_sequence", "2", None, &params);
        assert_eq!(n.severity, WtEventSeverity::Actionable);
        assert!(n.summary.contains("exit 127"));
    }

    #[test]
    fn classify_osc133_prompt_marker_is_silent() {
        // OSC 133;A is a prompt marker, not a command finish
        let params = json!({"session_id": "2", "sequence": "osc:133;A"});
        let n = classify_wt_event("vt_sequence", "2", None, &params);
        assert!(n.acknowledged); // silenced
    }

    #[test]
    fn classify_normal_vt_sequence_is_silent() {
        let params = json!({"session_id": "7", "sequence": "osc:0;title"});
        let n = classify_wt_event("vt_sequence", "7", None, &params);
        assert!(n.acknowledged); // silenced
    }

    #[test]
    fn classify_unknown_method_is_informational() {
        let params = json!({"session_id": "1"});
        let n = classify_wt_event("something_new", "1", None, &params);
        assert_eq!(n.severity, WtEventSeverity::Informational);
    }

    // ─── tab_renamed (tab-drag rekeying) ────────────────────────────────────

    #[test]
    fn tab_renamed_rekeys_active_tab_and_session_map() {
        let mut app = test_app();
        // Seed: active tab is AAAA with a bound ACP session.
        app.tab_id = Some("AAAA".to_string());
        app.tab_sessions
            .insert("AAAA".to_string(), TabSession::default());
        app.session_to_tab
            .insert("sess-1".to_string(), "AAAA".to_string());

        // Drive the rename via the WtEvent dispatch path — same code path
        // a real broadcast from the COM server takes.
        app.handle_event(AppEvent::WtEvent {
            method: "tab_renamed".to_string(),
            pane_id: String::new(),
            tab_id: None,
            params: json!({"old_tab_id": "AAAA", "new_tab_id": "BBBB"}),
        });

        assert_eq!(
            app.tab_id.as_deref(),
            Some("BBBB"),
            "active tab id must follow the rename"
        );
        assert!(
            app.tab_sessions.contains_key("BBBB"),
            "tab_sessions must contain the new key after rename"
        );
        assert!(
            !app.tab_sessions.contains_key("AAAA"),
            "tab_sessions must no longer contain the old key"
        );
        assert_eq!(
            app.session_to_tab.get("sess-1").map(String::as_str),
            Some("BBBB"),
            "session_to_tab values pointing at the old id must be rewritten"
        );
    }

    #[test]
    fn tab_renamed_appevent_variant_drives_same_handler() {
        // Direct AppEvent::TabRenamed dispatch — used by callers that
        // already deserialized the params (mirrors the WtEvent inline
        // path).
        let mut app = test_app();
        app.tab_id = Some("AAAA".to_string());
        app.tab_sessions
            .insert("AAAA".to_string(), TabSession::default());

        app.handle_event(AppEvent::TabRenamed {
            old_tab_id: "AAAA".to_string(),
            new_tab_id: "CCCC".to_string(),
            new_window_id: None,
        });

        assert_eq!(app.tab_id.as_deref(), Some("CCCC"));
        assert!(app.tab_sessions.contains_key("CCCC"));
        assert!(!app.tab_sessions.contains_key("AAAA"));
    }

    #[test]
    fn tab_renamed_sends_rename_session_request_to_acp_client() {
        // The chat-history side rekeys in-process, but tab_to_session
        // lives in the ACP client task — it has to be told to rekey via
        // the rename_session_tx channel. Without this signal, the next
        // prompt on the dragged tab can't find the old SessionId.
        let (prompt_tx, _prompt_rx) = tokio::sync::mpsc::unbounded_channel();
        let (recommendation_tx, _recommendation_rx) = tokio::sync::mpsc::unbounded_channel();
        let (permission_tx, _permission_rx) = tokio::sync::mpsc::unbounded_channel();
        let (cancel_tx, _cancel_rx) = tokio::sync::mpsc::unbounded_channel();
        let (new_session_tx, _new_session_rx) = tokio::sync::mpsc::unbounded_channel();
        let (load_session_tx, _load_session_rx) = tokio::sync::mpsc::unbounded_channel();
        let (drop_session_tx, _drop_session_rx) = tokio::sync::mpsc::unbounded_channel();
        let (rename_session_tx, mut rename_session_rx) = tokio::sync::mpsc::unbounded_channel();
        let (restart_tx, _restart_rx) = tokio::sync::mpsc::unbounded_channel();
        let debug_capture = Arc::new(AtomicBool::new(false));
        let (master_tx, _master_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            prompt_tx,
            recommendation_tx,
            permission_tx,
            cancel_tx,
            new_session_tx,
            load_session_tx,
            drop_session_tx,
            rename_session_tx,
            restart_tx,
            master_tx,
            debug_capture,
            true,
            false,
            Arc::new(crate::shell::ShellManager::new()),
        );

        app.tab_id = Some("AAAA".to_string());
        app.tab_sessions
            .insert("AAAA".to_string(), TabSession::default());

        app.handle_event(AppEvent::TabRenamed {
            old_tab_id: "AAAA".to_string(),
            new_tab_id: "BBBB".to_string(),
            new_window_id: None,
        });

        // The ACP client task should have received exactly one
        // RenameSessionRequest with the old/new ids — that's what makes
        // the dragged tab's chat history line up with the agent's turn
        // context after the drag.
        let req = rename_session_rx
            .try_recv()
            .expect("rename_session_tx must have received a request");
        assert_eq!(req.old_tab_id, "AAAA");
        assert_eq!(req.new_tab_id, "BBBB");
        assert!(
            rename_session_rx.try_recv().is_err(),
            "exactly one request should have been sent"
        );
    }

    #[test]
    fn tab_renamed_noop_does_not_send_rename_session_request() {
        // A no-op rename (old == new) must not bother the ACP client —
        // there's nothing to rekey, and a spurious request would
        // needlessly grab the tab_to_session lock.
        let (prompt_tx, _prompt_rx) = tokio::sync::mpsc::unbounded_channel();
        let (recommendation_tx, _recommendation_rx) = tokio::sync::mpsc::unbounded_channel();
        let (permission_tx, _permission_rx) = tokio::sync::mpsc::unbounded_channel();
        let (cancel_tx, _cancel_rx) = tokio::sync::mpsc::unbounded_channel();
        let (new_session_tx, _new_session_rx) = tokio::sync::mpsc::unbounded_channel();
        let (load_session_tx, _load_session_rx) = tokio::sync::mpsc::unbounded_channel();
        let (drop_session_tx, _drop_session_rx) = tokio::sync::mpsc::unbounded_channel();
        let (rename_session_tx, mut rename_session_rx) = tokio::sync::mpsc::unbounded_channel();
        let (restart_tx, _restart_rx) = tokio::sync::mpsc::unbounded_channel();
        let debug_capture = Arc::new(AtomicBool::new(false));
        let (master_tx, _master_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            prompt_tx,
            recommendation_tx,
            permission_tx,
            cancel_tx,
            new_session_tx,
            load_session_tx,
            drop_session_tx,
            rename_session_tx,
            restart_tx,
            master_tx,
            debug_capture,
            true,
            false,
            Arc::new(crate::shell::ShellManager::new()),
        );

        app.tab_id = Some("AAAA".to_string());
        app.tab_sessions
            .insert("AAAA".to_string(), TabSession::default());

        app.handle_event(AppEvent::TabRenamed {
            old_tab_id: "AAAA".to_string(),
            new_tab_id: "AAAA".to_string(),
            new_window_id: None,
        });

        assert!(
            rename_session_rx.try_recv().is_err(),
            "no-op rename must not send a RenameSessionRequest"
        );
    }

    #[test]
    fn tab_renamed_with_missing_fields_is_dropped() {
        let mut app = test_app();
        app.tab_id = Some("AAAA".to_string());
        app.tab_sessions
            .insert("AAAA".to_string(), TabSession::default());

        // Empty new_tab_id — must not corrupt state.
        app.handle_event(AppEvent::WtEvent {
            method: "tab_renamed".to_string(),
            pane_id: String::new(),
            tab_id: None,
            params: json!({"old_tab_id": "AAAA", "new_tab_id": ""}),
        });
        assert_eq!(
            app.tab_id.as_deref(),
            Some("AAAA"),
            "rename with empty new_tab_id must be dropped, leaving state untouched"
        );
        assert!(app.tab_sessions.contains_key("AAAA"));

        // Missing field entirely — must not corrupt state.
        app.handle_event(AppEvent::WtEvent {
            method: "tab_renamed".to_string(),
            pane_id: String::new(),
            tab_id: None,
            params: json!({"old_tab_id": "AAAA"}),
        });
        assert_eq!(app.tab_id.as_deref(), Some("AAAA"));
        assert!(app.tab_sessions.contains_key("AAAA"));
    }

    // ─── load_session owner_tab_id filter ───────────────────────────────────
    //
    // WT broadcasts `load_session` over shared COM, so every helper in every
    // window receives it. Pre-PR-B, every helper would respond regardless of
    // the target tab — the misroute at the heart of bug #1 (resume into a
    // newly-spawned agent pane landed in the wrong helper). The filter
    // ensures a helper only acts on a `load_session` whose `tab_id` matches
    // its `owner_tab_id`. The legacy single-helper flow (no owner_tab_id)
    // still works as before.

    fn make_app_with_load_session_channel() -> (
        App,
        tokio::sync::mpsc::UnboundedReceiver<crate::protocol::acp::client::LoadSessionForTab>,
    ) {
        let (prompt_tx, _prompt_rx) = tokio::sync::mpsc::unbounded_channel();
        let (recommendation_tx, _recommendation_rx) = tokio::sync::mpsc::unbounded_channel();
        let (permission_tx, _permission_rx) = tokio::sync::mpsc::unbounded_channel();
        let (cancel_tx, _cancel_rx) = tokio::sync::mpsc::unbounded_channel();
        let (new_session_tx, _new_session_rx) = tokio::sync::mpsc::unbounded_channel();
        let (load_session_tx, load_session_rx) = tokio::sync::mpsc::unbounded_channel();
        let (drop_session_tx, _drop_session_rx) = tokio::sync::mpsc::unbounded_channel();
        let (rename_session_tx, _rename_session_rx) = tokio::sync::mpsc::unbounded_channel();
        let (restart_tx, _restart_rx) = tokio::sync::mpsc::unbounded_channel();
        let debug_capture = Arc::new(AtomicBool::new(false));
        let (master_tx, _master_rx) = tokio::sync::mpsc::unbounded_channel();
        let app = App::new(
            prompt_tx,
            recommendation_tx,
            permission_tx,
            cancel_tx,
            new_session_tx,
            load_session_tx,
            drop_session_tx,
            rename_session_tx,
            restart_tx,
            master_tx,
            debug_capture,
            true,
            false,
            Arc::new(crate::shell::ShellManager::new()),
        );
        (app, load_session_rx)
    }

    #[test]
    fn load_session_ignored_when_target_tab_differs_from_owner() {
        let (mut app, mut load_session_rx) = make_app_with_load_session_channel();
        app.owner_tab_id = Some("OWNER-TAB".to_string());

        // Broadcast targeting a different tab — must NOT be forwarded
        // through the load_session_tx channel (otherwise the ACP client
        // would call session/load and bind the wrong tab).
        app.handle_event(AppEvent::WtEvent {
            method: "load_session".to_string(),
            pane_id: String::new(),
            tab_id: None,
            params: json!({
                "tab_id": "OTHER-TAB",
                "session_id": "sess-xyz",
                "cwd": "C:/foo",
            }),
        });

        assert!(
            load_session_rx.try_recv().is_err(),
            "load_session for non-owner tab must be silently dropped"
        );
    }

    #[test]
    fn load_session_applied_when_target_tab_matches_owner() {
        let (mut app, mut load_session_rx) = make_app_with_load_session_channel();
        app.owner_tab_id = Some("OWNER-TAB".to_string());
        app.tab_sessions
            .insert("OWNER-TAB".to_string(), TabSession::default());

        app.handle_event(AppEvent::WtEvent {
            method: "load_session".to_string(),
            pane_id: String::new(),
            tab_id: None,
            params: json!({
                "tab_id": "OWNER-TAB",
                "session_id": "sess-abc",
                "cwd": "C:/foo",
            }),
        });

        let req = load_session_rx
            .try_recv()
            .expect("matching tab id must enqueue a LoadSessionForTab");
        assert_eq!(req.tab_id, "OWNER-TAB");
        assert_eq!(req.session_id, "sess-abc");
        assert_eq!(req.cwd.as_deref(), Some("C:/foo"));
    }

    #[test]
    fn load_session_passes_through_when_owner_tab_id_unset() {
        // Legacy mode: helper spawned without `--owner-tab-id` (the
        // pre-multi-window code path). Filter must be transparent.
        let (mut app, mut load_session_rx) = make_app_with_load_session_channel();
        assert!(app.owner_tab_id.is_none());
        app.tab_sessions
            .insert("ANY-TAB".to_string(), TabSession::default());

        app.handle_event(AppEvent::WtEvent {
            method: "load_session".to_string(),
            pane_id: String::new(),
            tab_id: None,
            params: json!({
                "tab_id": "ANY-TAB",
                "session_id": "sess-legacy",
                "cwd": "",
            }),
        });

        let req = load_session_rx
            .try_recv()
            .expect("legacy mode must still forward load_session");
        assert_eq!(req.session_id, "sess-legacy");
    }

    // ─── SessionAttached load-target gating (Plan-C race fix) ───────────────

    /// After a load_session sets the replay window open, an unrelated
    /// `SessionAttached` (e.g. the bootstrap `session/new` that the helper
    /// always runs at startup) MUST NOT close the window — otherwise
    /// subsequent replay chunks for the real load target get dropped at
    /// the chunk handlers' `if !loading_session { return; }` gate.
    /// This is the exact race the Plan-C
    /// `--initial-load-session-id` boot path was hitting (helper queued
    /// the load_session via AppEvent before bootstrap completed, then
    /// bootstrap SessionAttached arrived and prematurely closed the
    /// window).
    #[test]
    fn session_attached_for_bootstrap_does_not_close_load_replay_window() {
        let (mut app, _load_session_rx) = make_app_with_load_session_channel();
        app.owner_tab_id = Some("OWNER-TAB".to_string());
        app.tab_sessions
            .insert("OWNER-TAB".to_string(), TabSession::default());

        // Open the replay window targeting "sess-target".
        app.handle_event(AppEvent::WtEvent {
            method: "load_session".to_string(),
            pane_id: String::new(),
            tab_id: None,
            params: json!({
                "tab_id": "OWNER-TAB",
                "session_id": "sess-target",
                "cwd": "",
            }),
        });
        assert!(app.tab_sessions["OWNER-TAB"].loading_session);
        assert_eq!(
            app.tab_sessions["OWNER-TAB"]
                .loading_target_session_id
                .as_deref(),
            Some("sess-target")
        );

        // Bootstrap `session/new` completes — SessionAttached for a
        // DIFFERENT session id arrives.
        app.handle_event(AppEvent::SessionAttached {
            tab_id: "OWNER-TAB".to_string(),
            session_id: "sess-bootstrap".to_string(),
            available_models: vec![],
            current_model_id: None,
        });

        // Window MUST still be open so replay chunks for sess-target
        // (which arrive after `session/load` actually runs) are accepted.
        assert!(
            app.tab_sessions["OWNER-TAB"].loading_session,
            "unrelated SessionAttached must not close the load_session replay window"
        );
        assert_eq!(
            app.tab_sessions["OWNER-TAB"]
                .loading_target_session_id
                .as_deref(),
            Some("sess-target"),
            "load target must persist across unrelated SessionAttached"
        );
    }

    /// SessionAttached for the actual load target DOES close the window
    /// (the normal happy path — keep working).
    #[test]
    fn session_attached_for_load_target_closes_replay_window() {
        let (mut app, _load_session_rx) = make_app_with_load_session_channel();
        app.owner_tab_id = Some("OWNER-TAB".to_string());
        app.tab_sessions
            .insert("OWNER-TAB".to_string(), TabSession::default());

        app.handle_event(AppEvent::WtEvent {
            method: "load_session".to_string(),
            pane_id: String::new(),
            tab_id: None,
            params: json!({
                "tab_id": "OWNER-TAB",
                "session_id": "sess-target",
                "cwd": "",
            }),
        });
        assert!(app.tab_sessions["OWNER-TAB"].loading_session);

        app.handle_event(AppEvent::SessionAttached {
            tab_id: "OWNER-TAB".to_string(),
            session_id: "sess-target".to_string(),
            available_models: vec![],
            current_model_id: None,
        });

        assert!(
            !app.tab_sessions["OWNER-TAB"].loading_session,
            "SessionAttached for the load target must close the window"
        );
        assert!(
            app.tab_sessions["OWNER-TAB"]
                .loading_target_session_id
                .is_none(),
            "target id must be cleared after window closes"
        );
    }

    /// TabError must clear both flags so a subsequent load can re-open
    /// the window cleanly.
    #[test]
    fn tab_error_clears_load_target() {
        let (mut app, _load_session_rx) = make_app_with_load_session_channel();
        app.owner_tab_id = Some("OWNER-TAB".to_string());
        app.tab_sessions
            .insert("OWNER-TAB".to_string(), TabSession::default());

        app.handle_event(AppEvent::WtEvent {
            method: "load_session".to_string(),
            pane_id: String::new(),
            tab_id: None,
            params: json!({
                "tab_id": "OWNER-TAB",
                "session_id": "sess-target",
                "cwd": "",
            }),
        });
        assert!(app.tab_sessions["OWNER-TAB"].loading_session);

        app.handle_event(AppEvent::TabError {
            tab_id: "OWNER-TAB".to_string(),
            message: "agent rejected load_session".to_string(),
        });

        assert!(!app.tab_sessions["OWNER-TAB"].loading_session);
        assert!(app.tab_sessions["OWNER-TAB"]
            .loading_target_session_id
            .is_none());
    }

    /// Replayed history must be packed into collapsed CompletedTurn rows
    /// after session/load completes. Each User message opens a new turn;
    /// the prompt header is a short preview (the full original User text
    /// is kept as the first details entry so expanding shows everything).
    /// Subsequent non-User messages become later details. Default
    /// `expanded: false` so the resumed transcript doesn't dump as one
    /// long wall.
    #[test]
    fn pack_replayed_messages_groups_into_collapsed_turns() {
        let mut tab = TabSession::default();
        tab.messages = vec![
            ChatMessage::System("Resuming session abc...".to_string()),
            ChatMessage::User("# Terminal Agent\nYou are...".to_string()),
            ChatMessage::Agent("Hello, I am ready.".to_string()),
            ChatMessage::User("list files".to_string()),
            ChatMessage::ToolCall {
                id: "t1".to_string(),
                title: "ls".to_string(),
                status: "done".to_string(),
            },
            ChatMessage::Agent("Here are the files...".to_string()),
        ];

        tab.pack_replayed_messages_into_turns();

        // System marker stays — it's not anchored to a User.
        assert_eq!(tab.messages.len(), 1);
        assert!(matches!(&tab.messages[0], ChatMessage::System(s) if s.starts_with("Resuming")));

        // Two turns: one per User prompt.
        assert_eq!(tab.completed_turns.len(), 2);

        let t0 = &tab.completed_turns[0];
        // Preview shows first non-empty line + ellipsis (extra lines below).
        assert_eq!(t0.prompt, "# Terminal Agent…");
        // details = [original full User, Agent reply].
        assert_eq!(t0.details.len(), 2);
        assert!(matches!(&t0.details[0], ChatMessage::User(s) if s.starts_with("# Terminal Agent\nYou are")));
        assert!(matches!(&t0.details[1], ChatMessage::Agent(_)));
        assert!(!t0.expanded, "replayed turn must default to collapsed");
        assert!(t0.trailing_marker.is_none());

        let t1 = &tab.completed_turns[1];
        // Short single-line prompt — no ellipsis.
        assert_eq!(t1.prompt, "list files");
        // details = [original User, ToolCall, Agent].
        assert_eq!(t1.details.len(), 3);
        assert!(matches!(&t1.details[0], ChatMessage::User(s) if s == "list files"));
        assert!(matches!(&t1.details[1], ChatMessage::ToolCall { .. }));
        assert!(matches!(&t1.details[2], ChatMessage::Agent(_)));
        assert!(!t1.expanded);
    }

    /// Preview logic: huge single-line prompt must clip to the cap with
    /// a trailing ellipsis; short single-line prompts stay verbatim.
    #[test]
    fn collapsed_prompt_preview_clips_long_single_line() {
        let long = "a".repeat(500);
        let preview = collapsed_prompt_preview(&long);
        // 80 chars + ellipsis.
        assert_eq!(preview.chars().count(), 81);
        assert!(preview.ends_with('…'));

        let short = "hello world";
        assert_eq!(collapsed_prompt_preview(short), "hello world");
        assert!(!collapsed_prompt_preview(short).ends_with('…'));
    }

    /// Edge: messages that come BEFORE the first User must NOT be lost —
    /// they stay in `tab.messages`. Pre-User stray Agent dumps (rare but
    /// possible) should remain visible rather than being silently dropped.
    #[test]
    fn pack_replayed_messages_preserves_pre_user_orphans() {
        let mut tab = TabSession::default();
        tab.messages = vec![
            ChatMessage::System("Resuming...".to_string()),
            ChatMessage::Agent("stray context dump".to_string()),
            ChatMessage::User("hi".to_string()),
            ChatMessage::Agent("hello".to_string()),
        ];

        tab.pack_replayed_messages_into_turns();

        assert_eq!(tab.messages.len(), 2);
        assert!(matches!(&tab.messages[0], ChatMessage::System(_)));
        assert!(matches!(&tab.messages[1], ChatMessage::Agent(s) if s == "stray context dump"));
        assert_eq!(tab.completed_turns.len(), 1);
        assert_eq!(tab.completed_turns[0].prompt, "hi");
        assert!(!tab.completed_turns[0].expanded);
    }

    /// Empty messages must no-op (no panic, no spurious turn).
    #[test]
    fn pack_replayed_messages_empty_is_noop() {
        let mut tab = TabSession::default();
        tab.pack_replayed_messages_into_turns();
        assert!(tab.messages.is_empty());
        assert!(tab.completed_turns.is_empty());
    }

    /// Integration: SessionAttached for the load target must trigger
    /// packing — replayed User/Agent rows must end up as collapsed
    /// CompletedTurn entries, not loose ChatMessage rows.
    #[test]
    fn session_attached_for_load_target_packs_replayed_history() {
        let (mut app, _load_session_rx) = make_app_with_load_session_channel();
        app.owner_tab_id = Some("OWNER-TAB".to_string());
        app.tab_sessions
            .insert("OWNER-TAB".to_string(), TabSession::default());

        app.handle_event(AppEvent::WtEvent {
            method: "load_session".to_string(),
            pane_id: String::new(),
            tab_id: None,
            params: json!({
                "tab_id": "OWNER-TAB",
                "session_id": "sess-target",
                "cwd": "",
            }),
        });
        // Simulate replay chunks landing in messages.
        let tab = app.tab_sessions.get_mut("OWNER-TAB").unwrap();
        tab.messages.push(ChatMessage::User("first prompt".to_string()));
        tab.messages.push(ChatMessage::Agent("first reply".to_string()));
        tab.messages.push(ChatMessage::User("second prompt".to_string()));
        tab.messages.push(ChatMessage::Agent("second reply".to_string()));

        app.handle_event(AppEvent::SessionAttached {
            tab_id: "OWNER-TAB".to_string(),
            session_id: "sess-target".to_string(),
            available_models: vec![],
            current_model_id: None,
        });

        let tab = &app.tab_sessions["OWNER-TAB"];
        assert!(!tab.loading_session);
        assert_eq!(
            tab.completed_turns.len(),
            2,
            "both replayed user prompts must become collapsed CompletedTurn rows"
        );
        for turn in &tab.completed_turns {
            assert!(!turn.expanded, "replayed turns default collapsed");
        }
        // The leading System("Resuming session ...") marker stays in
        // messages — it's not anchored to a User so packing leaves it
        // alone.
        assert!(tab
            .messages
            .iter()
            .all(|m| matches!(m, ChatMessage::System(_))));
    }

    // ─── WtNotification auto-dismiss ────────────────────────────────────────

    #[test]
    fn informational_auto_dismisses_after_threshold() {
        let mut n = WtNotification {
            severity: WtEventSeverity::Informational,
            pane_id: "1".to_string(),
            tab_id: None,
            summary: "test".to_string(),
            acknowledged: false,
            age_ticks: 0,
        };
        assert!(!n.should_auto_dismiss());
        n.age_ticks = 42;
        assert!(!n.should_auto_dismiss());
        n.age_ticks = 43;
        assert!(n.should_auto_dismiss());
    }

    #[test]
    fn critical_never_auto_dismisses() {
        let n = WtNotification {
            severity: WtEventSeverity::Critical,
            pane_id: "1".to_string(),
            tab_id: None,
            summary: "crash".to_string(),
            acknowledged: false,
            age_ticks: 1000,
        };
        assert!(!n.should_auto_dismiss());
    }

    #[test]
    fn actionable_never_auto_dismisses() {
        let n = WtNotification {
            severity: WtEventSeverity::Actionable,
            pane_id: "1".to_string(),
            tab_id: None,
            summary: "exited".to_string(),
            acknowledged: false,
            age_ticks: 1000,
        };
        assert!(!n.should_auto_dismiss());
    }

    // ─── App notification state ─────────────────────────────────────────────

    #[test]
    fn wt_event_critical_raises_banner_only_no_chat() {
        // WT events route through the bottom bar / `wt_notifications` queue,
        // never the agent's chat history. The chat is for agent dialogue;
        // process-lifecycle noise belongs in the bar.
        let mut app = test_app();
        app.handle_event(AppEvent::WtEvent {
            method: "connection_state".to_string(),
            pane_id: "3".to_string(),
            tab_id: None,
            params: json!({"session_id": "3", "state": "failed"}),
        });
        assert!(app.show_notification_banner);
        assert_eq!(app.wt_notifications.len(), 1);
        assert_eq!(app.wt_notifications[0].severity, WtEventSeverity::Critical);
        assert!(
            app.current_tab().messages.is_empty(),
            "WT events must not pollute chat history with Error messages"
        );
    }

    #[test]
    fn wt_event_actionable_raises_banner_only_no_chat() {
        let mut app = test_app();
        app.handle_event(AppEvent::WtEvent {
            method: "connection_state".to_string(),
            pane_id: "5".to_string(),
            tab_id: None,
            params: json!({"session_id": "5", "state": "closed"}),
        });
        assert!(app.show_notification_banner);
        assert!(
            app.current_tab().messages.is_empty(),
            "WT events must not pollute chat history with System messages"
        );
    }

    #[test]
    fn wt_event_informational_no_banner_no_chat_message() {
        let mut app = test_app();
        app.handle_event(AppEvent::WtEvent {
            method: "connection_state".to_string(),
            pane_id: "1".to_string(),
            tab_id: None,
            params: json!({"session_id": "1", "state": "connected"}),
        });
        assert!(!app.show_notification_banner);
        assert!(app.current_tab().messages.is_empty());
        assert_eq!(app.wt_notifications.len(), 1);
    }

    #[test]
    fn wt_event_from_own_pane_is_ignored() {
        let mut app = test_app();
        app.pane_id = Some("42".to_string());
        app.handle_event(AppEvent::WtEvent {
            method: "connection_state".to_string(),
            pane_id: "42".to_string(),
            tab_id: None,
            params: json!({"session_id": "42", "state": "failed"}),
        });
        // Events from our own pane should be completely ignored
        assert!(!app.show_notification_banner);
        assert!(app.wt_notifications.is_empty());
        assert!(app.current_tab().messages.is_empty());
    }

    #[test]
    fn wt_event_critical_from_other_tab_does_not_surface_in_owner_tab() {
        // Regression for the cross-tab "Pane …: connection failed" leak:
        // helper A owns tab A; tab B's Copilot pane fails; WT broadcasts
        // the `connection_state:failed` event to every helper. Helper A
        // must drop it instead of writing a red Error into tab A's chat.
        let mut app = test_app();
        app.owner_tab_id = Some("{tab-A}".to_string());
        app.handle_event(AppEvent::WtEvent {
            method: "connection_state".to_string(),
            pane_id: "B-PANE".to_string(),
            tab_id: Some("{tab-B}".to_string()),
            params: json!({"pane_id": "B-PANE", "state": "failed", "tab_id": "{tab-B}"}),
        });
        assert!(!app.show_notification_banner);
        assert!(app.wt_notifications.is_empty());
        assert!(app.current_tab().messages.is_empty());
    }

    #[test]
    fn wt_event_critical_from_owner_tab_raises_banner_not_chat() {
        // Same-tab event raises the banner but still does NOT push into chat
        // — the bar is the user-visible surface for connection failures.
        let mut app = test_app();
        app.owner_tab_id = Some("{tab-A}".to_string());
        app.handle_event(AppEvent::WtEvent {
            method: "connection_state".to_string(),
            pane_id: "A-PANE".to_string(),
            tab_id: Some("{tab-A}".to_string()),
            params: json!({"pane_id": "A-PANE", "state": "failed", "tab_id": "{tab-A}"}),
        });
        assert!(app.show_notification_banner);
        assert_eq!(app.wt_notifications.len(), 1);
        assert!(
            app.current_tab().messages.is_empty(),
            "WT events must not pollute chat history"
        );
    }

    #[test]
    fn dismiss_notifications_clears_banner_and_acknowledges() {
        let mut app = test_app();
        app.handle_event(AppEvent::WtEvent {
            method: "connection_state".to_string(),
            pane_id: "3".to_string(),
            tab_id: None,
            params: json!({"session_id": "3", "state": "failed"}),
        });
        assert!(app.show_notification_banner);
        assert_eq!(app.unacknowledged_count(), 1);

        app.dismiss_notifications();
        assert!(!app.show_notification_banner);
        assert_eq!(app.unacknowledged_count(), 0);
        assert!(app.wt_notifications[0].acknowledged);
    }

    #[test]
    fn notification_badge_returns_most_recent_unacknowledged() {
        let mut app = test_app();
        // First event
        app.handle_event(AppEvent::WtEvent {
            method: "connection_state".to_string(),
            pane_id: "1".to_string(),
            tab_id: None,
            params: json!({"session_id": "1", "state": "closed"}),
        });
        // Second event (more recent)
        app.handle_event(AppEvent::WtEvent {
            method: "connection_state".to_string(),
            pane_id: "2".to_string(),
            tab_id: None,
            params: json!({"session_id": "2", "state": "failed"}),
        });

        let (summary, severity) = app.notification_badge().unwrap();
        assert!(summary.contains("Pane 2"));
        assert_eq!(*severity, WtEventSeverity::Critical);
        assert_eq!(app.unacknowledged_count(), 2);
    }

    #[test]
    fn notification_queue_caps_at_20() {
        let mut app = test_app();
        for i in 0..25 {
            app.handle_event(AppEvent::WtEvent {
                method: "connection_state".to_string(),
                pane_id: format!("{}", i),
                tab_id: None,
                params: json!({"session_id": format!("{}", i), "state": "connected"}),
            });
        }
        assert_eq!(app.wt_notifications.len(), 20);
    }

    #[test]
    fn tick_ages_and_auto_dismisses_informational() {
        let mut app = test_app();
        app.handle_event(AppEvent::WtEvent {
            method: "connection_state".to_string(),
            pane_id: "1".to_string(),
            tab_id: None,
            params: json!({"session_id": "1", "state": "connected"}),
        });
        assert_eq!(app.wt_notifications.len(), 1);
        assert_eq!(app.wt_notifications[0].age_ticks, 0);

        // Simulate enough ticks to trigger auto-dismiss (43 ticks)
        for _ in 0..43 {
            app.handle_event(AppEvent::Tick);
        }
        // Informational notification should be auto-removed
        assert_eq!(app.wt_notifications.len(), 0);
    }

    #[test]
    fn tick_does_not_dismiss_critical_notifications() {
        let mut app = test_app();
        app.handle_event(AppEvent::WtEvent {
            method: "connection_state".to_string(),
            pane_id: "3".to_string(),
            tab_id: None,
            params: json!({"session_id": "3", "state": "failed"}),
        });
        // Simulate many ticks
        for _ in 0..200 {
            app.handle_event(AppEvent::Tick);
        }
        // Critical notification should persist
        assert_eq!(app.wt_notifications.len(), 1);
        assert!(app.show_notification_banner);
    }

    #[test]
    fn banner_hides_when_all_acknowledged() {
        let mut app = test_app();
        app.handle_event(AppEvent::WtEvent {
            method: "connection_state".to_string(),
            pane_id: "3".to_string(),
            tab_id: None,
            params: json!({"session_id": "3", "state": "failed"}),
        });
        assert!(app.show_notification_banner);

        // Acknowledge all
        app.dismiss_notifications();

        // One more tick to process the banner-hide logic
        app.handle_event(AppEvent::Tick);
        assert!(!app.show_notification_banner);
    }

    #[test]
    fn active_notification_returns_none_when_all_acknowledged() {
        let mut app = test_app();
        app.handle_event(AppEvent::WtEvent {
            method: "connection_state".to_string(),
            pane_id: "3".to_string(),
            tab_id: None,
            params: json!({"session_id": "3", "state": "closed"}),
        });
        assert!(app.active_notification().is_some());

        app.dismiss_notifications();
        assert!(app.active_notification().is_none());
    }

    #[test]
    fn multiple_events_different_panes() {
        let mut app = test_app();
        // Informational from pane 1
        app.handle_event(AppEvent::WtEvent {
            method: "connection_state".to_string(),
            pane_id: "1".to_string(),
            tab_id: None,
            params: json!({"session_id": "1", "state": "connected"}),
        });
        // Critical from pane 2
        app.handle_event(AppEvent::WtEvent {
            method: "connection_state".to_string(),
            pane_id: "2".to_string(),
            tab_id: None,
            params: json!({"session_id": "2", "state": "failed"}),
        });
        // Actionable from pane 3
        app.handle_event(AppEvent::WtEvent {
            method: "connection_state".to_string(),
            pane_id: "3".to_string(),
            tab_id: None,
            params: json!({"session_id": "3", "state": "closed"}),
        });

        assert_eq!(app.wt_notifications.len(), 3);
        // Unacknowledged count only counts actionable + critical
        assert_eq!(app.unacknowledged_count(), 2);
        // Banner should show (due to critical + actionable)
        assert!(app.show_notification_banner);
        // Chat must stay empty — WT events surface in the bar/banner, never
        // in agent dialogue.
        assert!(app.current_tab().messages.is_empty());
    }

    // ─── Task C: Agents snapshot viewer / master refetch ────────────────────

    #[test]
    fn agents_view_open_sends_sessions_list_request() {
        let (mut app, mut master_rx) = test_app_with_master_rx();
        app.open_agents_view_for_tab(DEFAULT_TAB_ID.to_string());
        match master_rx
            .try_recv()
            .expect("open must request sessions/list")
        {
            crate::protocol::acp::client::MasterExtRequest::SessionsList { .. } => {}
            other => panic!("expected SessionsList, got {other:?}"),
        }
        assert!(app.current_tab().agents_view.snapshot.is_some());
        assert!(app.current_tab().agents_view.refetch_in_flight);
    }

    #[test]
    fn sessions_changed_with_open_agents_view_schedules_refetch() {
        let (mut app, mut master_rx) = test_app_with_master_rx();
        app.current_tab_mut().current_view = View::Agents;
        app.current_tab_mut().agents_view.snapshot = Some(Vec::new());
        app.handle_event(AppEvent::SessionsChanged);
        match master_rx.try_recv().expect("change must request refetch") {
            crate::protocol::acp::client::MasterExtRequest::SessionsList { .. } => {}
            other => panic!("expected SessionsList, got {other:?}"),
        }
        assert!(app.current_tab().agents_view.refetch_in_flight);
    }

    #[test]
    fn sessions_changed_with_closed_agents_view_is_noop() {
        let (mut app, mut master_rx) = test_app_with_master_rx();
        app.current_tab_mut().current_view = View::Chat;
        app.current_tab_mut().agents_view.snapshot = None;
        app.handle_event(AppEvent::SessionsChanged);
        assert!(master_rx.try_recv().is_err(), "closed UI must not refetch");
    }

    // ─── /model per-pane override ───────────────────────────────────────────

    fn model_info(id: &str) -> AcpModelInfo {
        AcpModelInfo {
            id: id.to_string(),
            name: id.to_uppercase(),
            description: None,
        }
    }

    /// `/model <id>` records a per-pane override and hot-applies it to *that*
    /// tab's live session (a targeted `SetSessionModel`, not a fan-out).
    #[test]
    fn model_pick_overrides_and_applies_to_live_session() {
        use crate::protocol::acp::client::MasterExtRequest;
        let (mut app, mut master_rx) = test_app_with_master_rx();
        app.available_models = vec![model_info("gpt-5.5"), model_info("gpt-5.4")];
        app.current_tab_mut().session_id = Some("sid-1".into());

        app.cmd_model("gpt-5.4".into());

        assert_eq!(
            app.current_tab().model_override.as_deref(),
            Some("gpt-5.4"),
            "the pane records its per-pane override"
        );
        match master_rx
            .try_recv()
            .expect("a live session gets set_session_model")
        {
            MasterExtRequest::SetSessionModel { session_id, model } => {
                assert_eq!(model, "gpt-5.4");
                assert_eq!(
                    session_id.expect("targets just this session").0.to_string(),
                    "sid-1"
                );
            }
            other => panic!("expected SetSessionModel, got {other:?}"),
        }
    }

    /// A global `acpModel` settings change is authoritative: it overrides a
    /// pane's local `/model` pick — clearing the override, redirecting the
    /// shared current model, and pushing the new model to the pane's session.
    #[test]
    fn global_settings_change_overrides_local_pick() {
        use crate::protocol::acp::client::MasterExtRequest;
        let (mut app, mut master_rx) = test_app_with_master_rx();
        app.available_models = vec![model_info("local"), model_info("globalv2")];
        app.current_tab_mut().session_id = Some("sid-1".into());

        // Pane pins a local model first.
        app.cmd_model("local".into());
        let _ = master_rx.try_recv(); // drain the pick's own apply
        assert_eq!(app.current_tab().model_override.as_deref(), Some("local"));

        // Global settings change to a different model — authoritative.
        app.apply_global_acp_model(Some("globalv2".into()));

        assert_eq!(
            app.current_tab().model_override,
            None,
            "a global change clears the per-pane override"
        );
        assert_eq!(
            app.current_model_id.as_deref(),
            Some("globalv2"),
            "the shared current model follows the new global value"
        );
        match master_rx
            .try_recv()
            .expect("the previously-overridden pane still gets the new global model")
        {
            MasterExtRequest::SetSessionModel { session_id, model } => {
                assert_eq!(model, "globalv2");
                assert_eq!(session_id.unwrap().0.to_string(), "sid-1");
            }
            other => panic!("expected SetSessionModel, got {other:?}"),
        }
    }

    /// A pane with no local pick follows the global `acpModel` on hot-reload.
    #[test]
    fn non_overridden_pane_follows_global_model() {
        use crate::protocol::acp::client::MasterExtRequest;
        let (mut app, mut master_rx) = test_app_with_master_rx();
        app.current_tab_mut().session_id = Some("sid-1".into());
        app.acp_model = Some("global".into());

        app.send_acp_model_update();

        match master_rx
            .try_recv()
            .expect("non-overridden pane follows global")
        {
            MasterExtRequest::SetSessionModel { session_id, model } => {
                assert_eq!(model, "global");
                assert_eq!(session_id.unwrap().0.to_string(), "sid-1");
            }
            other => panic!("expected SetSessionModel, got {other:?}"),
        }
    }

    /// `/model` with an unrecognized argument warns and changes nothing.
    #[test]
    fn model_pick_rejects_unknown_model() {
        let (mut app, mut master_rx) = test_app_with_master_rx();
        app.available_models = vec![model_info("known")];
        app.current_tab_mut().session_id = Some("sid-1".into());

        app.cmd_model("nope".into());

        assert!(
            app.current_tab().model_override.is_none(),
            "an unknown model must not set an override"
        );
        assert!(
            master_rx.try_recv().is_err(),
            "an unknown model must not emit a set_session_model"
        );
    }

    /// MVP sessions origin filter: with `ShellOnly`, agent-pane rows must
    /// be hidden from `agents_rows_for_tab` (the cursor / Enter
    /// dispatch source of truth) — *not just* from `agents_view::render`.
    /// A bug where render filtered but `agents_rows_for_tab` didn't
    /// would let Enter on visible row N activate hidden row M.
    #[test]
    fn shell_only_filter_hides_agent_pane_rows_from_cursor_model() {
        use crate::agent_sessions::{OriginFilter, SessionOrigin};
        let mut app = test_app();
        app.sessions_origin_filter = OriginFilter::ShellOnly;
        // Snapshot path: master pushed two rows — one tagged
        // AgentPane (Class A, hidden under ShellOnly), one tagged
        // Unknown (Class B, visible).
        let mut pane = session_info_for_test("class-a");
        pane.origin = Some(SessionOrigin::AgentPane);
        pane.last_activity_at_ms = Some(200);
        let mut shell = session_info_for_test("class-b");
        shell.origin = Some(SessionOrigin::Unknown);
        shell.last_activity_at_ms = Some(100);
        app.current_tab_mut().agents_view.snapshot = Some(vec![pane, shell]);

        let rows = app.agents_rows_for_tab(DEFAULT_TAB_ID);
        assert_eq!(rows.len(), 1, "only the Class B row is visible: {rows:?}");
        assert_eq!(rows[0].key, "class-b");

        // Flip to All — both rows must reappear so the un-MVP toggle
        // brings agent-pane rows back without any other code change.
        app.sessions_origin_filter = OriginFilter::All;
        let rows = app.agents_rows_for_tab(DEFAULT_TAB_ID);
        assert_eq!(rows.len(), 2);

        // AgentPaneOnly is the inverse — only Class A surfaces.
        app.sessions_origin_filter = OriginFilter::AgentPaneOnly;
        let rows = app.agents_rows_for_tab(DEFAULT_TAB_ID);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].key, "class-a");
    }

    /// Registry path (no snapshot): the same filter must apply when
    /// `agents_rows_for_tab` falls back to `agent_sessions` directly.
    /// Without this, helpers that haven't received a master snapshot
    /// yet would show every row regardless of the MVP filter.
    #[test]
    fn shell_only_filter_applies_to_registry_fallback_path() {
        use crate::agent_sessions::{CliSource, OriginFilter, SessionEvent, SessionOrigin};
        use std::path::PathBuf;
        let mut app = test_app();
        app.sessions_origin_filter = OriginFilter::ShellOnly;
        // No snapshot primed — `agents_rows_for_tab` goes through
        // `iter_sorted_with_filters` on the registry.
        app.agent_sessions.apply(SessionEvent::SessionStarted {
            key: "shell-key".into(),
            cli_source: CliSource::Claude,
            pane_session_id: "00000000-0000-0000-0000-00000000aaaa".into(),
            cwd: PathBuf::from("/x"),
            title: "shell".into(),
        });
        app.agent_sessions.apply(SessionEvent::SessionStarted {
            key: "pane-key".into(),
            cli_source: CliSource::Claude,
            pane_session_id: "00000000-0000-0000-0000-00000000bbbb".into(),
            cwd: PathBuf::from("/x"),
            title: "pane".into(),
        });
        app.agent_sessions.set_origin("pane-key", SessionOrigin::AgentPane);

        let rows = app.agents_rows_for_tab(DEFAULT_TAB_ID);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].key, "shell-key");
    }

    /// The PRODUCTION snapshot path (master pushed `sessions/list` response
    /// into `agents_view.snapshot`) must preserve the `Wsl` location in every
    /// `AgentSession` produced by `agents_rows_for_tab`.
    ///
    /// This is the regression test that would have caught the original bug:
    /// `session_info_to_agent_session` hardcoded `location: Host`, so WSL
    /// rows crossing the master→helper boundary silently lost their distro
    /// stamp.  The fix carries `location` through `SessionInfo`; this test
    /// guards that fix forever.
    #[test]
    fn agents_rows_snapshot_preserves_wsl_location() {
        use crate::agent_sessions::{OriginFilter, SessionLocation};

        let mut app = test_app();
        // Use `All` to bypass the MVP ShellOnly filter — we want to confirm
        // location preservation regardless of origin filtering.
        app.sessions_origin_filter = OriginFilter::All;

        let mut info = session_info_for_test("wsl-1");
        info.origin = Some(crate::agent_sessions::SessionOrigin::Unknown);
        info.location = SessionLocation::Wsl { distro: "Ubuntu".into() };

        app.current_tab_mut().current_view = View::Agents;
        app.current_tab_mut().agents_view.snapshot = Some(vec![info]);

        let rows = app.agents_rows_for_tab(DEFAULT_TAB_ID);
        assert_eq!(rows.len(), 1, "expected one row; got: {rows:?}");
        assert!(
            rows[0].location.is_wsl(),
            "snapshot path must preserve WSL location; got: {:?}",
            rows[0].location
        );
        assert_eq!(
            rows[0].location,
            SessionLocation::Wsl { distro: "Ubuntu".into() },
            "distro name must round-trip through session_info_to_agent_session"
        );
    }

    /// End-to-end render proof: a WSL `SessionInfo` in the `/sessions`
    /// snapshot must actually paint its bracketed distro tag (`[WSL-Ubuntu]`)
    /// on screen. `agents_rows_snapshot_preserves_wsl_location` proves the
    /// data path and `origin_prefix_shows_distro_for_wsl_rows` proves the
    /// prefix builder; this closes the loop through `crate::ui::render` so a
    /// regression in `agents_view::render`'s own `session_info_to_agent_session`
    /// conversion (a *second* call site, separate from `agents_rows_for_tab`)
    /// can't silently drop the tag.
    #[test]
    fn render_sessions_view_paints_wsl_distro_tag() {
        use crate::agent_sessions::{OriginFilter, SessionLocation};

        let mut app = test_app();
        app.state = ConnectionState::Connected;
        app.sessions_origin_filter = OriginFilter::All;

        let mut info = session_info_for_test("wsl-render-1");
        info.title = Some("hack on wsl".into());
        info.origin = Some(crate::agent_sessions::SessionOrigin::Unknown);
        info.location = SessionLocation::Wsl { distro: "Ubuntu".into() };

        app.current_tab_mut().current_view = View::Agents;
        app.current_tab_mut().agents_view.snapshot = Some(vec![info]);

        let text = render_to_text(&mut app, 80, 24);
        assert!(
            text.contains("[WSL-Ubuntu]"),
            "the /sessions view must paint the bracketed WSL distro tag; rendered:\n{text}"
        );
    }

    /// `resolve_sessions_origin_filter` reads the `WTA_SESSIONS_SHOW_AGENT_PANE`
    /// env var. With it unset (or 0/false) the MVP default
    /// (`ShellOnly`) wins; with it set to a truthy value we flip to
    /// `All` so a single debug helper can see everything without a
    /// rebuild.
    ///
    /// Env vars are process-global, so this test serializes via the
    /// `WTA_SESSIONS_SHOW_AGENT_PANE_TEST_LOCK` mutex shared with any other
    /// future test that touches the same var.
    #[test]
    fn resolve_sessions_origin_filter_respects_env_override() {
        use crate::agent_sessions::OriginFilter;
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());

        std::env::remove_var("WTA_SESSIONS_SHOW_AGENT_PANE");
        assert_eq!(crate::app::resolve_sessions_origin_filter(), MVP_SESSIONS_ORIGIN_FILTER);
        assert_eq!(MVP_SESSIONS_ORIGIN_FILTER, OriginFilter::ShellOnly);

        std::env::set_var("WTA_SESSIONS_SHOW_AGENT_PANE", "1");
        assert_eq!(crate::app::resolve_sessions_origin_filter(), OriginFilter::All);

        std::env::set_var("WTA_SESSIONS_SHOW_AGENT_PANE", "true");
        assert_eq!(crate::app::resolve_sessions_origin_filter(), OriginFilter::All);

        std::env::set_var("WTA_SESSIONS_SHOW_AGENT_PANE", "0");
        assert_eq!(crate::app::resolve_sessions_origin_filter(), MVP_SESSIONS_ORIGIN_FILTER);

        std::env::remove_var("WTA_SESSIONS_SHOW_AGENT_PANE");
    }

    #[test]
    fn snapshot_refetch_preserves_focused_sid() {
        let (mut app, mut master_rx) = test_app_with_master_rx();
        app.open_agents_view_for_tab(DEFAULT_TAB_ID.to_string());
        let first_req = match master_rx.try_recv().unwrap() {
            crate::protocol::acp::client::MasterExtRequest::SessionsList { request_id, .. } => {
                request_id
            }
            other => panic!("expected SessionsList, got {other:?}"),
        };
        app.handle_event(AppEvent::AgentsSnapshotLoaded {
            request_id: first_req,
            sessions: vec![
                session_info_for_test("a"),
                session_info_for_test("b"),
                session_info_for_test("c"),
            ],
        });
        app.current_tab_mut().agents_list_state.select(Some(1));
        app.current_tab_mut().agents_view.focused_sid =
            Some(agent_client_protocol::SessionId::new("b"));
        app.handle_event(AppEvent::SessionsChanged);
        let second_req = match master_rx.try_recv().unwrap() {
            crate::protocol::acp::client::MasterExtRequest::SessionsList { request_id, .. } => {
                request_id
            }
            other => panic!("expected SessionsList, got {other:?}"),
        };
        app.handle_event(AppEvent::AgentsSnapshotLoaded {
            request_id: second_req,
            sessions: vec![
                session_info_for_test("c"),
                session_info_for_test("a"),
                session_info_for_test("b"),
            ],
        });
        assert_eq!(app.current_tab().agents_list_state.selected(), Some(2));
        assert_eq!(
            app.current_tab()
                .agents_view
                .focused_sid
                .as_ref()
                .map(|s| s.0.as_ref()),
            Some("b")
        );
    }

    #[test]
    fn sessions_changed_coalesces_rapid_pushes() {
        let (mut app, mut master_rx) = test_app_with_master_rx();
        app.current_tab_mut().current_view = View::Agents;
        app.current_tab_mut().agents_view.snapshot = Some(Vec::new());
        for _ in 0..100 {
            app.handle_event(AppEvent::SessionsChanged);
        }
        let first_req = match master_rx.try_recv().expect("one in-flight refetch") {
            crate::protocol::acp::client::MasterExtRequest::SessionsList { request_id, .. } => {
                request_id
            }
            other => panic!("expected SessionsList, got {other:?}"),
        };
        assert!(
            master_rx.try_recv().is_err(),
            "rapid pushes coalesce while in flight"
        );
        assert!(app.current_tab().agents_view.refetch_in_flight);
        assert!(app.current_tab().agents_view.dirty);
        app.handle_event(AppEvent::AgentsSnapshotLoaded {
            request_id: first_req,
            sessions: Vec::new(),
        });
        match master_rx.try_recv().expect("dirty trailing refetch") {
            crate::protocol::acp::client::MasterExtRequest::SessionsList { .. } => {}
            other => panic!("expected SessionsList, got {other:?}"),
        }
        assert!(
            master_rx.try_recv().is_err(),
            "at most one trailing refetch"
        );
    }

    /// Failure / timeout path must unblock `refetch_in_flight` so the
    /// next `SessionsChanged` (from a broadcast or the 5s tick) can
    /// retry, while keeping the existing snapshot rendered. Without
    /// this, an `ext_method` future that never resolves (the ACP-0.10
    /// cancellation-safety bug) would freeze the view forever.
    #[test]
    fn agents_snapshot_failed_unblocks_refetch_without_dropping_snapshot() {
        let (mut app, mut master_rx) = test_app_with_master_rx();
        app.open_agents_view_for_tab(DEFAULT_TAB_ID.to_string());
        let first_req = match master_rx.try_recv().unwrap() {
            crate::protocol::acp::client::MasterExtRequest::SessionsList { request_id, .. } => {
                request_id
            }
            other => panic!("expected SessionsList, got {other:?}"),
        };
        // Land a real snapshot first so we can assert it is preserved
        // across the subsequent failure.
        app.handle_event(AppEvent::AgentsSnapshotLoaded {
            request_id: first_req,
            sessions: vec![session_info_for_test("a"), session_info_for_test("b")],
        });
        assert!(!app.current_tab().agents_view.refetch_in_flight);
        let before_len = app
            .current_tab()
            .agents_view
            .snapshot
            .as_ref()
            .map(|v| v.len())
            .unwrap_or(0);
        assert_eq!(before_len, 2);

        // Kick a second refetch and report it as failed.
        app.handle_event(AppEvent::SessionsChanged);
        let second_req = match master_rx.try_recv().expect("second refetch sent") {
            crate::protocol::acp::client::MasterExtRequest::SessionsList { request_id, .. } => {
                request_id
            }
            other => panic!("expected SessionsList, got {other:?}"),
        };
        assert!(app.current_tab().agents_view.refetch_in_flight);
        app.handle_event(AppEvent::AgentsSnapshotFailed {
            request_id: second_req,
        });

        // refetch_in_flight must clear; snapshot must NOT be wiped.
        assert!(
            !app.current_tab().agents_view.refetch_in_flight,
            "failure path must unblock the gate"
        );
        let after_len = app
            .current_tab()
            .agents_view
            .snapshot
            .as_ref()
            .map(|v| v.len())
            .unwrap_or(0);
        assert_eq!(
            after_len, 2,
            "failure path must not overwrite the existing snapshot"
        );
        assert!(
            master_rx.try_recv().is_err(),
            "no spurious immediate retry without dirty coalescing"
        );
    }

    /// If pushes arrive while the in-flight `sessions/list` is doomed
    /// to fail, the trailing-refetch behaviour must still fire on
    /// `AgentsSnapshotFailed` — otherwise the user would have to wait
    /// for the next 5s tick after every failure even when state has
    /// already changed since the request went out.
    #[test]
    fn agents_snapshot_failed_fires_dirty_trailing_refetch() {
        let (mut app, mut master_rx) = test_app_with_master_rx();
        app.open_agents_view_for_tab(DEFAULT_TAB_ID.to_string());
        let req_id = match master_rx.try_recv().unwrap() {
            crate::protocol::acp::client::MasterExtRequest::SessionsList { request_id, .. } => {
                request_id
            }
            other => panic!("expected SessionsList, got {other:?}"),
        };
        // While the request is in-flight, more pushes arrive and
        // coalesce into `dirty=true`.
        for _ in 0..5 {
            app.handle_event(AppEvent::SessionsChanged);
        }
        assert!(app.current_tab().agents_view.dirty);
        assert!(
            master_rx.try_recv().is_err(),
            "additional pushes must coalesce while in flight"
        );

        app.handle_event(AppEvent::AgentsSnapshotFailed { request_id: req_id });
        match master_rx
            .try_recv()
            .expect("dirty trailing refetch after failure")
        {
            crate::protocol::acp::client::MasterExtRequest::SessionsList { .. } => {}
            other => panic!("expected SessionsList, got {other:?}"),
        }
        assert!(app.current_tab().agents_view.refetch_in_flight);
        assert!(!app.current_tab().agents_view.dirty);
    }

    /// `AgentsSnapshotFailed` for a stale `request_id` (e.g. arrives
    /// after the tab was closed and reopened) must be a no-op — it
    /// must not clobber a fresh in-flight refetch's
    /// `refetch_in_flight=true` flag.
    #[test]
    fn agents_snapshot_failed_ignores_stale_request_id() {
        let (mut app, mut master_rx) = test_app_with_master_rx();
        app.open_agents_view_for_tab(DEFAULT_TAB_ID.to_string());
        let _stale = match master_rx.try_recv().unwrap() {
            crate::protocol::acp::client::MasterExtRequest::SessionsList { request_id, .. } => {
                request_id
            }
            other => panic!("expected SessionsList, got {other:?}"),
        };
        // Resolve the first request, then kick another so latest_request_id
        // moves on.
        app.handle_event(AppEvent::AgentsSnapshotLoaded {
            request_id: _stale,
            sessions: vec![session_info_for_test("a")],
        });
        app.handle_event(AppEvent::SessionsChanged);
        let _fresh = match master_rx.try_recv().unwrap() {
            crate::protocol::acp::client::MasterExtRequest::SessionsList { request_id, .. } => {
                request_id
            }
            other => panic!("expected SessionsList, got {other:?}"),
        };
        assert!(app.current_tab().agents_view.refetch_in_flight);

        // A stale failure must NOT touch the fresh in-flight state.
        app.handle_event(AppEvent::AgentsSnapshotFailed { request_id: _stale });
        assert!(
            app.current_tab().agents_view.refetch_in_flight,
            "stale failure must not clear the fresh in-flight gate"
        );
    }

    /// The loading-shimmer signal: true only while the agents view is open
    /// and waiting on its first `session/list` reply (empty placeholder
    /// snapshot + in-flight refetch). Replaces the removed on-disk-scan
    /// `HistoryLoadState::Loading` signal.
    #[test]
    fn agents_view_awaiting_snapshot_tracks_first_session_list() {
        let (mut app, _master_rx) = test_app_with_master_rx();
        // Chat view → never awaiting (the shimmer is agents-view only).
        assert!(!app.agents_view_awaiting_snapshot());

        // Opening the agents view primes an empty placeholder snapshot and an
        // in-flight refetch — exactly the loading-shimmer window.
        app.open_agents_view_for_tab(DEFAULT_TAB_ID.to_string());
        assert!(
            app.agents_view_awaiting_snapshot(),
            "awaiting the first session/list snapshot right after open"
        );

        // A non-empty snapshot (master replied with rows) ends the awaiting
        // state even while a follow-up refetch is in flight.
        app.current_tab_mut().agents_view.snapshot = Some(vec![session_info_for_test("a")]);
        assert!(!app.agents_view_awaiting_snapshot());

        // An empty reply with the refetch finished is the genuine empty
        // state, not loading.
        app.current_tab_mut().agents_view.snapshot = Some(Vec::new());
        app.current_tab_mut().agents_view.refetch_in_flight = false;
        assert!(!app.agents_view_awaiting_snapshot());
    }

    #[test]
    fn agents_view_loading_shows_during_f5_rescan() {
        let (mut app, _master_rx) = test_app_with_master_rx();
        app.open_agents_view_for_tab(DEFAULT_TAB_ID.to_string());
        // First snapshot landed: rows present, fetch settled — not loading.
        app.current_tab_mut().agents_view.snapshot = Some(vec![session_info_for_test("a")]);
        app.current_tab_mut().agents_view.refetch_in_flight = false;
        assert!(!app.agents_view_awaiting_snapshot(), "a settled list is not loading");

        // F5 dispatches a rescan: the loading shimmer must show even though the
        // list already has rows, so the refresh is visible.
        app.current_tab_mut().agents_view.pending_rescan = true;
        app.schedule_agents_refetch_for_tab(DEFAULT_TAB_ID);
        assert!(
            app.agents_view_awaiting_snapshot(),
            "F5 rescan must show the loading shimmer even with rows present"
        );

        // The rescan response clears it back to the settled list.
        let rid = app
            .current_tab()
            .agents_view
            .latest_request_id
            .expect("a request was dispatched");
        app.handle_agents_snapshot_loaded(rid, vec![session_info_for_test("a")]);
        assert!(
            !app.agents_view_awaiting_snapshot(),
            "loading clears once the rescan response lands"
        );
    }

    fn session_info_for_test(id: &str) -> crate::session_registry::SessionInfo {
        let mut info = crate::session_registry::SessionInfo::new(
            agent_client_protocol::SessionId::new(id),
            std::path::PathBuf::from(format!("/repo/{id}")),
        );
        info.title = Some(id.to_string());
        info.status = Some(crate::agent_sessions::AgentStatus::Idle);
        info.cli_source = Some(crate::agent_sessions::CliSource::Claude);
        info.last_activity_at_ms = Some(1);
        info
    }

    // ─── agent session view: Enter / Delete dispatch ───────────────────────────
    //
    // Originally added in commit `e4723510e` ("Enter/Delete actions on Agents
    // view (M4.4-M4.6)") and lost in the post-#29 merge that stubbed out
    // dispatch_resume. Re-added on top of the new
    // `spawn_wtcli_split_then_focus_with_callback` helper.

    #[test]
    fn enter_on_live_row_dispatches_focus_command() {
        use crate::agent_sessions::{CliSource, SessionEvent};
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use std::path::PathBuf;
        let mut app = test_app();
        app.agent_sessions.apply(SessionEvent::SessionStarted {
            key: "a".into(),
            cli_source: CliSource::Claude,
            pane_session_id: "00000000-0000-0000-0000-0000000000aa".into(),
            cwd: PathBuf::from("/x"),
            title: "t".into(),
        });
        app.current_tab_mut().current_view = View::Agents;
        app.current_tab_mut().agents_list_state.select(Some(0));

        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let cmd = app
            .last_dispatched_command_for_test()
            .expect("a command was dispatched");
        assert_eq!(cmd.kind, DispatchedCommandKind::FocusPane);
        assert_eq!(cmd.session_id.as_deref(), Some("a"));
    }

    // F5 in the session-management view refetches the session list (footer
    // hint: "F5 to refresh"). When no fetch is in flight it dispatches a
    // fresh sessions/list request to master.
    #[test]
    fn f5_in_session_view_refetches_sessions() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let (mut app, mut master_rx) = test_app_with_master_rx();
        let tab_id = app.active_tab_key().to_string();
        app.open_agents_view_for_tab(tab_id);

        // The open-time refetch must be snapshot-only (no disk rescan).
        match master_rx.try_recv().expect("open requests sessions/list") {
            crate::protocol::acp::client::MasterExtRequest::SessionsList { rescan, .. } => {
                assert!(!rescan, "view-open refetch must not rescan");
            }
            other => panic!("expected SessionsList, got {other:?}"),
        }
        // Clear the in-flight flag so the F5 refetch dispatches fresh.
        app.current_tab_mut().agents_view.refetch_in_flight = false;

        app.handle_key(KeyEvent::new(KeyCode::F(5), KeyModifiers::NONE));

        match master_rx.try_recv().expect("F5 must request sessions/list") {
            crate::protocol::acp::client::MasterExtRequest::SessionsList { rescan, .. } => {
                assert!(rescan, "F5 must request a master-side disk rescan");
            }
            other => panic!("expected SessionsList, got {other:?}"),
        }
    }

    // Esc out of the session-management (Agents) view restores the pane
    // visibility the user had *before* they entered it, rather than always
    // leaving an open chat pane behind. Two cases mirror the two ways the
    // view is reached (see `open_agents_view_for_tab` + the Esc handler).

    #[test]
    fn esc_from_session_view_refolds_when_entered_from_folded_pane() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = test_app();
        let tab_id = app.active_tab_key().to_string();

        // Pane starts folded (stashed): pane_open == false.
        app.tab_mut(&tab_id).pane_open = false;

        // Reproduce the C++ "unstash into sessions" request, which applies
        // `view` before `pane_open`: the view switch snapshots the pre-message
        // `pane_open=false`, then the pane is marked open while sessions show.
        app.open_agents_view_for_tab(tab_id.clone());
        app.tab_mut(&tab_id).pane_open = true;
        assert_eq!(app.current_tab().current_view, View::Agents);

        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        // Re-folds: pane hidden. The view is intentionally left on Agents
        // (not switched to Chat) so the pane stashes straight from the
        // session list without flashing the chat view for a frame first.
        assert!(
            !app.current_tab().pane_open,
            "Esc from a pane that was folded before session management must re-fold it"
        );
        assert_eq!(
            app.current_tab().current_view,
            View::Agents,
            "fold-restore must not switch to chat (would flash before stashing)"
        );
        assert_eq!(
            app.current_tab().agents_view_prev_pane_open, None,
            "the snapshot must be cleared after Esc so a re-entry re-captures"
        );
    }

    #[test]
    fn esc_from_session_view_keeps_pane_open_when_entered_from_chat() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = test_app();
        let tab_id = app.active_tab_key().to_string();

        // Pane is already an expanded chat pane: pane_open == true. The
        // chat->sessions request keeps pane_open=true, so the snapshot is
        // Some(true) and Esc must leave the pane open.
        app.tab_mut(&tab_id).pane_open = true;
        app.open_agents_view_for_tab(tab_id.clone());
        assert_eq!(app.current_tab().current_view, View::Agents);

        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        assert_eq!(app.current_tab().current_view, View::Chat);
        assert!(
            app.current_tab().pane_open,
            "Esc from an expanded chat pane must return to it (stay open)"
        );
    }

    // Checklist C085 "View switch preserves input": a typed-but-unsubmitted chat draft must
    // survive a round-trip through the session (Agents) view. This is the deterministic coverage
    // for the item whose E2E form is not harness-reliable (opening the session view input-free and
    // reading it back races the per-tab pre-warm's extra pane; the slash `/sessions` trigger would
    // itself type into the draft; Esc is overloaded chat-clear vs view-exit). Here we drive the
    // REAL Esc key handler — the exact path where an accidental input-clear on view exit would
    // live — not just the open/close_agents_view helpers.
    #[test]
    fn view_switch_preserves_chat_draft_input() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = test_app();
        let tab_id = app.active_tab_key().to_string();

        // A user is composing a prompt in the chat view (pane open, draft typed, not submitted).
        app.tab_mut(&tab_id).pane_open = true;
        let draft = "unsubmitted draft prompt";
        app.current_tab_mut().input = draft.into();
        app.current_tab_mut().cursor_pos = draft.len();

        // Switch chat -> sessions view (the chat->sessions request keeps pane_open=true).
        app.open_agents_view_for_tab(tab_id.clone());
        assert_eq!(app.current_tab().current_view, View::Agents);
        assert_eq!(
            app.current_tab().input,
            draft,
            "the draft must be untouched while the session view is shown"
        );

        // Esc back to chat (the round-trip return path).
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.current_tab().current_view, View::Chat);

        // The draft AND the cursor position must still be there after the round-trip.
        assert_eq!(
            app.current_tab().input,
            draft,
            "returning to chat after a view switch must preserve the unsubmitted draft"
        );
        assert_eq!(
            app.current_tab().cursor_pos,
            draft.len(),
            "the cursor position in the draft must be preserved across the view round-trip"
        );
    }

    // A pane folded *from within* the sessions view (fold keeps current_view ==
    // Agents) and then reopened must re-snapshot the now-folded state, so a
    // later Esc re-folds instead of using a stale "was open" snapshot.
    #[test]
    fn esc_reuses_latest_snapshot_after_fold_from_session_view() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = test_app();
        let tab_id = app.active_tab_key().to_string();

        // 1. Enter sessions from an open chat pane -> snapshot Some(true).
        app.tab_mut(&tab_id).pane_open = true;
        app.open_agents_view_for_tab(tab_id.clone());

        // 2. Fold while staying in the sessions view (current_view unchanged).
        app.tab_mut(&tab_id).pane_open = false;

        // 3. Reopen sessions (C++ unstash echo) -> must re-snapshot Some(false).
        app.open_agents_view_for_tab(tab_id.clone());
        app.tab_mut(&tab_id).pane_open = true;

        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        assert!(
            !app.current_tab().pane_open,
            "the second entry must capture the folded state, so Esc re-folds"
        );
    }

    #[test]
    fn enter_on_history_row_dispatches_new_tab_with_resume() {
        use crate::agent_sessions::{CliSource, SessionEvent};
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        // Use a real existing directory so cwd_util::validate_starting_directory
        // accepts it. A missing path would (correctly) be dropped from
        // the argv — that behaviour is covered by
        // `enter_on_history_row_with_missing_cwd_omits_d_flag` below.
        let real_cwd = std::env::temp_dir();
        let real_cwd_str = real_cwd.to_string_lossy().to_string();
        let mut app = test_app();
        app.agent_sessions.apply(SessionEvent::SessionStarted {
            key: "abc-123".into(),
            cli_source: CliSource::Claude,
            pane_session_id: "p".into(),
            cwd: real_cwd.clone(),
            title: "t".into(),
        });
        app.agent_sessions.apply(SessionEvent::SessionStopped {
            key: "abc-123".into(),
            reason: "user_exit".into(),
        });

        app.current_tab_mut().current_view = View::Agents;
        app.current_tab_mut().agents_list_state.select(Some(0));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        let cmd = app
            .last_dispatched_command_for_test()
            .expect("a command was dispatched");
        assert_eq!(cmd.kind, DispatchedCommandKind::NewTabResume);
        let argv = cmd.argv.join(" ");
        // The dispatch must use `wtcli new-tab` (not `split-pane`) so the
        // resumed CLI lands in its own WT tab instead of carving up the
        // originating tab.
        assert!(argv.contains("new-tab"), "argv: {}", argv);
        assert!(
            !argv.contains("split-pane"),
            "argv must NOT use split-pane: {}",
            argv
        );
        // The CLI invocation is still wrapped in `cmd /c` so .cmd shims
        // resolve via PATHEXT, but the legacy `cd /d` prefix is gone —
        // cwd is threaded through wtcli's `-d` flag now. Issue #135:
        // a muted "Resuming … session …" banner is prepended so the
        // user sees immediate feedback while the CLI cold-starts; the
        // CLI's alt-screen TUI overwrites it on success. (Previously
        // SGR 1;36;5 — bold + cyan + slow-blink — was used, but the
        // blink + bold were too noisy. Now SGR 2;37 = dim + white, a
        // low-contrast tone similar to the cwd line in a typical
        // Copilot-CLI shell prompt.)
        assert!(
            argv.contains(
                "cmd /c echo \x1b[2;37mResuming claude session abc-123...\x1b[0m"
            ),
            "expected dim-white Resuming banner echo; argv: {:?}",
            argv
        );
        assert!(
            argv.contains("&& claude --resume abc-123"),
            "expected resume command chained after banner; argv: {}",
            argv
        );
        assert!(
            !argv.contains("cd /d"),
            "argv must NOT contain cd /d (cwd is now passed via -d): {}",
            argv
        );
        // Resume is keyed off the session's project cwd — the new tab's
        // primary pane must start in that directory so the CLI's session
        // store lookup (`~/.claude/projects/<encoded-cwd>/...`) succeeds.
        let expected = format!("-d {}", real_cwd_str);
        assert!(
            argv.contains(&expected),
            "expected `{}` in argv: {}",
            expected,
            argv
        );
    }

    /// When the stored cwd no longer exists on disk (e.g. user deleted
    /// the project), `dispatch_resume` must omit `-d <cwd>` entirely so
    /// wtcli falls back to the profile's startingDirectory. Without
    /// this guard, `CreateProcessW` would fail with `ERROR_DIRECTORY`
    /// and produce a visibly-broken pane.
    #[test]
    fn enter_on_history_row_with_missing_cwd_omits_d_flag() {
        use crate::agent_sessions::{CliSource, SessionEvent};
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use std::path::PathBuf;
        let missing = {
            let mut p = std::env::temp_dir();
            p.push(format!(
                "wta-missing-cwd-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ));
            p
        };
        assert!(!missing.exists());
        let mut app = test_app();
        app.agent_sessions.apply(SessionEvent::SessionStarted {
            key: "abc-stale".into(),
            cli_source: CliSource::Claude,
            pane_session_id: "p".into(),
            cwd: PathBuf::from(&missing),
            title: "t".into(),
        });
        app.agent_sessions.apply(SessionEvent::SessionStopped {
            key: "abc-stale".into(),
            reason: "user_exit".into(),
        });
        app.current_tab_mut().current_view = View::Agents;
        app.current_tab_mut().agents_list_state.select(Some(0));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        let cmd = app
            .last_dispatched_command_for_test()
            .expect("a command was dispatched");
        assert_eq!(cmd.kind, DispatchedCommandKind::NewTabResume);
        let argv = cmd.argv.join(" ");
        assert!(argv.contains("new-tab"), "argv: {}", argv);
        // The stale cwd must NOT have leaked through as `-d`.
        assert!(
            !argv.contains("-d "),
            "argv must omit -d when cwd is missing: {}",
            argv
        );
        assert!(
            !argv.contains(&missing.to_string_lossy().to_string()),
            "argv must not embed the stale cwd: {}",
            argv
        );
    }

    #[test]
    fn shift_enter_on_history_row_dispatches_resume_in_agent_pane() {
        // Shift+Enter on a terminal-state row should route to the
        // ResumeInAgentPane path, NOT the legacy NewTabResume — it
        // emits `resume_in_new_agent_tab` to WT instead of spawning a
        // normal terminal tab locally. The dispatched-command tape
        // captures the shape so downstream wiring can be
        // regression-checked.
        use crate::agent_sessions::{CliSource, SessionEvent};
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        // Use a real existing directory so cwd_util::validate_starting_directory
        // accepts it. A missing cwd would (correctly) be omitted —
        // covered by `shift_enter_on_history_row_with_missing_cwd_omits_cwd`.
        let real_cwd = std::env::temp_dir();
        let real_cwd_str = real_cwd.to_string_lossy().to_string();
        let mut app = test_app();
        // Capability gate: dispatch is only attempted when the agent
        // advertised loadSession. Without this, the handler
        // short-circuits with a system message instead.
        app.agent_supports_load_session = true;
        app.agent_sessions.apply(SessionEvent::SessionStarted {
            key: "abc-123".into(),
            cli_source: CliSource::Claude,
            pane_session_id: "p".into(),
            cwd: real_cwd.clone(),
            title: "t".into(),
        });
        app.agent_sessions.apply(SessionEvent::SessionStopped {
            key: "abc-123".into(),
            reason: "user_exit".into(),
        });

        app.current_tab_mut().current_view = View::Agents;
        app.current_tab_mut().agents_list_state.select(Some(0));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));

        let cmd = app
            .last_dispatched_command_for_test()
            .expect("a command was dispatched");
        assert_eq!(cmd.kind, DispatchedCommandKind::ResumeInAgentPane);
        assert_eq!(cmd.session_id.as_deref(), Some("abc-123"));
        let argv = cmd.argv.join(" ");
        assert!(argv.contains("resume_in_new_agent_tab"), "argv: {}", argv);
        assert!(argv.contains("--session-id abc-123"), "argv: {}", argv);
        let expected = format!("--cwd {}", real_cwd_str);
        assert!(
            argv.contains(&expected),
            "expected `{}` in argv: {}",
            expected,
            argv
        );
    }

    /// Shift+Enter mirror of `enter_on_history_row_with_missing_cwd_omits_d_flag`:
    /// when the stored cwd no longer exists, the resume-in-agent-pane
    /// path must omit the `cwd` field from the emitted
    /// `resume_in_new_agent_tab` event so WT's `_OpenNewTab` falls back
    /// to the profile's startingDirectory (otherwise the new tab opens
    /// with a broken connection).
    #[test]
    fn shift_enter_on_history_row_with_missing_cwd_omits_cwd() {
        use crate::agent_sessions::{CliSource, SessionEvent};
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use std::path::PathBuf;
        let missing = {
            let mut p = std::env::temp_dir();
            p.push(format!(
                "wta-missing-shift-cwd-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ));
            p
        };
        assert!(!missing.exists());
        let mut app = test_app();
        app.agent_supports_load_session = true;
        app.agent_sessions.apply(SessionEvent::SessionStarted {
            key: "abc-stale".into(),
            cli_source: CliSource::Claude,
            pane_session_id: "p".into(),
            cwd: PathBuf::from(&missing),
            title: "t".into(),
        });
        app.agent_sessions.apply(SessionEvent::SessionStopped {
            key: "abc-stale".into(),
            reason: "user_exit".into(),
        });
        app.current_tab_mut().current_view = View::Agents;
        app.current_tab_mut().agents_list_state.select(Some(0));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));

        let cmd = app
            .last_dispatched_command_for_test()
            .expect("a command was dispatched");
        assert_eq!(cmd.kind, DispatchedCommandKind::ResumeInAgentPane);
        let argv = cmd.argv.join(" ");
        assert!(argv.contains("resume_in_new_agent_tab"), "argv: {}", argv);
        // Fallback contract: the --cwd flag (and any value) must be
        // omitted entirely so the consumer uses its default. A
        // regression that sent `--cwd ""` would slip past a
        // string-contains check, hence the explicit flag assertion.
        assert!(
            !cmd.argv.iter().any(|a| a == "--cwd"),
            "argv must omit --cwd when cwd is missing: {:?}",
            cmd.argv
        );
        assert!(
            !argv.contains(&missing.to_string_lossy().to_string()),
            "argv must not embed the stale cwd: {}",
            argv
        );
    }

    #[test]
    fn shift_enter_history_row_without_load_session_capability_shows_hint() {
        // Capability gate: when the agent doesn't advertise loadSession,
        // Shift+Enter must not open a new tab. Instead it pushes a
        // system message in the session management view explaining the
        // fallback (plain Enter). The dispatched-command tape captures
        // the gated path so the regression is observable.
        use crate::agent_sessions::{CliSource, SessionEvent};
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use std::path::PathBuf;
        let mut app = test_app();
        // No `agent_supports_load_session = true` — default is false.
        app.agent_sessions.apply(SessionEvent::SessionStarted {
            key: "abc-123".into(),
            cli_source: CliSource::Claude,
            pane_session_id: "p".into(),
            cwd: PathBuf::from("/work/proj"),
            title: "t".into(),
        });
        app.agent_sessions.apply(SessionEvent::SessionStopped {
            key: "abc-123".into(),
            reason: "user_exit".into(),
        });

        app.current_tab_mut().current_view = View::Agents;
        app.current_tab_mut().agents_list_state.select(Some(0));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));

        let cmd = app
            .last_dispatched_command_for_test()
            .expect("a command was dispatched");
        // B-10: `decide_enter_action` short-circuits to NotResumable
        // before any side-effect dispatch when the agent doesn't
        // advertise loadSession. Previously this routed all the way
        // through `dispatch_resume_in_agent_pane`'s internal gate;
        // now the gate is hoisted into the pure state machine so
        // there's one canonical path. The system hint message is
        // unchanged.
        assert_eq!(cmd.kind, DispatchedCommandKind::NotResumable);
        let argv = cmd.argv.join(" ");
        assert!(argv.contains("LoadSessionNotSupported"), "argv: {}", argv);
        // The current tab gets a System hint message.
        let has_hint = app.current_tab().messages.iter().any(|m| {
            matches!(m, ChatMessage::System(text)
                if text.contains("loadSession")
                    && text.contains("Press Enter"))
        });
        assert!(has_hint, "expected system hint message in the current tab");
    }

    #[test]
    fn shift_enter_on_live_row_falls_back_to_focus() {
        // Live rows have no historical state to "load" — Shift+Enter on
        // them must NOT trigger the resume-in-agent-pane flow. It falls
        // through to the same FocusPane dispatch as plain Enter.
        use crate::agent_sessions::{CliSource, SessionEvent};
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use std::path::PathBuf;
        let mut app = test_app();
        app.agent_sessions.apply(SessionEvent::SessionStarted {
            key: "a".into(),
            cli_source: CliSource::Claude,
            pane_session_id: "00000000-0000-0000-0000-0000000000aa".into(),
            cwd: PathBuf::from("/x"),
            title: "t".into(),
        });
        app.current_tab_mut().current_view = View::Agents;
        app.current_tab_mut().agents_list_state.select(Some(0));

        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
        let cmd = app
            .last_dispatched_command_for_test()
            .expect("a command was dispatched");
        assert_eq!(cmd.kind, DispatchedCommandKind::FocusPane);
    }

    // -------- B-10: state-machine-driven Enter / Shift+Enter dispatch --------
    //
    // Pure routing rules are exhaustively tested in
    // `session_mgmt::tests`. Here we verify the *integration* — that the
    // key-handler path actually constructs a RowSnapshot from the
    // selected AgentSession, hands it to `decide_enter_action`, and
    // dispatches each EnterAction variant through the correct side
    // effect (or NotResumable hint). One or two representative cases
    // per variant is enough; B-1 holds the truth table.

    /// Class A (AgentPane origin) dead row + plain Enter:
    /// new state machine routes to ResumeInAgentPane (ACP load).
    /// This is the headline behavior change from B-10 — previously
    /// Class A dead + Enter ran the CLI --resume flag path.
    #[test]
    fn enter_on_class_a_dead_row_dispatches_resume_in_agent_pane() {
        use crate::agent_sessions::{CliSource, OriginFilter, SessionEvent, SessionOrigin};
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use std::path::PathBuf;
        let mut app = test_app();
        // This test exercises the Class A (AgentPane) Enter routing,
        // which the MVP sessions filter hides. Opt out so the row is
        // visible to the cursor; the dispatch logic under test is
        // unchanged by the filter.
        app.sessions_origin_filter = OriginFilter::All;
        app.agent_supports_load_session = true;
        app.agent_sessions.apply(SessionEvent::SessionStarted {
            key: "abc-class-a".into(),
            cli_source: CliSource::Claude,
            pane_session_id: "p".into(),
            cwd: PathBuf::from("/work/cls-a"),
            title: "t".into(),
        });
        app.agent_sessions.apply(SessionEvent::SessionStopped {
            key: "abc-class-a".into(),
            reason: "user_exit".into(),
        });
        app.agent_sessions
            .set_origin("abc-class-a", SessionOrigin::AgentPane);

        app.current_tab_mut().current_view = View::Agents;
        app.current_tab_mut().agents_list_state.select(Some(0));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        let cmd = app
            .last_dispatched_command_for_test()
            .expect("a command was dispatched");
        assert_eq!(cmd.kind, DispatchedCommandKind::ResumeInAgentPane);
        let argv = cmd.argv.join(" ");
        assert!(argv.contains("resume_in_new_agent_tab"), "argv: {}", argv);
        assert!(argv.contains("--session-id abc-class-a"), "argv: {}", argv);
    }

    /// Class A (AgentPane origin) dead row + Shift+Enter:
    /// Shift flips the default → ResumeCliFlag (new tab CLI --resume).
    #[test]
    fn shift_enter_on_class_a_dead_row_dispatches_cli_resume() {
        use crate::agent_sessions::{CliSource, OriginFilter, SessionEvent, SessionOrigin};
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use std::path::PathBuf;
        let mut app = test_app();
        // See enter_on_class_a_dead_row_dispatches_resume_in_agent_pane
        // for the OriginFilter::All rationale — the MVP filter hides
        // Class A rows from the cursor model; this test exercises the
        // routing logic that fires when they ARE visible.
        app.sessions_origin_filter = OriginFilter::All;
        app.agent_supports_load_session = true;
        app.agent_sessions.apply(SessionEvent::SessionStarted {
            key: "abc-class-a-shift".into(),
            cli_source: CliSource::Claude,
            pane_session_id: "p".into(),
            cwd: PathBuf::from("/work/cls-a"),
            title: "t".into(),
        });
        app.agent_sessions.apply(SessionEvent::SessionStopped {
            key: "abc-class-a-shift".into(),
            reason: "user_exit".into(),
        });
        app.agent_sessions
            .set_origin("abc-class-a-shift", SessionOrigin::AgentPane);

        app.current_tab_mut().current_view = View::Agents;
        app.current_tab_mut().agents_list_state.select(Some(0));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));

        // What matters here is that Shift+Enter on
        // Class A dead routed through dispatch_resume (the CLI flag
        // path), NOT dispatch_resume_in_agent_pane.
        let cmd = app
            .last_dispatched_command_for_test()
            .expect("a command was dispatched");
        assert_eq!(cmd.kind, DispatchedCommandKind::NewTabResume);
    }

    /// Live row + Shift+Enter: identical to Enter (Shift is a no-op on
    /// live rows because agents forbid two clients on one session).
    /// This is implicitly the case for `shift_enter_on_live_row_falls_
    /// back_to_focus` above; here we additionally assert with a Class
    /// A origin to confirm origin doesn't matter for Live rows.
    #[test]
    fn shift_enter_on_class_a_live_row_focuses() {
        use crate::agent_sessions::{CliSource, OriginFilter, SessionEvent, SessionOrigin};
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use std::path::PathBuf;
        let mut app = test_app();
        // Same rationale as the Class A dead-row tests above:
        // MVP sessions filter hides AgentPane rows, this test verifies the
        // dispatch logic for when they are visible.
        app.sessions_origin_filter = OriginFilter::All;
        app.agent_sessions.apply(SessionEvent::SessionStarted {
            key: "live-class-a".into(),
            cli_source: CliSource::Claude,
            pane_session_id: "00000000-0000-0000-0000-0000000000bb".into(),
            cwd: PathBuf::from("/x"),
            title: "t".into(),
        });
        app.agent_sessions
            .set_origin("live-class-a", SessionOrigin::AgentPane);

        app.current_tab_mut().current_view = View::Agents;
        app.current_tab_mut().agents_list_state.select(Some(0));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));

        let cmd = app
            .last_dispatched_command_for_test()
            .expect("a command was dispatched");
        assert_eq!(cmd.kind, DispatchedCommandKind::FocusPane);
        assert_eq!(cmd.session_id.as_deref(), Some("live-class-a"));
    }

    /// Class B (Unknown origin) + plain Enter on a Live row preserves
    /// the legacy focus behavior — this exercises the most common
    /// session management path (user-started `copilot` in a normal pane via hooks).
    #[test]
    fn enter_on_class_b_live_row_focuses() {
        use crate::agent_sessions::{CliSource, SessionEvent};
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use std::path::PathBuf;
        let mut app = test_app();
        // SessionStarted defaults origin to Unknown (Class B).
        app.agent_sessions.apply(SessionEvent::SessionStarted {
            key: "live-class-b".into(),
            cli_source: CliSource::Copilot,
            pane_session_id: "00000000-0000-0000-0000-0000000000cc".into(),
            cwd: PathBuf::from("/x"),
            title: "t".into(),
        });

        app.current_tab_mut().current_view = View::Agents;
        app.current_tab_mut().agents_list_state.select(Some(0));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        let cmd = app
            .last_dispatched_command_for_test()
            .expect("a command was dispatched");
        assert_eq!(cmd.kind, DispatchedCommandKind::FocusPane);
    }

    #[test]
    fn delete_on_history_row_removes_session_from_registry() {
        use crate::agent_sessions::{CliSource, SessionEvent};
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use std::path::PathBuf;
        let mut app = test_app();
        app.agent_sessions.apply(SessionEvent::SessionStarted {
            key: "k".into(),
            cli_source: CliSource::Claude,
            pane_session_id: "p".into(),
            cwd: PathBuf::from("/x"),
            title: "t".into(),
        });
        app.agent_sessions.apply(SessionEvent::SessionStopped {
            key: "k".into(),
            reason: "".into(),
        });
        app.current_tab_mut().current_view = View::Agents;
        app.current_tab_mut().agents_list_state.select(Some(0));

        app.handle_key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
        assert!(!app.agent_sessions.has_session(&"k".to_string()));
    }

    #[test]
    fn delete_on_live_row_is_noop() {
        use crate::agent_sessions::{CliSource, SessionEvent};
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use std::path::PathBuf;
        let mut app = test_app();
        app.agent_sessions.apply(SessionEvent::SessionStarted {
            key: "k".into(),
            cli_source: CliSource::Claude,
            pane_session_id: "p".into(),
            cwd: PathBuf::from("/x"),
            title: "t".into(),
        });
        app.current_tab_mut().current_view = View::Agents;
        app.current_tab_mut().agents_list_state.select(Some(0));

        app.handle_key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
        assert!(app.agent_sessions.has_session(&"k".to_string()));
    }

    // ─── Phantom-session prune ───────────────────────────────────────

    #[test]
    fn agents_view_state_is_isolated_per_tab() {
        // Regression: opening the Agents picker in tab A should not show
        // up as opened (or with the same selection) when the user switches
        // to tab B. `current_view` and `agents_list_state` live on
        // TabSession exactly to keep these states independent.
        use crate::agent_sessions::{CliSource, SessionEvent};
        use std::path::PathBuf;
        let mut app = test_app();
        for k in ["a", "b", "c"] {
            app.agent_sessions.apply(SessionEvent::SessionStarted {
                key: k.into(),
                cli_source: CliSource::Claude,
                pane_session_id: format!("p-{}", k),
                cwd: PathBuf::from("/x"),
                title: format!("t-{}", k),
            });
        }

        // Tab "0" (the seeded default): open picker, select row 2.
        app.tab_id = Some("0".into());
        app.current_tab_mut().current_view = View::Agents;
        app.current_tab_mut().agents_list_state.select(Some(2));

        // Switch to tab "1" — its TabSession is lazily created with
        // defaults: View::Chat and no selection.
        app.tab_id = Some("1".into());
        let tab1 = app.current_tab_mut();
        assert_eq!(tab1.current_view, View::Chat, "new tab must start in Chat");
        assert_eq!(tab1.agents_list_state.selected(), None);

        // Mutating tab 1 must not bleed back into tab 0.
        tab1.current_view = View::Agents;
        tab1.agents_list_state.select(Some(0));

        app.tab_id = Some("0".into());
        let tab0 = app.current_tab();
        assert_eq!(tab0.current_view, View::Agents);
        assert_eq!(tab0.agents_list_state.selected(), Some(2));
    }

    #[test]
    fn closing_other_tab_preserves_per_tab_view_when_tab_changed_follows() {
        // Reproduces the user-reported bug:
        //   tab1 has the session list (agent session view) open. User opens
        //   tab2, then closes tab2. Focus returns to tab1, the agent
        //   pane is still visible, but the session list has vanished
        //   — the user has to press the shortcut again to bring it
        //   back.
        //
        // Root cause was on the C++ side: `_OnTabSelectionChanged`
        // is suppressed during tab removal, so the
        // `_NotifyAgentTabChanged(tab1)` that normally follows the
        // auto-selection of the previous tab never fired. wta's
        // `tab_id` got nulled by `tab_closed` and never restored, so
        // `current_tab()` silently fell back to the empty
        // `DEFAULT_TAB_ID` slot. After the C++ fix
        // (explicit `_ReconcileAgentPaneForActiveTab` post-removal),
        // wta receives the missing `tab_changed { tab_id: tab1 }`
        // event and `current_tab()` resolves back to tab1's
        // preserved TabSession with `View::Agents` intact.
        //
        // This test simulates the full wta-side event sequence:
        //   1. tab1 active, picker open with selection at row 2.
        //   2. user clicks tab2 → tab_changed { tab_id: tab2 }.
        //   3. user closes tab2 → tab_closed { tab_id: tab2 }.
        //   4. C++ fires the post-removal reconcile →
        //      tab_changed { tab_id: tab1 }.
        // After (4), `current_tab()` must return tab1's TabSession
        // with View::Agents and the row-2 selection preserved.
        use crate::agent_sessions::{CliSource, SessionEvent};
        use std::path::PathBuf;
        let mut app = test_app();
        for k in ["a", "b", "c"] {
            app.agent_sessions.apply(SessionEvent::SessionStarted {
                key: k.into(),
                cli_source: CliSource::Claude,
                pane_session_id: format!("p-{}", k),
                cwd: PathBuf::from("/x"),
                title: format!("t-{}", k),
            });
        }

        // (1) tab1 active, agent session view, selection at row 2.
        let tab1 = "tab1-stable-id";
        let tab2 = "tab2-stable-id";
        app.tab_id = Some(tab1.into());
        app.current_tab_mut().current_view = View::Agents;
        app.current_tab_mut().agents_list_state.select(Some(2));

        // (2) User clicks tab2: switch_tab_session simulates the
        // arrival of `tab_changed { tab_id: tab2 }`.
        app.switch_tab_session(tab2.into());
        // tab2 starts at defaults; tab1 entry is untouched in the map.
        assert_eq!(app.current_tab().current_view, View::Chat);

        // (3) User closes tab2: drop_tab_session simulates
        // `tab_closed { tab_id: tab2 }`. tab2's entry is removed and
        // tab_id is nulled (DEFAULT_TAB_ID slot lazily created).
        app.drop_tab_session(tab2);
        assert!(
            app.tab_id.is_none(),
            "drop of active tab must null tab_id pending the next tab_changed"
        );

        // Critical: BEFORE the C++ fix, this is where wta is left
        // stranded — no further `tab_changed` ever arrives. The user
        // sees the agent pane stuck on DEFAULT_TAB_ID's empty Chat
        // view even though tab1's state is still in the map.
        // Demonstrate the bug shape:
        assert_eq!(
            app.current_tab().current_view,
            View::Chat,
            "without the follow-up tab_changed, current_tab falls back to DEFAULT_TAB_ID"
        );

        // (4) The C++ fix: post-removal reconcile fires
        // `_NotifyAgentTabChanged(tab1)` which lands here as
        // `switch_tab_session(tab1)`.
        app.switch_tab_session(tab1.into());

        // Now current_tab resolves back to tab1's preserved state.
        assert_eq!(
            app.current_tab().current_view,
            View::Agents,
            "tab1's View::Agents must be preserved across tab2's open/close"
        );
        assert_eq!(
            app.current_tab().agents_list_state.selected(),
            Some(2),
            "tab1's list selection must be preserved"
        );
    }

    #[test]
    fn autofix_still_triggers_for_non_agent_pane() {
        let mut app = test_app();
        app.state = ConnectionState::Connected;
        app.autofix_enabled = true;
        // No SessionStarted apply -> pane is not an agent pane.
        let pane = "non-agent-pane-guid";

        let notification = WtNotification {
            severity: WtEventSeverity::Actionable,
            pane_id: pane.to_string(),
            tab_id: Some("test-tab".to_string()),
            summary: "Command failed (exit 1)".to_string(),
            acknowledged: false,
            age_ticks: 0,
        };
        app.maybe_trigger_autofix(&notification);

        assert_eq!(
            app.tab_mut("test-tab").autofix.pane_id.as_deref(),
            Some(pane),
            "autofix must still arm normal panes when a command fails"
        );
        // The target tab's turn (not the active tab's) should be in-flight.
        assert!(
            !app.tab_mut("test-tab").turn.is_idle(),
            "autofix prompt should be in-flight on the target tab"
        );
    }

    /// F3: a transport death (helper `handle_io` watchdog) moves the UI out of
    /// `Connected`, and its connection.lost ("/restart") line must survive even
    /// when a different error (e.g. the in-flight prompt failure, "returned as
    /// is") is already shown — only identical consecutive errors collapse, so
    /// the recovery hint is never hidden.
    #[test]
    fn transport_loss_surfaces_restart_hint_even_behind_another_error() {
        let lost = t!("connection.lost").into_owned();
        let mut app = test_app();
        app.state = ConnectionState::Connected;
        // In-flight prompt fails first (raw), then the watchdog's connection.lost.
        app.handle_event(AppEvent::AgentError {
            session_id: None,
            failure: crate::protocol::acp::failure::AgentFailure::Protocol {
                code: -32603,
                message: "pipe closed".to_string(),
            },
            message: "prompt error: pipe closed".to_string(),
        });
        app.handle_event(AppEvent::AgentError {
            session_id: None,
            failure: crate::protocol::acp::failure::AgentFailure::TransportLost,
            message: lost.clone(),
        });
        assert!(
            matches!(app.state, ConnectionState::Failed(_)),
            "a transport loss must move the UI out of Connected (F3)"
        );
        assert!(
            app.current_tab()
                .messages
                .iter()
                .any(|m| matches!(m, ChatMessage::Error(s) if *s == lost)),
            "the connection.lost /restart hint must be shown, not hidden behind the raw error"
        );
        // An identical connection.lost arriving again must not stack a duplicate.
        app.handle_event(AppEvent::AgentError {
            session_id: None,
            failure: crate::protocol::acp::failure::AgentFailure::TransportLost,
            message: lost.clone(),
        });
        let n = app
            .current_tab()
            .messages
            .iter()
            .filter(|m| matches!(m, ChatMessage::Error(s) if *s == lost))
            .count();
        assert_eq!(n, 1, "identical connection.lost must not duplicate");
    }

    /// `is_post_login_auth_failure` must catch BOTH the plain `AuthRequired`
    /// and the `HandshakeFailed { NewSession }` the pipe client wraps a
    /// still-AuthRequired post-login `new_session` into — `is_auth()` alone
    /// would miss the latter and the auth recovery would never fire. It must
    /// NOT match `HandshakeFailed { Authenticate }` (a genuine authenticate
    /// RPC rejection/timeout) — that routes to sign-in, not a master restart.
    #[test]
    fn post_login_auth_failure_matches_auth_required_and_handshake_new_session() {
        use crate::protocol::acp::failure::{AgentFailure, HandshakeStage};
        assert!(is_post_login_auth_failure(&AgentFailure::AuthRequired {
            message: "auth".to_string()
        }));
        assert!(is_post_login_auth_failure(&AgentFailure::HandshakeFailed {
            stage: HandshakeStage::NewSession,
            detail: "still auth after authenticate".to_string()
        }));
        // An authenticate-RPC rejection/timeout must NOT trigger auth recovery
        // (a master restart can't fix bad credentials) — it routes to sign-in.
        assert!(!is_post_login_auth_failure(&AgentFailure::HandshakeFailed {
            stage: HandshakeStage::Authenticate,
            detail: "authenticate rejected/timed out".to_string()
        }));
        // A non-auth handshake stage must NOT trigger auth recovery.
        assert!(!is_post_login_auth_failure(&AgentFailure::HandshakeFailed {
            stage: HandshakeStage::Initialize,
            detail: "boom".to_string()
        }));
    }

    /// `PostLoginAuthRecovery` shows a transient "Reconnecting…" (NOT the
    /// sign-in screen, so there is no flash), and the `AuthRecoveryTimedOut`
    /// dead-man only falls back to the sign-in screen if the restart never
    /// took effect (this helper survived the window).
    #[test]
    fn post_login_auth_recovery_shows_reconnecting_then_signin_fallback() {
        let mut app = test_app();
        app.handle_event(AppEvent::PostLoginAuthRecovery {
            failure: crate::protocol::acp::failure::AgentFailure::AuthRequired {
                message: "auth".to_string(),
            },
            tab_id: None,
            agent_id: "copilot".to_string(),
        });
        // Common case: transient Reconnecting, NOT the setup screen (no flash).
        assert!(
            !matches!(app.mode, AppMode::Setup),
            "recovery must NOT flash the sign-in screen"
        );
        assert!(
            matches!(app.state, ConnectionState::Connecting(_)),
            "recovery must show a transient Reconnecting state"
        );
        let generation = app.auth_recovery_generation;
        // A STALE timer (older generation) must be ignored — it must not force
        // the sign-in screen onto the current Connecting state.
        app.handle_event(AppEvent::AuthRecoveryTimedOut {
            agent_id: "copilot".to_string(),
            generation: generation.wrapping_sub(1),
        });
        assert!(
            !matches!(app.mode, AppMode::Setup),
            "a stale-generation timeout must be ignored"
        );
        // Dead-man fallback (restart never took effect) → sign-in screen.
        app.handle_event(AppEvent::AuthRecoveryTimedOut {
            agent_id: "copilot".to_string(),
            generation,
        });
        assert!(
            matches!(app.mode, AppMode::Setup),
            "timeout fallback must surface the sign-in screen"
        );
    }

    /// The degraded latch (`App::transport_lost`) drives the slash-command
    /// greying. It must arm on a transport loss and stay armed (the helper has
    /// no in-process reconnect), so the popup keeps refusing everything but
    /// /restart until recovery.
    #[test]
    fn transport_lost_latch_arms_on_transport_loss() {
        let mut app = test_app();
        app.state = ConnectionState::Connected;
        assert!(!app.transport_lost, "fresh app is not degraded");

        app.handle_event(AppEvent::AgentError {
            session_id: None,
            failure: crate::protocol::acp::failure::AgentFailure::TransportLost,
            message: t!("connection.lost").into_owned(),
        });

        assert!(
            app.transport_lost,
            "a transport loss must arm the degraded latch"
        );
    }

    /// A non-transport failure (a one-off protocol error) must NOT arm the
    /// latch — the session is still alive, so commands stay enabled.
    #[test]
    fn protocol_error_does_not_arm_degraded_latch() {
        let mut app = test_app();
        app.state = ConnectionState::Connected;

        app.handle_event(AppEvent::AgentError {
            session_id: None,
            failure: crate::protocol::acp::failure::AgentFailure::Protocol {
                code: -32603,
                message: "bad params".to_string(),
            },
            message: "protocol error".to_string(),
        });

        assert!(
            !app.transport_lost,
            "a non-transport protocol error must not degrade the pane"
        );
    }

    /// An auth failure routes to sign-in, not the dead-transport path, so it
    /// must not arm the degraded latch (otherwise the post-sign-in pane would
    /// wrongly grey out its commands).
    #[test]
    fn auth_failure_does_not_arm_degraded_latch() {
        let mut app = test_app();
        app.state = ConnectionState::Connected;

        app.handle_event(AppEvent::AgentError {
            session_id: None,
            failure: crate::protocol::acp::failure::AgentFailure::AuthRequired {
                message: "authentication required".to_string(),
            },
            message: "authentication required".to_string(),
        });

        assert!(
            !app.transport_lost,
            "an auth failure must not arm the degraded latch"
        );
    }

    /// A fresh connection (e.g. the post-sign-in reconnect that goes back
    /// through master) must clear the latch so commands re-enable.
    #[test]
    fn agent_connected_clears_degraded_latch() {
        let mut app = test_app();
        app.transport_lost = true;

        app.handle_event(AppEvent::AgentConnected {
            name: "Copilot".to_string(),
            model: None,
            version: None,
            session_id: "sid-fresh".to_string(),
            available_models: Vec::new(),
            current_model_id: None,
            load_session_supported: true,
            image_supported: false,
        });

        assert!(
            !app.transport_lost,
            "reaching Connected must clear the degraded latch"
        );
    }

    /// Auth failures must reach the sign-in screen, not get flattened to a dead
    /// `connection.lost`. Classification is typed (`AgentFailure::AuthRequired`),
    /// done once at the helper boundary, so the handler routes purely on the
    /// discriminant — no substring matching of the message text.
    #[test]
    fn auth_error_routes_to_signin_not_connection_lost() {
        let mut app = test_app();
        app.state = ConnectionState::Connected;
        app.handle_event(AppEvent::AgentError {
            session_id: None,
            failure: crate::protocol::acp::failure::AgentFailure::AuthRequired {
                message: "authentication required".to_string(),
            },
            message: "new_session over master pipe failed: authentication required"
                .to_string(),
        });
        assert_eq!(
            app.mode,
            AppMode::Setup,
            "an auth failure must route to the sign-in screen"
        );
        assert!(
            !matches!(app.state, ConnectionState::Failed(_)),
            "an auth failure must not become a Failed connection-lost state"
        );
    }

    /// A soft stop is an *outcome*, not a connection failure: the handler must
    /// append an informational System line carrying the localized reason text,
    /// while leaving the connection `Connected` and never routing to the
    /// sign-in screen. This is what keeps soft stops off the `AgentFailure`
    /// axis — the gap the client-level emit test cannot cover.
    #[test]
    fn soft_stop_appends_system_line_without_changing_state() {
        use crate::protocol::acp::soft_stop::SoftStopReason;
        let mut app = test_app();
        app.state = ConnectionState::Connected;

        app.handle_event(AppEvent::AgentSoftStop {
            session_id: "0".to_string(),
            reason: SoftStopReason::Refusal,
        });

        let expected = t!("system.stopped_refusal").into_owned();
        assert!(
            app.current_tab()
                .messages
                .iter()
                .any(|m| matches!(m, ChatMessage::System(s) if *s == expected)),
            "a soft stop must append its localized System line"
        );
        assert!(
            matches!(app.state, ConnectionState::Connected),
            "a soft stop must not change the connection state"
        );
        assert_ne!(
            app.mode,
            AppMode::Setup,
            "a soft stop is not a failure — it must never route to sign-in"
        );
        assert!(
            !app.current_tab()
                .messages
                .iter()
                .any(|m| matches!(m, ChatMessage::Error(_))),
            "a soft stop must not surface an Error line"
        );
    }

    /// Each `SoftStopReason` must resolve to its own distinct localized line so
    /// the user can tell truncation from a request-budget stop from a refusal.
    #[test]
    fn soft_stop_reasons_map_to_distinct_localized_lines() {
        use crate::protocol::acp::soft_stop::SoftStopReason;
        for (reason, key) in [
            (SoftStopReason::MaxTokens, "system.stopped_max_tokens"),
            (
                SoftStopReason::MaxTurnRequests,
                "system.stopped_max_turn_requests",
            ),
            (SoftStopReason::Refusal, "system.stopped_refusal"),
        ] {
            let mut app = test_app();
            app.handle_event(AppEvent::AgentSoftStop {
                session_id: "0".to_string(),
                reason,
            });
            let expected = t!(key).into_owned();
            assert!(
                app.current_tab()
                    .messages
                    .iter()
                    .any(|m| matches!(m, ChatMessage::System(s) if *s == expected)),
                "reason {reason:?} must render the {key} line"
            );
        }
    }

    /// F7: while `Connecting`, the activity frame must keep advancing on Tick so
    /// the indicator animates and a cold start doesn't look frozen.
    #[test]
    fn connecting_state_advances_activity_frame_on_tick() {
        let mut app = test_app();
        app.state = ConnectionState::Connecting("Initializing ACP...".to_string());
        let before = app.activity_frame;
        app.handle_event(AppEvent::Tick);
        assert_ne!(
            app.activity_frame, before,
            "the connecting indicator must keep animating (F7)"
        );
    }

    /// `connection_state: closed/failed` is pane-process termination, not
    /// a shell command failure — it carries no exit code, no command
    /// context, and the pane is gone so any follow-up ReadPaneOutput
    /// would trip E_FAIL. The dispatcher in `handle_event` only routes
    /// `vt_sequence` events to autofix; this asserts the connection_state
    /// path stays banner-only.
    #[test]
    fn connection_state_closed_does_not_trigger_autofix_even_when_binding_cleared() {
        use crate::agent_sessions::{CliSource, SessionEvent};
        use std::path::PathBuf;
        let mut app = test_app();
        app.state = ConnectionState::Connected;
        app.autofix_enabled = true;
        let pane = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

        // Bind, then unbind — mirrors the Copilot order: agent.session.end
        // hook arrives and runs SessionStopped before WT emits closed.
        // The session is NOT tagged with `SessionOrigin::AgentPane` (this
        // test sets up state via raw SessionStarted, so origin defaults
        // to Unknown), which means SessionStopped immediately transitions
        // to Ended and releases the pane binding — exactly the precondition
        // this test depends on.
        app.agent_sessions.apply(SessionEvent::SessionStarted {
            key: "copilot-key".into(),
            cli_source: CliSource::Copilot,
            pane_session_id: pane.into(),
            cwd: PathBuf::from("/work"),
            title: "t".into(),
        });
        app.agent_sessions.apply(SessionEvent::SessionStopped {
            key: "copilot-key".into(),
            reason: "user_exit".into(),
        });
        // Sanity: binding is gone, so the inner is_agent_pane guard alone
        // would not catch this.
        assert!(!app.agent_sessions.is_agent_pane(pane));

        app.handle_event(AppEvent::WtEvent {
            method: "connection_state".to_string(),
            pane_id: pane.to_string(),
            tab_id: None,
            params: serde_json::json!({"session_id": pane, "state": "closed"}),
        });

        assert!(
            app.tab_sessions
                .values()
                .all(|t| t.autofix.pane_id.is_none()),
            "connection_state:closed must never arm autofix — no exit code, \
             no command context, pane is dead so subsequent ReadPaneOutput \
             would throw E_FAIL"
        );
        assert!(
            app.current_tab().turn.is_idle(),
            "no autofix prompt should be in-flight"
        );
        // The pane-closed event surfaces via the banner / `wt_notifications`,
        // never in chat. Chat is the agent dialogue surface.
        assert!(
            app.current_tab().messages.is_empty(),
            "WT events must not push into chat history"
        );
        assert!(app.show_notification_banner);
    }

    /// Regression: a stale agent-CLI binding in the registry must NOT eat a
    /// real shell command failure. OSC 133;D is emitted by shell integration
    /// (PowerShell/bash), never by an agent CLI, so a D arriving in an
    /// "agent-bound" pane implies the binding is a ghost — typically left
    /// over from a hook that misreported `pane_id`, or from the previous
    /// agent CLI having exited without the registry catching it yet.
    /// Real-world repro: autofix runs Copilot, Copilot's hooks emit events
    /// with `pane_id` = the source (user's) pane, registry registers the
    /// user's PowerShell pane as Copilot-bound, then the next typo there
    /// silently dies in the suppression check.
    #[test]
    fn ghost_agent_binding_does_not_suppress_shell_failure() {
        use crate::agent_sessions::{CliSource, SessionEvent};
        use std::path::PathBuf;
        let mut app = test_app();
        app.state = ConnectionState::Connected;
        app.autofix_enabled = true;
        let pane = "11111111-2222-3333-4444-555555555555";
        app.agent_sessions.apply(SessionEvent::SessionStarted {
            key: "copilot-key".into(),
            cli_source: CliSource::Copilot,
            pane_session_id: pane.into(),
            cwd: PathBuf::from("/work"),
            title: "t".into(),
        });
        assert!(app.agent_sessions.is_agent_pane(pane), "precondition: pane is registered as agent-bound");

        app.handle_event(AppEvent::WtEvent {
            method: "vt_sequence".to_string(),
            pane_id: pane.to_string(),
            tab_id: Some("test-tab".to_string()),
            params: serde_json::json!({
                "session_id": pane,
                "sequence": "osc:133;D;1",
            }),
        });

        assert_eq!(
            app.tab_mut("test-tab").autofix.pane_id.as_deref(),
            Some(pane),
            "shell failure must arm autofix even when the registry still holds a stale agent binding for the pane"
        );
    }

    /// Positive coverage: a vt_sequence (osc:133;D;1) in a normal shell pane
    /// still fires autofix (the proper command-failure signal). Ensures the
    /// new "vt_sequence-only" routing doesn't silently disable autofix.
    #[test]
    fn vt_sequence_failure_in_normal_pane_still_triggers_autofix() {
        let mut app = test_app();
        app.state = ConnectionState::Connected;
        app.autofix_enabled = true;
        let pane = "fedcba98-7654-3210-fedc-ba9876543210";

        app.handle_event(AppEvent::WtEvent {
            method: "vt_sequence".to_string(),
            pane_id: pane.to_string(),
            tab_id: Some("test-tab".to_string()),
            params: serde_json::json!({
                "session_id": pane,
                "sequence": "osc:133;D;1",
            }),
        });

        assert_eq!(
            app.tab_mut("test-tab").autofix.pane_id.as_deref(),
            Some(pane),
            "vt_sequence osc:133;D;<non-zero> in a normal pane must still arm autofix"
        );
    }

    fn vt_event(pane: &str, tab: &str, seq: &str) -> AppEvent {
        AppEvent::WtEvent {
            method: "vt_sequence".to_string(),
            pane_id: pane.to_string(),
            tab_id: Some(tab.to_string()),
            params: serde_json::json!({ "session_id": pane, "sequence": seq }),
        }
    }

    /// Detected state must survive the `osc:133;A` that PowerShell emits
    /// ~1ms after the triggering `osc:133;D` — that A is the trigger's
    /// echo, not the user moving on. The NEXT prompt-start (after the
    /// user actually does something) is what dismisses.
    #[test]
    fn detected_survives_trigger_echo_dismisses_on_next_prompt_start() {
        let mut app = test_app();
        app.state = ConnectionState::Connected;
        app.autofix_enabled = false; // suggest-mode → produces Detected
        let pane = "11111111-2222-3333-4444-555555555555";
        let tab = "tab-A";

        // D;1 → Detected pill armed.
        app.handle_event(vt_event(pane, tab, "osc:133;D;1"));
        assert!(
            matches!(
                app.tab_mut(tab).autofix.bar_snapshot,
                AutofixBarSnapshot::Detected { .. }
            ),
            "D;1 must establish Detected"
        );
        assert_eq!(
            app.tab_mut(tab).autofix.trigger_echo_pane.as_deref(),
            Some(pane),
            "trigger_echo_pane must be armed at Detected set so the immediate A is consumed"
        );

        // Immediate A (PowerShell redrawing the prompt) — must NOT dismiss.
        app.handle_event(vt_event(pane, tab, "osc:133;A"));
        assert!(
            matches!(
                app.tab_mut(tab).autofix.bar_snapshot,
                AutofixBarSnapshot::Detected { .. }
            ),
            "the trigger-echo A must not dismiss Detected"
        );
        assert!(
            app.tab_mut(tab).autofix.trigger_echo_pane.is_none(),
            "trigger_echo_pane must be consumed by the echo A"
        );

        // A second A (user actually moved on) — must dismiss.
        app.handle_event(vt_event(pane, tab, "osc:133;A"));
        assert!(
            matches!(
                app.tab_mut(tab).autofix.bar_snapshot,
                AutofixBarSnapshot::Idle
            ),
            "a subsequent A (user moved on) must dismiss Detected"
        );
    }

    /// Pending state (auto-suggest on path: D arms `autofix.pane_id` and
    /// emits Pending) must also survive the trigger-echo A and dismiss on
    /// the next user-driven prompt-start. The Pending/Armed dismiss path
    /// goes through `turn_cancel` (or its manual fallback when no ACP
    /// session is bound).
    #[test]
    fn pending_survives_trigger_echo_dismisses_on_next_prompt_start() {
        let mut app = test_app();
        app.state = ConnectionState::Connected;
        app.autofix_enabled = true; // LLM-call path → produces Pending
        let pane = "22222222-3333-4444-5555-666666666666";
        let tab = "tab-B";

        app.handle_event(vt_event(pane, tab, "osc:133;D;1"));
        assert_eq!(
            app.tab_mut(tab).autofix.pane_id.as_deref(),
            Some(pane),
            "D;1 must arm Pending (autofix.pane_id set)"
        );
        assert_eq!(
            app.tab_mut(tab).autofix.trigger_echo_pane.as_deref(),
            Some(pane),
        );

        // Echo A — Pending stays.
        app.handle_event(vt_event(pane, tab, "osc:133;A"));
        assert_eq!(
            app.tab_mut(tab).autofix.pane_id.as_deref(),
            Some(pane),
            "trigger-echo A must not cancel Pending"
        );

        // Real A — turn_cancel (or manual fallback) clears pane_id and bar.
        app.handle_event(vt_event(pane, tab, "osc:133;A"));
        assert!(
            app.tab_mut(tab).autofix.pane_id.is_none(),
            "subsequent A must cancel Pending"
        );
        assert!(
            matches!(
                app.tab_mut(tab).autofix.bar_snapshot,
                AutofixBarSnapshot::Idle
            ),
            "bar must return to Idle after Pending cancel"
        );
    }

    /// User clicks the Detected pill on a stable prompt → autofix
    /// transitions Detected → Pending → Armed via the LLM call. No D
    /// event is in flight during this transition, so no echo A is
    /// coming. The next prompt-start the user produces must dismiss on
    /// the FIRST Enter, not be eaten as a fake echo.
    ///
    /// Bug repro before this fix: emit_autofix_state_pending used to
    /// arm `trigger_echo_pane` unconditionally, so the forced-from-
    /// Detected path planted a gate with no echo to consume. The
    /// gate then ate the user's first real Enter.
    #[test]
    fn force_from_detected_does_not_arm_echo_gate() {
        let mut app = test_app();
        app.state = ConnectionState::Connected;
        app.autofix_enabled = false; // suggest-mode produces Detected first
        let pane = "44444444-5555-6666-7777-888888888888";
        let tab = "tab-D";

        // D;1 → Detected (gate armed, echo A consumed below).
        app.handle_event(vt_event(pane, tab, "osc:133;D;1"));
        app.handle_event(vt_event(pane, tab, "osc:133;A")); // echo
        assert!(
            app.tab_mut(tab).autofix.trigger_echo_pane.is_none(),
            "echo A must consume the gate"
        );

        // User clicks the pill → forced trigger → Pending. This is on a
        // stable prompt with no D in flight — gate must NOT re-arm.
        let synth = WtNotification {
            severity: WtEventSeverity::Actionable,
            pane_id: pane.to_string(),
            tab_id: Some(tab.to_string()),
            summary: "Command failed (exit 1)".to_string(),
            acknowledged: false,
            age_ticks: 0,
        };
        app.trigger_autofix_inner(&synth, /*forced*/ true);
        assert!(
            app.tab_mut(tab).autofix.trigger_echo_pane.is_none(),
            "force-from-Detected path must not arm trigger_echo_pane — \
             no D is in flight, no echo A is coming, and arming would eat \
             the user's first dismiss Enter"
        );
    }

    /// Returning to Idle clears the echo guard. Otherwise, a stale
    /// `trigger_echo_pane` could swallow a real prompt-start that arrives
    /// long after the state has already been cleared by other means
    /// (e.g. the user clicked the Suggested pill, then the autofix
    /// re-fires later in the same pane).
    #[test]
    fn trigger_echo_pane_clears_when_state_returns_to_idle() {
        let mut app = test_app();
        app.state = ConnectionState::Connected;
        app.autofix_enabled = false;
        let pane = "33333333-4444-5555-6666-777777777777";
        let tab = "tab-C";

        app.handle_event(vt_event(pane, tab, "osc:133;D;1"));
        assert_eq!(
            app.tab_mut(tab).autofix.trigger_echo_pane.as_deref(),
            Some(pane)
        );

        // Externally clear the bar (e.g. user dismissed via Esc / pill).
        let tab_owned = tab.to_string();
        app.emit_autofix_state_cleared(&tab_owned);
        assert!(
            app.tab_mut(tab).autofix.trigger_echo_pane.is_none(),
            "trigger_echo_pane must be released when bar transitions to Idle, \
             otherwise the next real prompt-start would be silently swallowed"
        );
    }

    /// Gemini "manual launch" scenario: the user opened a normal pwsh/cmd
    /// pane and typed `gemini`. The hook bridge fires `agent.session.start`
    /// (binding the pane) but `agent.session.end` is unreliable on `/exit`
    /// (Gemini cancels its own hook chain), AND the pane stays alive after
    /// Gemini exits because pwsh keeps running. So neither
    /// `connection_state: closed` nor `SessionStopped` ever arrive.
    ///
    /// The shell's FinalTerm prompt-start marker (`osc:133;A`) fires when
    /// pwsh redraws its prompt after Gemini releases the foreground —
    /// that's our signal.
    #[test]
    fn osc133_prompt_start_in_agent_pane_transitions_row_to_ended() {
        use crate::agent_sessions::{CliSource, SessionEvent};
        use std::path::PathBuf;
        let mut app = test_app();
        let pane = "ffffffff-eeee-dddd-cccc-bbbbbbbbbbbb";
        app.agent_sessions.apply(SessionEvent::SessionStarted {
            key: "gemini-key".into(),
            cli_source: CliSource::Gemini,
            pane_session_id: pane.into(),
            cwd: PathBuf::from("/work"),
            title: "t".into(),
        });
        // Sanity: row is live before the prompt-start arrives.
        assert!(app.agent_sessions.is_agent_pane(pane));

        app.handle_event(AppEvent::WtEvent {
            method: "vt_sequence".to_string(),
            pane_id: pane.to_string(),
            tab_id: None,
            params: serde_json::json!({
                "session_id": pane,
                "sequence": "osc:133;A",
            }),
        });

        let row = app
            .agent_sessions
            .iter_sorted()
            .into_iter()
            .find(|s| s.key == "gemini-key")
            .expect("row still exists");
        assert!(
            matches!(row.status, crate::agent_sessions::AgentStatus::Ended),
            "agent-bound pane seeing osc:133;A must transition to Ended",
        );
        // The pane→key binding must be cleared either way.
        assert!(
            !app.agent_sessions.is_agent_pane(pane),
            "pane binding should be cleared after close",
        );
    }

    /// Negative coverage: `osc:133;A` in a normal (non-agent) pane must
    /// never apply PaneClosed (defensive — the registry would treat it as
    /// a no-op anyway, but verify the guard short-circuits the call).
    #[test]
    fn osc133_prompt_start_in_normal_pane_is_inert() {
        let mut app = test_app();
        let pane = "00000000-1111-2222-3333-444444444444";
        // No SessionStarted apply -> not an agent pane.
        app.handle_event(AppEvent::WtEvent {
            method: "vt_sequence".to_string(),
            pane_id: pane.to_string(),
            tab_id: None,
            params: serde_json::json!({
                "session_id": pane,
                "sequence": "osc:133;A",
            }),
        });
        // Nothing to assert positively — the registry just doesn't grow.
        assert_eq!(app.agent_sessions.iter_sorted().len(), 0);
    }

    /// Gemini scenario: no `agent.session.end` hook bridge, so the only
    /// signal we get when the user `/exit`s a resumed Gemini pane is
    /// WT-native `connection_state: closed`. Without bridging that into a
    /// `SessionEvent::PaneClosed`, the row stays stuck at Idle/Working
    /// forever in the session management list.
    #[test]
    fn connection_state_closed_transitions_agent_row_to_ended() {
        use crate::agent_sessions::{AgentStatus, CliSource, SessionEvent};
        use std::path::PathBuf;
        let mut app = test_app();
        let pane = "deadbeef-1111-2222-3333-444455556666";
        // Gemini-style: the pane was bound (via ResumePaneAssigned in real
        // life; SessionStarted is a stand-in here) but no session.end hook
        // ever fires.
        app.agent_sessions.apply(SessionEvent::SessionStarted {
            key: "gemini-key".into(),
            cli_source: CliSource::Gemini,
            pane_session_id: pane.into(),
            cwd: PathBuf::from("/work"),
            title: "t".into(),
        });
        // Sanity: the row is live before close.
        let s = app
            .agent_sessions
            .iter_sorted()
            .into_iter()
            .find(|s| s.key == "gemini-key")
            .expect("row exists");
        assert!(matches!(s.status, AgentStatus::Idle | AgentStatus::Working));

        app.handle_event(AppEvent::WtEvent {
            method: "connection_state".to_string(),
            pane_id: pane.to_string(),
            tab_id: None,
            params: serde_json::json!({"session_id": pane, "state": "closed"}),
        });

        let row = app
            .agent_sessions
            .iter_sorted()
            .into_iter()
            .find(|s| s.key == "gemini-key")
            .expect("row still exists");
        assert!(
            matches!(row.status, AgentStatus::Ended),
            "Gemini row must transition to Ended on connection_state:closed",
        );
        assert!(
            !app.agent_sessions.is_agent_pane(pane),
            "pane binding should be cleared after close",
        );
    }

    /// Regression: OSC 133;A in an AGENT-PANE-origin session must NOT
    /// trigger PaneClosed. The previous gate (`is_agent_pane(pane_id)`)
    /// fired on any pane with a bound session, demoting agent panes
    /// when WT itself emitted a stray OSC 133;A around focus events.
    /// Fix at app.rs ~4717 restricts the bridge to origin=Unknown
    /// (shell-pane agents like `gemini` typed in pwsh).
    #[test]
    fn osc133_prompt_start_in_agent_pane_origin_is_ignored() {
        use crate::agent_sessions::{CliSource, SessionEvent, SessionOrigin};
        use std::path::PathBuf;
        let mut app = test_app();
        let pane = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
        let key = "copilot-agent-pane-key";
        app.agent_sessions.apply(SessionEvent::SessionStarted {
            key: key.into(),
            cli_source: CliSource::Copilot,
            pane_session_id: pane.into(),
            cwd: PathBuf::from("/work"),
            title: "t".into(),
        });
        // Stamp this row as agent-pane origin (the wta-managed kind).
        app.agent_sessions.set_origin(key, SessionOrigin::AgentPane);

        // Sanity: row is Live before the stray OSC arrives.
        let before = app
            .agent_sessions
            .iter_sorted()
            .into_iter()
            .find(|s| s.key == key)
            .expect("row exists");
        assert!(matches!(
            before.status,
            crate::agent_sessions::AgentStatus::Idle
                | crate::agent_sessions::AgentStatus::Working
        ));
        assert_eq!(before.origin, SessionOrigin::AgentPane);

        // Fire OSC 133;A — this is the event WT spuriously emits
        // around focus_pane on agent panes. The handler must IGNORE
        // it for agent-pane origin and leave the row Live.
        app.handle_event(AppEvent::WtEvent {
            method: "vt_sequence".to_string(),
            pane_id: pane.to_string(),
            tab_id: None,
            params: serde_json::json!({
                "session_id": pane,
                "sequence": "osc:133;A",
            }),
        });

        let after = app
            .agent_sessions
            .iter_sorted()
            .into_iter()
            .find(|s| s.key == key)
            .expect("row must still exist (must NOT be pruned by spurious PaneClosed)");
        assert!(
            matches!(
                after.status,
                crate::agent_sessions::AgentStatus::Idle
                    | crate::agent_sessions::AgentStatus::Working
            ),
            "agent-pane row must stay Live on OSC 133;A; got {:?}",
            after.status,
        );
        assert!(
            app.agent_sessions.is_agent_pane(pane),
            "pane binding must NOT be cleared by a spurious shell-prompt OSC",
        );
    }

    // ─── turn-state integration tests ──────────────────────────────────────
    //
    // Drive `App` directly through the turn-state transitions in
    // `doc/specs/turn-state-refactor.md`'s table. We use the active tab's
    // `DEFAULT_TAB_ID` as the session key — `session_tab_mut` falls back to
    // the active tab when the id is unknown, which keeps these tests free
    // of ACP wiring.

    fn submit_test_prompt(app: &mut App, text: &str) {
        let prompt = SubmittedPrompt {
            id: 42,
            text: text.into(),
            submitted_at_unix_s: 0.0,
            autofix: None,
        };
        app.turn_submit_prompt(DEFAULT_TAB_ID, prompt);
    }

    /// Form A end-to-end (mock-acp-agent spec, "option 2"): the mock + real
    /// `WtaClient` harness lives in the acp module (it needs the private
    /// `WtaClient`), but this App-state assertion lives here where `App`
    /// internals are reachable. We drive a prompt through the **real** ACP
    /// client against the deterministic mock, pump the resulting `AppEvent`s
    /// into a **real** `App`, and assert the streamed reply is what the chat
    /// view would show — i.e. what the chat should display is covered without a
    /// real terminal, real WT, or an LLM.
    #[tokio::test]
    async fn mock_agent_reply_streams_into_app_chat() {
        use crate::protocol::acp::client::mock_agent_tests::connect_mock_agent;
        use agent_client_protocol as acp;
        use agent_client_protocol::Agent as _;

        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                // Borrow the acp-module harness: deterministic mock wired to a
                // real WtaClient over an in-memory duplex.
                let (conn, mut event_rx, _seen) = connect_mock_agent();
                conn.initialize(acp::InitializeRequest::new(acp::ProtocolVersion::LATEST))
                    .await
                    .expect("initialize failed");
                let session = conn
                    .new_session(acp::NewSessionRequest::new("/test"))
                    .await
                    .expect("new_session failed");
                conn.prompt(acp::PromptRequest::new(
                    session.session_id.clone(),
                    vec!["hello".into()],
                ))
                .await
                .expect("prompt failed");

                // Real App with an in-flight turn so streamed chunks are accepted
                // (the AgentMessageChunk handler drops chunks on an idle turn).
                let mut app = test_app();
                submit_test_prompt(&mut app, "hello");

                // Pump the AppEvents the real WtaClient produced into the real
                // App until the agent message chunk has been applied (bounded so
                // a wiring bug fails fast instead of hanging).
                let pumped = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                    loop {
                        match event_rx.recv().await {
                            Some(ev) => {
                                let is_chunk = matches!(ev, AppEvent::AgentMessageChunk { .. });
                                app.handle_event(ev);
                                if is_chunk {
                                    break;
                                }
                            }
                            None => break,
                        }
                    }
                })
                .await;
                assert!(pumped.is_ok(), "timed out waiting for the agent message chunk");

                // "What the chat shows" while streaming: the mock's reply is in
                // the active tab's streaming buffer.
                assert!(
                    app.current_tab()
                        .pending_agent_response
                        .contains("MOCK_OK:hello"),
                    "mock reply must stream into the App chat buffer; got {:?}",
                    app.current_tab().pending_agent_response
                );
            })
            .await;
    }

    /// Drive a prompt through the real ACP client against a mock that requests
    /// permission, pump the `PermissionRequest` into a real `App`, then simulate
    /// the user's key choice and assert the chosen option round-trips back to
    /// the agent. `expected_keys` is the key sequence the user presses; `want`
    /// is the option id the mock must end up recording.
    async fn run_permission_scenario(expected_keys: &[KeyCode], want: &str) {
        use crate::protocol::acp::client::mock_agent_tests::connect_mock_agent_asking_permission;
        use agent_client_protocol as acp;
        use agent_client_protocol::Agent as _;

        let (conn, mut event_rx, outcome) = connect_mock_agent_asking_permission();
        conn.initialize(acp::InitializeRequest::new(acp::ProtocolVersion::LATEST))
            .await
            .expect("initialize failed");
        let session = conn
            .new_session(acp::NewSessionRequest::new("/test"))
            .await
            .expect("new_session failed");
        conn.prompt(acp::PromptRequest::new(
            session.session_id.clone(),
            vec!["do it".into()],
        ))
        .await
        .expect("prompt failed");

        // Real App with an in-flight turn so the permission request is accepted.
        let mut app = test_app();
        submit_test_prompt(&mut app, "do it");

        // Pump events until the PermissionRequest is applied to the App.
        let pumped = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                match event_rx.recv().await {
                    Some(ev) => {
                        let is_perm = matches!(ev, AppEvent::PermissionRequest { .. });
                        app.handle_event(ev);
                        if is_perm {
                            break;
                        }
                    }
                    None => break,
                }
            }
        })
        .await;
        assert!(pumped.is_ok(), "timed out waiting for the permission request");

        // Display assertion: the permission card is queued with allow/reject,
        // allow selected by default.
        {
            let perm = app
                .current_tab()
                .permission
                .front()
                .expect("a permission request must be queued for display");
            assert_eq!(perm.options.len(), 2, "expected allow + reject options");
            assert_eq!(perm.options[0].id, "allow-once");
            assert_eq!(perm.options[1].id, "reject-once");
            assert_eq!(perm.selected, 0, "allow must be selected by default");
        }

        // Simulate the user's key choice (e.g. Enter = allow, Right then Enter = reject).
        for key in expected_keys {
            app.handle_key(KeyEvent::from(*key));
        }

        // The choice must round-trip back to the agent.
        let resolved = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Some(v) = outcome.lock().unwrap().clone() {
                    break v;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("timed out waiting for the permission outcome to reach the agent");
        assert_eq!(resolved, want, "the agent must receive the user's choice");

        // The card is cleared once resolved.
        assert!(
            app.current_tab().permission.is_empty(),
            "the permission card must clear after the user resolves it"
        );
    }

    /// Permission allow round-trip: Enter on the default-selected option (allow)
    /// surfaces the card, then sends `allow-once` back to the agent.
    #[tokio::test]
    async fn permission_allow_round_trips_to_agent() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(run_permission_scenario(&[KeyCode::Enter], "allow-once"))
            .await;
    }

    /// Permission reject round-trip: Right moves selection to reject, Enter
    /// sends `reject-once` back to the agent.
    #[tokio::test]
    async fn permission_reject_round_trips_to_agent() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(run_permission_scenario(
                &[KeyCode::Right, KeyCode::Enter],
                "reject-once",
            ))
            .await;
    }

    /// Regression (#permission-quick-keys): the `y` quick-key must resolve to
    /// the allow option even though the wire `kind` is PascalCase (`AllowOnce`)
    /// while the matcher searches for the lowercase substring `allow`. Before
    /// the case-insensitive fix this keypress was a silent no-op and the agent
    /// never received a response — this scenario would time out.
    #[tokio::test]
    async fn permission_quick_allow_key_round_trips_to_agent() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(run_permission_scenario(
                &[KeyCode::Char('y')],
                "allow-once",
            ))
            .await;
    }

    /// Regression (#permission-quick-keys): the `n` quick-key must resolve to
    /// the reject option. See [`permission_quick_allow_key_round_trips_to_agent`].
    #[tokio::test]
    async fn permission_quick_reject_key_round_trips_to_agent() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(run_permission_scenario(
                &[KeyCode::Char('n')],
                "reject-once",
            ))
            .await;
    }

    /// The `kind` string is the ACP `PermissionOptionKind` rendered via
    /// `format!("{:?}", …)`, i.e. PascalCase (`AllowOnce`, `RejectAlways`).
    /// `PermOption::is_allow`/`is_reject` must match those case-insensitively
    /// so the `y`/`n` quick-keys and the `[Y]`/`[N]` button labels both fire.
    #[test]
    fn perm_option_kind_matching_is_case_insensitive() {
        let opt = |kind: &str| PermOption {
            id: "id".into(),
            name: "name".into(),
            kind: kind.into(),
        };
        for k in ["AllowOnce", "AllowAlways", "allow_once"] {
            assert!(opt(k).is_allow(), "{k:?} must be recognized as allow");
            assert!(!opt(k).is_reject(), "{k:?} must not be reject");
        }
        for k in ["RejectOnce", "RejectAlways", "reject_once"] {
            assert!(opt(k).is_reject(), "{k:?} must be recognized as reject");
            assert!(!opt(k).is_allow(), "{k:?} must not be allow");
        }

        // PermissionState index helpers pick the first matching option.
        let perm = PermissionState {
            description: String::new(),
            options: vec![opt("AllowOnce"), opt("RejectOnce")],
            selected: 0,
            responder: None,
        };
        assert_eq!(perm.allow_index(), Some(0));
        assert_eq!(perm.reject_index(), Some(1));
    }

    /// Tool-call card: when the mock proposes a command (a `ToolCall`
    /// notification), the real `WtaClient` turns it into `AppEvent::ToolCall`
    /// and the real `App` surfaces a tool-call card in the chat — the display
    /// state the insert/run affordance hangs off.
    #[tokio::test]
    async fn tool_call_surfaces_card_in_chat() {
        use crate::protocol::acp::client::mock_agent_tests::connect_mock_agent_proposing_tool;
        use agent_client_protocol as acp;
        use agent_client_protocol::Agent as _;

        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (conn, mut event_rx) = connect_mock_agent_proposing_tool();
                conn.initialize(acp::InitializeRequest::new(acp::ProtocolVersion::LATEST))
                    .await
                    .expect("initialize failed");
                let session = conn
                    .new_session(acp::NewSessionRequest::new("/test"))
                    .await
                    .expect("new_session failed");
                conn.prompt(acp::PromptRequest::new(
                    session.session_id.clone(),
                    vec!["run it".into()],
                ))
                .await
                .expect("prompt failed");

                let mut app = test_app();
                submit_test_prompt(&mut app, "run it");

                let pumped = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                    loop {
                        match event_rx.recv().await {
                            Some(ev) => {
                                let is_tool = matches!(ev, AppEvent::ToolCall { .. });
                                app.handle_event(ev);
                                if is_tool {
                                    break;
                                }
                            }
                            None => break,
                        }
                    }
                })
                .await;
                assert!(pumped.is_ok(), "timed out waiting for the tool call");

                // Display assertion: the proposed command shows as a tool-call card.
                let has_card = app.current_tab().messages.iter().any(|m| {
                    matches!(m, ChatMessage::ToolCall { title, .. } if title == "Run: echo hi")
                });
                assert!(
                    has_card,
                    "a tool-call card must surface in the chat; got {:?}",
                    app.current_tab().messages
                );
            })
            .await;
    }

    /// Pump `AppEvent`s into a real `App` until `pred` matches (inclusive), with
    /// a timeout so a wiring bug fails fast instead of hanging.
    async fn pump_until(
        app: &mut App,
        rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
        pred: impl Fn(&AppEvent) -> bool,
    ) {
        let r = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                match rx.recv().await {
                    Some(ev) => {
                        let stop = pred(&ev);
                        app.handle_event(ev);
                        if stop {
                            break;
                        }
                    }
                    None => break,
                }
            }
        })
        .await;
        assert!(r.is_ok(), "timed out pumping events");
    }

    /// Drive initialize → new_session → prompt against the harness connection,
    /// leaving an in-flight turn whose streamed notifications the caller pumps
    /// into a real `App`. Returns `()` — it only drives ACP traffic; the caller
    /// owns the `App`.
    async fn app_after_prompt(
        conn: &agent_client_protocol::ClientSideConnection,
    ) {
        use agent_client_protocol as acp;
        use agent_client_protocol::Agent as _;
        conn.initialize(acp::InitializeRequest::new(acp::ProtocolVersion::LATEST))
            .await
            .expect("initialize failed");
        let session = conn
            .new_session(acp::NewSessionRequest::new("/test"))
            .await
            .expect("new_session failed");
        conn.prompt(acp::PromptRequest::new(
            session.session_id.clone(),
            vec!["go".into()],
        ))
        .await
        .expect("prompt failed");
    }

    /// Streaming: a reply split across two `AgentMessageChunk`s must coalesce
    /// into one contiguous streaming buffer in the chat.
    #[tokio::test]
    async fn streaming_two_chunks_coalesce_in_app_chat() {
        use crate::protocol::acp::client::mock_agent_tests::connect_mock_agent_streaming_two_chunks;

        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (conn, mut event_rx) = connect_mock_agent_streaming_two_chunks();
                app_after_prompt(&conn).await;

                let mut app = test_app();
                submit_test_prompt(&mut app, "go");

                // Two chunks arrive; pump each.
                pump_until(&mut app, &mut event_rx, |ev| {
                    matches!(ev, AppEvent::AgentMessageChunk { .. })
                })
                .await;
                pump_until(&mut app, &mut event_rx, |ev| {
                    matches!(ev, AppEvent::AgentMessageChunk { .. })
                })
                .await;

                assert_eq!(
                    app.current_tab().pending_agent_response,
                    "MOCK_OK",
                    "streamed chunks must coalesce into one contiguous reply"
                );
            })
            .await;
    }

    /// Tool-call lifecycle: a `ToolCallUpdate(Completed)` after the initial
    /// `ToolCall` must update the card's status in-place (not duplicate it).
    #[tokio::test]
    async fn tool_call_completion_updates_card_status() {
        use crate::protocol::acp::client::mock_agent_tests::connect_mock_agent_completing_tool;

        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (conn, mut event_rx) = connect_mock_agent_completing_tool();
                app_after_prompt(&conn).await;

                let mut app = test_app();
                submit_test_prompt(&mut app, "go");

                pump_until(&mut app, &mut event_rx, |ev| {
                    matches!(ev, AppEvent::ToolCallUpdate { .. })
                })
                .await;

                let cards: Vec<_> = app
                    .current_tab()
                    .messages
                    .iter()
                    .filter_map(|m| match m {
                        ChatMessage::ToolCall { id, status, .. } => Some((id.clone(), status.clone())),
                        _ => None,
                    })
                    .collect();
                assert_eq!(cards.len(), 1, "the update must edit in place, not add a card");
                assert_eq!(cards[0].0, "mock-tool-1");
                assert_eq!(cards[0].1, "Completed", "card status must reflect the update");
            })
            .await;
    }

    /// Plan: a `Plan` notification must surface as a plan card with its entries.
    #[tokio::test]
    async fn plan_surfaces_card_in_chat() {
        use crate::protocol::acp::client::mock_agent_tests::connect_mock_agent_proposing_plan;

        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (conn, mut event_rx) = connect_mock_agent_proposing_plan();
                app_after_prompt(&conn).await;

                let mut app = test_app();
                submit_test_prompt(&mut app, "go");

                pump_until(&mut app, &mut event_rx, |ev| matches!(ev, AppEvent::Plan { .. })).await;

                let plan = app.current_tab().messages.iter().find_map(|m| match m {
                    ChatMessage::Plan(entries) => Some(entries.clone()),
                    _ => None,
                });
                let entries = plan.expect("a plan card must surface in the chat");
                assert_eq!(entries.len(), 2);
                assert_eq!(entries[0].content, "Step one");
                assert_eq!(entries[0].status, PlanEntryStatus::InProgress);
                assert_eq!(entries[1].content, "Step two");
            })
            .await;
    }

    /// Render a driven `App` to a ratatui `TestBackend` and return the visible
    /// buffer as text (rows joined by `\n`). Lets scenarios assert on what is
    /// actually painted, not just on `App` state.
    fn render_to_text(app: &mut App, width: u16, height: u16) -> String {
        use ratatui::{backend::TestBackend, Terminal};
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| crate::ui::render(frame, app))
            .expect("render must not panic");
        let buf = terminal.backend().buffer();
        let w = buf.area.width as usize;
        let mut out = String::new();
        for (i, cell) in buf.content.iter().enumerate() {
            if i > 0 && i % w == 0 {
                out.push('\n');
            }
            out.push_str(cell.symbol());
        }
        out
    }

    /// Render (C063 "prompt out-of-focus appearance"): when keyboard focus leaves the agent pane
    /// (`pane_focused = false`) the input box must still look correct — the prompt marker and the
    /// connection placeholder still paint, the box is not blanked or broken. Only the caret styling
    /// changes (a solid REVERSED block when focused → DIM when not; input.rs:69/90), which is the
    /// intended out-of-focus appearance.
    #[test]
    fn render_input_box_intact_when_pane_unfocused() {
        let _g = crate::test_support::lock_locale();
        rust_i18n::set_locale("en-US");
        let mut app = test_app();
        app.state = ConnectionState::Connected;

        // Focused baseline: the input box paints the prompt + connected placeholder.
        app.pane_focused = true;
        let focused = render_to_text(&mut app, 80, 24);
        let placeholder = rust_i18n::t!("input.placeholder.connected").into_owned();
        assert!(
            focused.contains('>') && focused.contains(&placeholder),
            "sanity: the focused input must paint the prompt + placeholder; rendered:\n{focused}"
        );

        // Focus leaves the pane: the input box must remain intact (prompt + placeholder still there),
        // i.e. losing focus does not blank or corrupt the input surface.
        app.pane_focused = false;
        let unfocused = render_to_text(&mut app, 80, 24);
        assert!(
            unfocused.contains('>'),
            "the out-of-focus input must still paint the prompt marker; rendered:\n{unfocused}"
        );
        assert!(
            unfocused.contains(&placeholder),
            "the out-of-focus input must still paint the connection placeholder (box intact); rendered:\n{unfocused}"
        );
    }

    /// Render (C067 "non-ASCII input"): non-ASCII characters typed into the agent-pane input must be
    /// accepted and painted correctly (multi-byte UTF-8: accented Latin, Greek, CJK). Drives the real
    /// key handler with `KeyCode::Char` events (a Rust `char` is a full Unicode scalar, exactly what a
    /// keyboard/IME commit produces) and asserts they render. The E2E send path (wtcli send-keys)
    /// cannot carry non-ASCII, so this unit test is the deterministic coverage for the product side;
    /// the IME-composition half stays MANUAL. `insert_input_char` advances the caret by
    /// `ch.len_utf8()` (app.rs:1842), so multi-byte chars must round-trip.
    #[test]
    fn render_agent_input_accepts_non_ascii() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let _g = crate::test_support::lock_locale();
        rust_i18n::set_locale("en-US");
        let mut app = test_app();
        app.state = ConnectionState::Connected;
        let sample = "café Ω 你好";
        for c in sample.chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        // The input buffer holds the exact non-ASCII string (the product contract: non-ASCII input
        // is accepted verbatim, multi-byte caret advance included)...
        assert_eq!(
            app.current_tab().input,
            sample,
            "non-ASCII characters must be accepted verbatim into the input buffer"
        );
        // ...and the painted input line shows the multi-byte glyphs. (CJK are double-width; the
        // ratatui TestBackend splits a wide glyph across two cells so the raw cell-join may not
        // reconstruct the CJK codepoint — assert the single-width non-ASCII glyphs render, and rely
        // on the input-buffer assertion above for the wide-char acceptance contract.)
        let text = render_to_text(&mut app, 80, 24);
        for needle in ["café", "Ω"] {
            assert!(
                text.contains(needle),
                "the agent input must paint the non-ASCII text {needle:?}; rendered:\n{text}"
            );
        }
    }

    /// Render: a committed agent message must actually appear in the painted
    /// chat view (not just in `App` state). Lifts `ui/chat.rs` coverage.
    #[test]
    fn render_chat_shows_agent_message() {
        let mut app = test_app();
        app.state = ConnectionState::Connected;
        app.current_tab_mut()
            .messages
            .push(ChatMessage::Agent("VISIBLE_REPLY_XYZ".into()));

        let text = render_to_text(&mut app, 80, 24);
        assert!(
            text.contains("VISIBLE_REPLY_XYZ"),
            "the chat view must paint the agent message; rendered:\n{text}"
        );
    }

    /// Render (C134 "Hooks off behavior is safe"): with session management OFF — no tracked
    /// sessions, exactly as when wt-agent-hooks are not installed — the session-management (Agents)
    /// view must still paint a STABLE empty state (the draw does not panic and the navigation footer
    /// hint is drawn) rather than a broken/blank surface.
    #[test]
    fn render_agents_view_empty_when_no_sessions_is_stable() {
        let mut app = test_app();
        let key = app.active_tab_key().to_string();
        // No SessionStarted events applied => the registry is empty, exactly as when session
        // management is off (no wt-agent-hooks tracking any sessions).
        assert!(
            app.agents_rows_for_tab(&key).is_empty(),
            "precondition: no tracked sessions (hooks off)"
        );
        app.current_tab_mut().current_view = View::Agents;

        // render_to_text asserts the draw does not panic.
        let text = render_to_text(&mut app, 80, 24);

        // The navigation footer hint (agents.footer_hint) is drawn in the empty state too; its
        // leading "↑ ↓" arrows are invariant across every bundled locale, so assert on those.
        assert!(
            text.contains('↑') && text.contains('↓'),
            "the empty session view must paint the stable navigation footer hint; rendered:\n{text}"
        );
    }

    /// Render: a queued permission request must paint its description and the
    /// allow/reject option labels. Lifts `ui/permission.rs` coverage.
    #[test]
    fn render_permission_card_shows_options() {
        let mut app = test_app();
        app.state = ConnectionState::Connected;
        app.current_tab_mut().permission.push_back(PermissionState {
            description: "Run: echo PERM_XYZ".into(),
            options: vec![
                PermOption {
                    id: "allow-once".into(),
                    name: "Allow once".into(),
                    kind: "AllowOnce".into(),
                },
                PermOption {
                    id: "reject-once".into(),
                    name: "Reject".into(),
                    kind: "RejectOnce".into(),
                },
            ],
            selected: 0,
            responder: None,
        });

        let text = render_to_text(&mut app, 80, 24);
        assert!(
            text.contains("PERM_XYZ"),
            "the permission card must paint its description; rendered:\n{text}"
        );
        assert!(
            text.contains("Allow once"),
            "the permission card must paint the allow option; rendered:\n{text}"
        );
    }

    /// Render: a tool-call card must paint its title in the chat. Lifts the
    /// tool-call branch of `ui/chat.rs`.
    #[test]
    fn render_tool_call_card_in_chat() {
        let mut app = test_app();
        app.state = ConnectionState::Connected;
        app.current_tab_mut().messages.push(ChatMessage::ToolCall {
            id: "mock-tool-1".into(),
            title: "Run: echo TOOL_XYZ".into(),
            status: "Pending".into(),
        });

        let text = render_to_text(&mut app, 80, 24);
        assert!(
            text.contains("TOOL_XYZ"),
            "the tool-call card must paint its title; rendered:\n{text}"
        );
    }

    /// Render: the `/help` overlay must list the slash commands. Lifts
    /// `ui/command_popup.rs`.
    #[test]
    fn render_help_overlay_lists_commands() {
        let mut app = test_app();
        app.state = ConnectionState::Connected;
        app.help_overlay_visible = true;

        let text = render_to_text(&mut app, 80, 24);
        assert!(
            text.contains("/restart"),
            "the help overlay must list slash commands; rendered:\n{text}"
        );
    }

    /// Render: the `/model` picker must list the advertised models. Lifts
    /// `ui/model_popup.rs`.
    #[test]
    fn render_model_picker_lists_models() {
        let mut app = test_app();
        app.state = ConnectionState::Connected;
        app.available_models = vec![
            AcpModelInfo {
                id: "pick-1".into(),
                name: "PickModelXYZ".into(),
                description: None,
            },
            AcpModelInfo {
                id: "pick-2".into(),
                name: "OtherModel".into(),
                description: None,
            },
        ];
        app.current_tab_mut().model_picker_open = true;

        let text = render_to_text(&mut app, 80, 24);
        assert!(
            text.contains("PickModelXYZ"),
            "the model picker must list the advertised models; rendered:\n{text}"
        );
    }

    /// Render: the setup diagnostic screen must paint its title and subtitle.
    /// Lifts `ui/setup.rs` (reached only via `AppMode::Setup`).
    #[test]
    fn render_setup_screen_shows_title() {
        let mut app = test_app();
        app.mode = AppMode::Setup;
        app.setup = Some(SetupState {
            reason: SetupReason::AgentError,
            selected_index: 0,
            preflight: PreflightResult::passed_for_custom_agent("custom:qwen"),
            install_in_progress: false,
            install_log: Vec::new(),
            install_error: None,
            options: Vec::new(),
            title: "SETUP_TITLE_XYZ".into(),
            subtitle: "SETUP_SUBTITLE_XYZ".into(),
        });

        let text = render_to_text(&mut app, 80, 24);
        assert!(
            text.contains("SETUP_TITLE_XYZ"),
            "the setup screen must paint its title; rendered:\n{text}"
        );
        assert!(
            text.contains("SETUP_SUBTITLE_XYZ"),
            "the setup screen must paint its subtitle; rendered:\n{text}"
        );
    }

    /// Render: the auth/sign-in screen must paint the selected agent name.
    /// Lifts `ui/auth.rs` (reached only via `AppMode::Auth`).
    #[test]
    fn render_auth_screen_shows_agent_name() {
        let mut app = test_app();
        app.mode = AppMode::Auth;
        app.auth = Some(AuthState {
            agent_id: "copilot".into(),
            agent_name: "SELECTED_AGENT_NAME_XYZ".into(),
            login_command: String::new(),
            checking: true,
            status_message: String::new(),
            enterprise_mode: false,
            enterprise_host: String::new(),
        });

        let text = render_to_text(&mut app, 80, 24);
        assert!(
            text.contains("SELECTED_AGENT_NAME_XYZ"),
            "the auth screen must paint the selected agent name; rendered:\n{text}"
        );
    }

    /// Render: the sessions (agents) view must paint its footer keybinding
    /// hint. Lifts `ui/agents_view.rs` (reached via `View::Agents`).
    #[test]
    fn render_sessions_view_shows_footer_hint() {
        let mut app = test_app();
        app.state = ConnectionState::Connected;
        app.current_tab_mut().current_view = View::Agents;

        let text = render_to_text(&mut app, 80, 24);
        let expected = t!("agents.footer_hint").into_owned();
        // Assert on a stable leading token of the localized hint so the test
        // doesn't break on translation wording while still proving the view
        // painted its chrome.
        let probe: String = expected.chars().take(6).collect();
        assert!(
            !probe.trim().is_empty() && text.contains(&probe),
            "the sessions view must paint its footer hint ({expected:?}); rendered:\n{text}"
        );
    }

    /// Render: the auth screen's sign-in card branch (`checking == false`)
    /// must paint the connect prompt and, for Copilot, the GitHub Enterprise
    /// sign-in footer. Covers the `else` arm of `ui/auth.rs`.
    #[test]
    fn render_auth_sign_in_card() {
        let mut app = test_app();
        app.mode = AppMode::Auth;
        app.auth = Some(AuthState {
            agent_id: "copilot".into(),
            agent_name: "GitHub Copilot".into(),
            login_command: String::new(),
            checking: false,
            status_message: String::new(),
            enterprise_mode: false,
            enterprise_host: String::new(),
        });

        let text = render_to_text(&mut app, 80, 24);
        let connect = t!("auth.card_connect", name = "GitHub Copilot").into_owned();
        let probe: String = connect.chars().take(6).collect();
        assert!(
            !probe.trim().is_empty() && text.contains(&probe),
            "the auth sign-in card must paint the connect prompt ({connect:?}); rendered:\n{text}"
        );
        let footer = t!("auth.enterprise_prompt").into_owned();
        let footer_probe: String = footer.trim_start().chars().take(13).collect();
        assert!(
            !footer_probe.trim().is_empty() && text.contains(&footer_probe),
            "the auth sign-in card must paint the Copilot enterprise footer ({footer:?}); rendered:\n{text}"
        );
    }

    /// `device_verify_url` derives the device-code verification URL from the
    /// login command: github.com by default, but the GitHub Enterprise host
    /// when the command carries `--host https://<host>` (bug B).
    #[test]
    fn device_verify_url_follows_enterprise_host() {
        assert_eq!(
            device_verify_url("copilot login"),
            "https://github.com/login/device"
        );
        assert_eq!(
            device_verify_url("copilot login --host https://mycorp.ghe.com"),
            "https://mycorp.ghe.com/login/device"
        );
        // Trailing slash is trimmed.
        assert_eq!(
            device_verify_url("copilot login --host https://mycorp.ghe.com/"),
            "https://mycorp.ghe.com/login/device"
        );
        // A quoted exe path doesn't confuse the --host parse.
        assert_eq!(
            device_verify_url("\"C:\\Program Files\\copilot.exe\" login --host https://x.ghe.com"),
            "https://x.ghe.com/login/device"
        );
    }

    /// A failed Copilot device-flow login (e.g. an unreachable GitHub
    /// Enterprise host) must surface the captured reason on the auth screen
    /// instead of silently returning to the form with no feedback (bug C).
    #[test]
    fn copilot_login_failure_surfaces_reason() {
        let mut app = test_app();
        app.mode = AppMode::Auth;
        app.auth = Some(AuthState {
            agent_id: "copilot".into(),
            agent_name: "GitHub Copilot".into(),
            login_command: "copilot login --host https://nope.invalid".into(),
            checking: true,
            status_message: String::new(),
            enterprise_mode: true,
            enterprise_host: "nope.invalid".into(),
        });

        app.handle_event(AppEvent::LoginComplete {
            agent_id: "copilot".into(),
            success: false,
            error: Some("Login failed: TypeError: fetch failed".into()),
        });

        let auth = app.auth.as_ref().expect("auth screen stays after failure");
        assert!(!auth.checking, "failure clears the checking spinner");
        assert_eq!(
            auth.status_message, "Login failed: TypeError: fetch failed",
            "the copilot login failure reason must be surfaced"
        );
    }

    /// When no specific reason is captured, a Copilot login failure still shows
    /// a generic localized message rather than nothing.
    #[test]
    fn copilot_login_failure_without_reason_shows_generic_message() {
        let mut app = test_app();
        app.mode = AppMode::Auth;
        app.auth = Some(AuthState {
            agent_id: "copilot".into(),
            agent_name: "GitHub Copilot".into(),
            login_command: "copilot login".into(),
            checking: true,
            status_message: String::new(),
            enterprise_mode: false,
            enterprise_host: String::new(),
        });

        app.handle_event(AppEvent::LoginComplete {
            agent_id: "copilot".into(),
            success: false,
            error: None,
        });

        let auth = app.auth.as_ref().expect("auth screen stays after failure");
        assert_eq!(
            auth.status_message,
            t!("system.authentication_failed").into_owned(),
            "a reasonless copilot failure falls back to a generic message"
        );
    }

    /// Render: a Copilot login failure shows the reason at the *bottom* of the
    /// screen (not appended to the header) followed by situation-specific
    /// guidance. Regression guard for the "error on the first line" report.
    #[test]
    fn render_auth_copilot_failure_shows_reason_and_guidance_at_bottom() {
        let mut app = test_app();
        app.mode = AppMode::Auth;
        app.auth = Some(AuthState {
            agent_id: "copilot".into(),
            agent_name: "GitHub Copilot".into(),
            login_command: "copilot login --host https://nope.invalid".into(),
            checking: false,
            status_message: "Login failed: boom".into(),
            enterprise_mode: true,
            enterprise_host: "nope.invalid".into(),
        });

        let text = render_to_text(&mut app, 100, 24);
        assert!(
            text.contains("Login failed: boom"),
            "the failure reason must render; rendered:\n{text}"
        );
        // Situation-specific guidance is shown (stable leading probe).
        let help = t!("auth.login_failed_help_enterprise").into_owned();
        let help_probe: String = help.trim_start().chars().take(16).collect();
        assert!(
            text.contains(&help_probe),
            "enterprise failure guidance must render ({help:?}); rendered:\n{text}"
        );
        // The reason must NOT be on the header (card_connect) line — it now
        // belongs at the bottom.
        let header = text
            .lines()
            .find(|l| l.contains("Connect GitHub Copilot"))
            .expect("header line present");
        assert!(
            !header.contains("Login failed"),
            "the failure reason must not be in the header; rendered:\n{text}"
        );
    }

    /// Review fix ①: a stale `LoginComplete` after the user escaped the auth
    /// screen (auth = None) must be ignored — it must not force Chat mode or
    /// start ACP for an empty agent.
    #[test]
    fn login_complete_ignored_when_no_active_auth_attempt() {
        let mut app = test_app();
        app.mode = AppMode::Setup;
        app.auth = None;

        app.handle_event(AppEvent::LoginComplete {
            agent_id: "copilot".into(),
            success: true,
            error: None,
        });

        assert_eq!(
            app.mode,
            AppMode::Setup,
            "a stale success must not force Chat mode after the user left auth"
        );
        assert!(
            !app.pending_acp_start,
            "a stale success must not start an ACP client"
        );
    }

    /// Review fix ①: a `LoginComplete` whose agent doesn't match the active
    /// auth attempt (user switched agents) must be ignored.
    #[test]
    fn login_complete_ignored_on_agent_mismatch() {
        let mut app = test_app();
        app.mode = AppMode::Auth;
        app.auth = Some(AuthState {
            agent_id: "claude".into(),
            agent_name: "Claude".into(),
            login_command: "claude /login".into(),
            checking: true,
            status_message: String::new(),
            enterprise_mode: false,
            enterprise_host: String::new(),
        });

        app.handle_event(AppEvent::LoginComplete {
            agent_id: "copilot".into(),
            success: true,
            error: None,
        });

        assert_eq!(
            app.mode,
            AppMode::Auth,
            "a completion for a different agent must not transition to Chat"
        );
        assert!(
            app.auth.is_some(),
            "a mismatched completion must not tear down the active auth screen"
        );
    }

    /// Regression: a Copilot retry must clear any prior failure status so the
    /// checking view shows "Checking…" — not a stale "Login failed…" plus a
    /// phantom "code copied" from the previous attempt. `begin_auth_checking`
    /// is the shared entry point both login paths use.
    #[test]
    fn begin_auth_checking_clears_stale_status() {
        let mut app = test_app();
        app.mode = AppMode::Auth;
        app.auth = Some(AuthState {
            agent_id: "copilot".into(),
            agent_name: "GitHub Copilot".into(),
            login_command: "copilot login --host https://nope.invalid".into(),
            checking: false,
            status_message: "Login failed: TypeError: fetch failed".into(),
            enterprise_mode: true,
            enterprise_host: "nope.invalid".into(),
        });

        app.begin_auth_checking();

        let auth = app.auth.as_ref().expect("auth screen present");
        assert!(auth.checking, "begin_auth_checking must enter the checking state");
        assert!(
            auth.status_message.is_empty(),
            "a retry must clear the stale failure status so the checking view \
             does not render a phantom 'code copied'"
        );
    }

    /// Regression: after a GHE failure, the first Esc collapses the enterprise
    /// input AND clears the failure status, so it does not linger on the
    /// collapsed github.com sign-in screen ("failed/copied message carried back").
    #[test]
    fn esc_collapse_clears_enterprise_failure_status() {
        let mut app = test_app();
        app.mode = AppMode::Auth;
        app.auth = Some(AuthState {
            agent_id: "copilot".into(),
            agent_name: "GitHub Copilot".into(),
            login_command: "copilot login --host https://nope.invalid".into(),
            checking: false,
            status_message: "Login failed: TypeError: fetch failed".into(),
            enterprise_mode: true,
            enterprise_host: "nope.invalid".into(),
        });

        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        assert_eq!(app.mode, AppMode::Auth, "collapse stays on the sign-in screen");
        let auth = app.auth.as_ref().expect("collapse keeps the auth screen");
        assert!(!auth.enterprise_mode, "first Esc collapses the enterprise input");
        assert!(
            auth.status_message.is_empty(),
            "collapsing must clear the enterprise failure status so it does not linger"
        );
    }

    /// Render: the auth screen while checking with a non-empty status message
    /// must paint that message (the `waiting_for_authorization` branch). Covers
    /// `ui/auth.rs` lines 44-60.
    #[test]
    fn render_auth_checking_with_status_message() {
        let mut app = test_app();
        app.mode = AppMode::Auth;
        app.auth = Some(AuthState {
            agent_id: "copilot".into(),
            agent_name: "GitHub Copilot".into(),
            login_command: String::new(),
            checking: true,
            status_message: "AUTH_STATUS_XYZ".into(),
            enterprise_mode: false,
            enterprise_host: String::new(),
        });

        let text = render_to_text(&mut app, 80, 24);
        assert!(
            text.contains("AUTH_STATUS_XYZ"),
            "the auth screen must paint the status message while waiting; rendered:\n{text}"
        );
    }

    /// The GHE sign-in affordance: [E] reveals the domain input, typed chars
    /// edit it (Ctrl-modified keys and whitespace are ignored), Backspace
    /// deletes, and Esc collapses back to the github.com choice WITHOUT leaving
    /// the sign-in screen.
    #[test]
    fn auth_enterprise_domain_entry_via_keys() {
        let mut app = test_app();
        app.mode = AppMode::Auth;
        app.auth = Some(AuthState {
            agent_id: "copilot".into(),
            agent_name: "GitHub Copilot".into(),
            login_command: "copilot login".into(),
            checking: false,
            status_message: String::new(),
            enterprise_mode: false,
            enterprise_host: String::new(),
        });

        // [E] opens the enterprise domain input (it is not typed into the field).
        app.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
        assert!(
            app.auth.as_ref().unwrap().enterprise_mode,
            "E must reveal the domain input"
        );

        // Typed characters edit the domain.
        for c in ['c', 'o', 'r', 'p', '.', 'g', 'h', 'e', '.', 'c', 'o', 'm'] {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        // Ctrl-combinations and whitespace must NOT be typed into the field.
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        assert_eq!(app.auth.as_ref().unwrap().enterprise_host, "corp.ghe.com");

        // Backspace deletes one character.
        app.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(app.auth.as_ref().unwrap().enterprise_host, "corp.ghe.co");

        // Esc collapses the input but stays on the sign-in screen.
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        let auth = app
            .auth
            .as_ref()
            .expect("Esc collapse must not leave the sign-in screen");
        assert!(!auth.enterprise_mode, "Esc must collapse the enterprise input");
        assert_eq!(app.mode, AppMode::Auth, "Esc collapse must stay in Auth mode");
    }

    fn agent_status_for_test(id: &str, display: &str, cli_found: bool) -> crate::agent_check::AgentStatus {
        crate::agent_check::AgentStatus {
            id: id.into(),
            display_name: display.into(),
            cli_found,
            cli_path: None,
            has_credential: false,
            install_hint: String::new(),
            auth_hint: String::new(),
        }
    }

    #[test]
    fn diagnostic_setup_options_route_auth_by_agent() {
        let mut copilot = agent_status_for_test("copilot", "GitHub Copilot", true);
        copilot.has_credential = false;
        let copilot_options = build_setup_options(&SetupReason::AgentError, Some(&copilot));
        assert!(
            matches!(copilot_options.as_slice(), [SetupOption::SignIn { agent_id, .. }] if agent_id == "copilot"),
            "Copilot auth failures must offer the in-app SignIn flow"
        );

        let mut codex = agent_status_for_test("codex", "Codex", true);
        codex.has_credential = false;
        let codex_options = build_setup_options(&SetupReason::AgentError, Some(&codex));
        assert!(
            matches!(codex_options.as_slice(), [SetupOption::Retry]),
            "external-auth agents stay on the diagnostic Retry flow"
        );
    }

    /// Render: a setup screen with a full options list while a winget install
    /// is in progress must paint each option label and the install spinner row.
    /// Covers the `SetupOption` match arms + the install-progress block in
    /// `ui/setup.rs`.
    #[test]
    fn render_setup_options_while_installing() {
        let mut app = test_app();
        app.mode = AppMode::Setup;
        app.setup = Some(SetupState {
            reason: SetupReason::AgentMissing,
            selected_index: 0,
            preflight: PreflightResult::passed_for_custom_agent("custom:x"),
            install_in_progress: true,
            install_log: vec!["WINGET_LOG_XYZ".into()],
            install_error: None,
            options: vec![
                SetupOption::Install {
                    agent_id: "copilot".into(),
                    display_name: "GitHub Copilot".into(),
                },
                SetupOption::SignIn {
                    agent_id: "copilot".into(),
                    display_name: "GitHub Copilot".into(),
                },
                SetupOption::Retry,
            ],
            title: "INSTALLING_TITLE_XYZ".into(),
            subtitle: "sub".into(),
        });

        let text = render_to_text(&mut app, 80, 30);
        assert!(
            text.contains("INSTALLING_TITLE_XYZ"),
            "the setup screen must paint its title; rendered:\n{text}"
        );
        assert!(
            text.contains("WINGET_LOG_XYZ"),
            "the install-in-progress block must paint the winget log tail; rendered:\n{text}"
        );
    }

    /// Render: a setup screen carrying an install error must paint the error
    /// message. Covers the `install_error` branch in `ui/setup.rs` (line 186+).
    #[test]
    fn render_setup_install_error() {
        let mut app = test_app();
        app.mode = AppMode::Setup;
        app.setup = Some(SetupState {
            reason: SetupReason::AgentError,
            selected_index: 0,
            preflight: PreflightResult::passed_for_custom_agent("custom:x"),
            install_in_progress: false,
            install_log: vec!["log-a".into(), "log-b".into()],
            install_error: Some("INSTALL_ERR_XYZ".into()),
            options: vec![SetupOption::Retry],
            title: "err".into(),
            subtitle: "sub".into(),
        });

        let text = render_to_text(&mut app, 80, 30);
        assert!(
            text.contains("INSTALL_ERR_XYZ"),
            "the setup screen must paint the install error; rendered:\n{text}"
        );
    }

    /// Render: a setup screen with a completed-info log (no install running,
    /// no error) must paint the info line. Covers the info-log block in
    /// `ui/setup.rs` (lines 75-85).
    #[test]
    fn render_setup_info_log() {
        let mut app = test_app();
        app.mode = AppMode::Setup;
        app.setup = Some(SetupState {
            reason: SetupReason::AgentError,
            selected_index: 0,
            preflight: PreflightResult::passed_for_custom_agent("custom:x"),
            install_in_progress: false,
            install_log: vec!["INFO_LOG_XYZ".into()],
            install_error: None,
            options: vec![SetupOption::Retry],
            title: "info".into(),
            subtitle: "sub".into(),
        });

        let text = render_to_text(&mut app, 80, 30);
        assert!(
            text.contains("INFO_LOG_XYZ"),
            "the setup screen must paint the completed-info log line; rendered:\n{text}"
        );
    }

    /// Alt+V when the agent did not advertise the `image` prompt capability
    /// must no-op the paste and surface a clear system message rather than
    /// queueing an image the agent would reject.
    #[test]
    fn alt_v_without_image_capability_shows_not_supported_message() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = test_app();
        app.state = ConnectionState::Connected;
        app.agent_supports_image = false;

        app.handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::ALT));

        let want = t!("system.image_not_supported").into_owned();
        let tab = app.current_tab();
        assert!(
            tab.messages
                .iter()
                .any(|m| matches!(m, ChatMessage::System(s) if *s == want)),
            "Alt+V without image capability must push the not-supported message"
        );
        assert!(
            tab.pending_images.is_empty(),
            "no image should be queued when the capability is missing"
        );
    }

    /// Render: queued Alt+V images surface as the input-box title so the user
    /// can see what will be sent.
    #[test]
    fn input_box_titles_queued_images() {
        let mut app = test_app();
        app.state = ConnectionState::Connected;
        app.current_tab_mut()
            .pending_images
            .push(crate::clipboard_image::PastedImage {
                data_base64: "AAA=".into(),
                mime_type: "image/png".into(),
                label: "screenshot".into(),
            });

        let text = render_to_text(&mut app, 80, 30);
        assert!(
            text.contains("screenshot"),
            "the input box must title queued images; rendered:\n{text}"
        );
    }


    /// the action's command body (the card shows the command, not the choice
    /// `title` field, which only surfaces for action-less choices) plus the
    /// run-command button. Lifts `ui/recommendations.rs` (reached only when
    /// `turn.recommendations()` is Some).
    #[test]
    fn render_recommendation_card_shows_command() {
        use crate::coordinator::{RecommendationChoice, RecommendationSet, RecommendedAction};
        let mut app = test_app();
        app.state = ConnectionState::Connected;
        app.current_tab_mut().turn = TurnState::Surfaced {
            prompt: SubmittedPrompt {
                id: 1,
                text: "fix it".into(),
                submitted_at_unix_s: 0.0,
                autofix: None,
            },
            outcome: TurnOutcome::Recommendation(RecommendationSet {
                recommended_choice: Some(0),
                choices: vec![RecommendationChoice {
                    choice: 0,
                    title: "Run the fix".into(),
                    rationale: "because reasons".into(),
                    actions: vec![RecommendedAction::Send {
                        parent: String::new(),
                        input: "echo REC_CMD_XYZ".into(),
                    }],
                }],
            }),
            end_pending: false,
        };

        let text = render_to_text(&mut app, 80, 40);
        assert!(
            text.contains("REC_CMD_XYZ"),
            "the recommendation card must paint its command body; rendered:\n{text}"
        );
        let run_btn = t!("recommendations.button_run_command").into_owned();
        let probe: String = run_btn.chars().take(4).collect();
        assert!(
            !probe.trim().is_empty() && text.contains(&probe),
            "the recommendation card must paint the run-command button ({run_btn:?}); rendered:\n{text}"
        );
    }

    /// Render: every `ChatMessage` variant must paint without panicking and
    /// surface its distinguishing text. Lifts the `build_message_lines` /
    /// `message_height` match arms in `ui/chat.rs` (User/System/Plan/Error/
    /// AgentEvent/Disclaimer were previously unexercised).
    #[test]
    fn render_chat_all_message_variants() {
        let mut app = test_app();
        app.state = ConnectionState::Connected;
        {
            let tab = app.current_tab_mut();
            tab.messages.push(ChatMessage::User("USER_MSG_XYZ".into()));
            tab.messages.push(ChatMessage::Agent("AGENT_MSG_XYZ".into()));
            tab.messages.push(ChatMessage::System("SYSTEM_MSG_XYZ".into()));
            tab.messages.push(ChatMessage::Error("ERROR_MSG_XYZ".into()));
            tab.messages
                .push(ChatMessage::AgentEvent("AGENT_EVENT_MSG_XYZ".into()));
            tab.messages.push(ChatMessage::Plan(vec![
                PlanEntry {
                    content: "PLAN_DONE_XYZ".into(),
                    status: PlanEntryStatus::Completed,
                },
                PlanEntry {
                    content: "PLAN_DOING_XYZ".into(),
                    status: PlanEntryStatus::InProgress,
                },
                PlanEntry {
                    content: "PLAN_TODO_XYZ".into(),
                    status: PlanEntryStatus::Pending,
                },
            ]));
            tab.messages.push(ChatMessage::Disclaimer);
        }

        let text = render_to_text(&mut app, 80, 40);
        for needle in [
            "USER_MSG_XYZ",
            "AGENT_MSG_XYZ",
            "SYSTEM_MSG_XYZ",
            "ERROR_MSG_XYZ",
            "AGENT_EVENT_MSG_XYZ",
            "PLAN_DONE_XYZ",
            "PLAN_DOING_XYZ",
            "PLAN_TODO_XYZ",
        ] {
            assert!(
                text.contains(needle),
                "chat must paint {needle:?}; rendered:\n{text}"
            );
        }
    }

    /// Render: an expanded completed turn with a trailing marker must paint
    /// its prompt header, its detail rows, and the marker. Lifts
    /// `build_completed_turn_lines` in `ui/chat.rs`.
    #[test]
    fn render_chat_completed_turn_expanded_with_marker() {
        let mut app = test_app();
        app.state = ConnectionState::Connected;
        app.current_tab_mut().completed_turns.push(CompletedTurn {
            prompt: "TURN_PROMPT_XYZ".into(),
            details: vec![ChatMessage::Agent("TURN_DETAIL_XYZ".into())],
            expanded: true,
            trailing_marker: Some("TURN_MARKER_XYZ".into()),
        });

        let text = render_to_text(&mut app, 80, 40);
        for needle in ["TURN_PROMPT_XYZ", "TURN_DETAIL_XYZ", "TURN_MARKER_XYZ"] {
            assert!(
                text.contains(needle),
                "expanded completed turn must paint {needle:?}; rendered:\n{text}"
            );
        }
    }

    /// Render: while the helper is still connecting, the chat must paint the
    /// animated "Connecting…" activity line. Lifts the `Connecting` branch of
    /// `build_activity_line` in `ui/chat.rs`.
    #[test]
    fn render_chat_connecting_activity_line() {
        let mut app = test_app();
        app.state = ConnectionState::Connecting("starting".into());

        let text = render_to_text(&mut app, 80, 24);
        let label = t!("connection.connecting_activity").into_owned();
        let probe: String = label.chars().take(6).collect();
        assert!(
            !probe.trim().is_empty() && text.contains(&probe),
            "chat must paint the connecting activity line ({label:?}); rendered:\n{text}"
        );
    }

    /// Render: the first-run welcome hint must paint its title when connected
    /// and `show_welcome_hint` is set. Lifts the welcome branch of
    /// `ui/chat.rs` + `ui/layout.rs`.
    #[test]
    fn render_chat_welcome_hint() {
        let mut app = test_app();
        app.state = ConnectionState::Connected;
        app.show_welcome_hint = true;

        let text = render_to_text(&mut app, 80, 24);
        let title = t!("chat.welcome_title").into_owned();
        let probe: String = title.chars().take(6).collect();
        assert!(
            !probe.trim().is_empty() && text.contains(&probe),
            "chat must paint the welcome title ({title:?}); rendered:\n{text}"
        );
    }

    /// Render: when the pane is too short for a full permission card, the
    /// compact one-row fallback must paint the description and the `[Y/N]`
    /// hint. Lifts `render_compact` in `ui/permission.rs`. The compact path
    /// is gated on `terminal_rows - 3 < CARD_MIN_SIZE`.
    #[test]
    fn render_permission_compact_shows_hint() {
        let mut app = test_app();
        app.state = ConnectionState::Connected;
        app.terminal_rows = 7; // ceiling = 4 < CARD_MIN_SIZE(5) → compact fallback
        app.current_tab_mut().permission.push_back(PermissionState {
            description: "Run: echo PERM_COMPACT_XYZ".into(),
            options: vec![
                PermOption {
                    id: "allow-once".into(),
                    name: "Allow once".into(),
                    kind: "AllowOnce".into(),
                },
                PermOption {
                    id: "reject-once".into(),
                    name: "Reject".into(),
                    kind: "RejectOnce".into(),
                },
            ],
            selected: 0,
            responder: None,
        });

        let text = render_to_text(&mut app, 80, 24);
        assert!(
            text.contains("PERM_COMPACT_XYZ"),
            "the compact permission row must paint its description; rendered:\n{text}"
        );
        assert!(
            text.contains("Y/N"),
            "the compact permission row must paint the [Y/N] hint; rendered:\n{text}"
        );
    }

    fn submit_autofix_prompt(app: &mut App, pane: &str) {
        let gen = {
            let tab = app.tab_mut(DEFAULT_TAB_ID);
            tab.autofix.generation = tab.autofix.generation.wrapping_add(1);
            tab.autofix.pane_id = Some(pane.into());
            tab.autofix.generation
        };
        let prompt = SubmittedPrompt {
            id: 99,
            text: "diagnose this".into(),
            submitted_at_unix_s: 0.0,
            autofix: Some(AutofixContext {
                target_pane_id: pane.into(),
                generation: gen,
            }),
        };
        app.turn_submit_prompt(DEFAULT_TAB_ID, prompt);
    }

    /// Submit a manual-`/fix`-style autofix turn: an autofix context whose
    /// `target_pane_id` is empty (the App doesn't know the working pane until
    /// the client task resolves it and plumbs it back).
    fn submit_fix_prompt(app: &mut App, id: u64) {
        let gen = {
            let tab = app.tab_mut(DEFAULT_TAB_ID);
            tab.autofix.generation = tab.autofix.generation.wrapping_add(1);
            tab.autofix.generation
        };
        let prompt = SubmittedPrompt {
            id,
            text: String::new(),
            submitted_at_unix_s: 0.0,
            autofix: Some(AutofixContext {
                target_pane_id: String::new(),
                generation: gen,
            }),
        };
        app.turn_submit_prompt(DEFAULT_TAB_ID, prompt);
    }

    fn fix_target_pane(app: &App) -> String {
        app.current_tab()
            .turn
            .prompt()
            .unwrap()
            .autofix
            .as_ref()
            .unwrap()
            .target_pane_id
            .clone()
    }

    #[test]
    fn fix_target_pane_is_late_bound_by_prompt_id() {
        let mut app = test_app();
        submit_fix_prompt(&mut app, 42);
        assert_eq!(fix_target_pane(&app), "", "starts unbound");

        // A resolution for a different prompt id (a superseded /fix) is ignored.
        app.apply_autofix_target_resolved(Some(DEFAULT_TAB_ID.into()), 7, "pane-X".into());
        assert_eq!(fix_target_pane(&app), "", "stale prompt_id must not patch");

        // An empty pane id is a no-op.
        app.apply_autofix_target_resolved(Some(DEFAULT_TAB_ID.into()), 42, String::new());
        assert_eq!(fix_target_pane(&app), "", "empty pane id is ignored");

        // The matching prompt id binds the resolved working pane.
        app.apply_autofix_target_resolved(Some(DEFAULT_TAB_ID.into()), 42, "pane-7".into());
        assert_eq!(fix_target_pane(&app), "pane-7", "matching id binds the pane");
    }

    #[test]
    fn submit_clears_messages_and_pushes_user_bubble() {
        let mut app = test_app();
        app.current_tab_mut()
            .messages
            .push(ChatMessage::System("stale".into()));
        submit_test_prompt(&mut app, "hello");
        let tab = app.current_tab();
        assert!(matches!(tab.turn, TurnState::Submitted(_)));
        assert!(
            !tab.turn.accepts_new_prompt(),
            "Submitted blocks new prompts"
        );
        assert_eq!(tab.messages.len(), 1, "stale System bubble was cleared");
        assert!(matches!(tab.messages[0], ChatMessage::User(ref t) if t == "hello"));
    }

    #[test]
    fn first_message_chunk_transitions_to_streaming_with_buf() {
        let mut app = test_app();
        submit_test_prompt(&mut app, "hi");
        let advanced = app.turn_observe_chunk(DEFAULT_TAB_ID, ChunkKind::Message, "partial");
        assert!(advanced, "first message chunk must advance the buffer");
        let tab = app.current_tab();
        assert_eq!(tab.turn.buffer(), Some("partial"));
        assert!(tab.turn.is_streaming());
    }

    #[test]
    fn thought_chunk_first_transitions_with_empty_buf() {
        let mut app = test_app();
        submit_test_prompt(&mut app, "hi");
        let advanced = app.turn_observe_chunk(DEFAULT_TAB_ID, ChunkKind::Thought, "thinking…");
        assert!(!advanced, "thought chunks never advance the buffer");
        let tab = app.current_tab();
        assert!(tab.turn.is_streaming());
        assert_eq!(tab.turn.buffer(), Some(""));
    }

    #[test]
    fn end_with_no_eager_chat_fallback_commits_completed_turn() {
        let mut app = test_app();
        submit_test_prompt(&mut app, "why blue?");
        // Pure prose — won't parse as a RecommendationSet, falls to chat.
        app.turn_observe_chunk(
            DEFAULT_TAB_ID,
            ChunkKind::Message,
            "Light scatters in the atmosphere.",
        );
        app.turn_close(DEFAULT_TAB_ID);
        let tab = app.current_tab();
        assert!(
            matches!(
                tab.turn,
                TurnState::Surfaced {
                    outcome: TurnOutcome::ChatTurn,
                    end_pending: false,
                    ..
                }
            ),
            "got {:?}",
            tab.turn
        );
        assert!(
            tab.turn.accepts_new_prompt(),
            "chat fallback unblocks input"
        );
        assert_eq!(tab.completed_turns.len(), 1);
        assert_eq!(tab.completed_turns[0].prompt, "why blue?");
    }

    #[test]
    fn end_with_no_chunks_clears_autofix_bottom_bar() {
        let mut app = test_app();
        submit_autofix_prompt(&mut app, "pane-7");
        assert!(app.tab_mut(DEFAULT_TAB_ID).autofix.pane_id.is_some());
        // No chunks arrived; AgentMessageEnd fires.
        app.turn_close(DEFAULT_TAB_ID);
        let tab = app.current_tab();
        assert!(
            matches!(
                tab.turn,
                TurnState::Surfaced {
                    outcome: TurnOutcome::Empty,
                    end_pending: false,
                    ..
                }
            ),
            "got {:?}",
            tab.turn
        );
        assert!(
            app.tab_mut(DEFAULT_TAB_ID).autofix.pane_id.is_none(),
            "autofix.pane_id must be cleared so the bar leaves Pending"
        );
    }

    #[test]
    fn stale_autofix_chunks_dropped_when_generation_diverges() {
        let mut app = test_app();
        submit_autofix_prompt(&mut app, "pane-1");
        // Simulate an Esc cancel or a newer trigger bumping the counter
        // on the same tab as the in-flight prompt.
        {
            let tab = app.tab_mut(DEFAULT_TAB_ID);
            tab.autofix.generation = tab.autofix.generation.wrapping_add(1);
        }
        let advanced = app.turn_observe_chunk(DEFAULT_TAB_ID, ChunkKind::Message, "stale");
        assert!(!advanced, "stale-gen chunks must be dropped");
        let tab = app.current_tab();
        assert!(
            matches!(tab.turn, TurnState::Submitted(_)),
            "state unchanged on stale drop, got {:?}",
            tab.turn
        );
        assert_eq!(tab.turn.buffer(), None);
    }

    #[test]
    fn stale_autofix_at_close_resets_to_idle() {
        let mut app = test_app();
        submit_autofix_prompt(&mut app, "pane-1");
        // A chunk advances state to Streaming.
        app.turn_observe_chunk(DEFAULT_TAB_ID, ChunkKind::Message, "partial");
        // Generation diverges (newer trigger / Esc).
        {
            let tab = app.tab_mut(DEFAULT_TAB_ID);
            tab.autofix.generation = tab.autofix.generation.wrapping_add(1);
        }
        app.turn_close(DEFAULT_TAB_ID);
        assert!(
            app.current_tab().turn.is_idle(),
            "stale-close must reset to Idle, got {:?}",
            app.current_tab().turn
        );
    }

    #[test]
    fn cancel_bumps_generation_and_returns_to_idle() {
        let mut app = test_app();
        submit_autofix_prompt(&mut app, "pane-1");
        let gen_before = app.tab_mut(DEFAULT_TAB_ID).autofix.generation;
        app.turn_cancel(DEFAULT_TAB_ID);
        assert_eq!(
            app.tab_mut(DEFAULT_TAB_ID).autofix.generation,
            gen_before.wrapping_add(1)
        );
        assert!(app.current_tab().turn.is_idle());
        assert!(app.tab_mut(DEFAULT_TAB_ID).autofix.pane_id.is_none());
    }

    #[test]
    fn cancel_mid_stream_preserves_visible_prose_with_canceled_marker() {
        // Esc while prose is streaming → commit partial prose as a
        // CompletedTurn (default-expanded) with the trailing_marker set
        // so the user sees what arrived and that they cancelled it.
        let mut app = test_app();
        submit_test_prompt(&mut app, "tell me a story");
        app.turn_observe_chunk(DEFAULT_TAB_ID, ChunkKind::Message, "\n\nOnce upon a time");
        app.turn_cancel(DEFAULT_TAB_ID);
        let tab = app.current_tab();
        assert!(tab.turn.is_idle(), "got {:?}", tab.turn);
        assert_eq!(tab.completed_turns.len(), 1);
        let committed = &tab.completed_turns[0];
        assert_eq!(committed.prompt, "tell me a story");
        assert!(
            committed.expanded,
            "cancel-committed turns default expanded"
        );
        assert!(committed
            .details
            .iter()
            .any(|m| matches!(m, ChatMessage::Agent(t) if t.contains("Once upon a time"))));
        assert!(
            committed
                .trailing_marker
                .as_deref()
                .map_or(false, |m| m.contains("canceled")),
            "trailing_marker should hold (canceled), got {:?}",
            committed.trailing_marker
        );
        assert!(tab.messages.is_empty(), "messages cleared on cancel");
        assert!(tab.tool_calls.is_empty(), "tool_calls cleared on cancel");
    }

    #[test]
    fn cancel_mid_stream_records_canceled_marker_even_without_visible_prose() {
        // A buffer that's pure JSON (no `explanation` field, no prose
        // prefix) renders as nothing during streaming. We must NOT commit
        // raw JSON as agent prose, but we still record a completed_turn
        // with the canceled marker so the user knows the prompt was sent
        // and cancelled.
        let mut app = test_app();
        submit_test_prompt(&mut app, "kill pid 1234");
        app.turn_observe_chunk(
            DEFAULT_TAB_ID,
            ChunkKind::Message,
            r#"{"recommended_choice":1,"choices":[{"choice":1,"#,
        );
        app.turn_cancel(DEFAULT_TAB_ID);
        let tab = app.current_tab();
        assert!(tab.turn.is_idle());
        assert_eq!(tab.completed_turns.len(), 1);
        let committed = &tab.completed_turns[0];
        assert_eq!(committed.prompt, "kill pid 1234");
        assert!(
            !committed
                .details
                .iter()
                .any(|m| matches!(m, ChatMessage::Agent(_))),
            "JSON-only buffer must not be committed as agent prose"
        );
        assert!(
            committed
                .trailing_marker
                .as_deref()
                .map_or(false, |m| m.contains("canceled")),
            "trailing_marker should hold (canceled), got {:?}",
            committed.trailing_marker
        );
        assert!(tab.messages.is_empty());
        assert!(tab.tool_calls.is_empty());
    }

    #[test]
    fn end_pending_blocks_new_prompts_until_message_end() {
        // Eager-surface path: user submits → JSON streams → recommendation
        // surfaces before AgentMessageEnd. While end_pending=true the UI
        // gate must hold. AgentMessageEnd then releases it.
        let mut app = test_app();
        submit_test_prompt(&mut app, "first");
        // RecommendationSet shape that survives `validate_recommendation_set`.
        let json = r#"```json
{"recommended_choice":1,"choices":[{"choice":1,"title":"do it","rationale":"r","actions":[{"type":"send","parent":"pane-X","input":"ls"}]}]}
```"#;
        app.turn_observe_chunk(DEFAULT_TAB_ID, ChunkKind::Message, json);
        app.turn_try_eager_surface(DEFAULT_TAB_ID);
        let tab = app.current_tab();
        assert!(
            matches!(
                tab.turn,
                TurnState::Surfaced {
                    outcome: TurnOutcome::Recommendation(_),
                    end_pending: true,
                    ..
                }
            ),
            "expected eager surface, got {:?}",
            tab.turn
        );
        assert!(
            !tab.turn.accepts_new_prompt(),
            "end_pending=true must hold the UI gate"
        );
        // AgentMessageEnd flips end_pending=false.
        app.turn_close(DEFAULT_TAB_ID);
        assert!(app.current_tab().turn.accepts_new_prompt());
    }

    // ─── card / panel height math ───────────────────────────────────────────

    use crate::app::turn_state::{SubmittedPrompt, TurnOutcome, TurnState};
    use crate::coordinator::{
        OpenTarget, RecommendationChoice, RecommendationSet, RecommendedAction,
    };
    use crate::ui::card::{card_content_width, CARD_H_CHROME, CARD_MIN_SIZE};

    fn perm_with(desc: &str) -> PermissionState {
        PermissionState {
            description: desc.to_string(),
            options: vec![PermOption {
                id: "allow_once".into(),
                name: "Allow".into(),
                kind: "allow_once".into(),
            }],
            selected: 0,
            responder: None,
        }
    }

    fn rec_send(input: &str) -> RecommendationChoice {
        RecommendationChoice {
            choice: 0,
            title: "t".into(),
            rationale: String::new(),
            actions: vec![RecommendedAction::Send {
                parent: String::new(),
                input: input.into(),
            }],
        }
    }

    fn install_recs(app: &mut App, choices: Vec<RecommendationChoice>) {
        let tab = app.current_tab_mut();
        tab.turn = TurnState::Surfaced {
            prompt: SubmittedPrompt {
                id: 1,
                text: "p".into(),
                submitted_at_unix_s: 0.0,
                autofix: None,
            },
            outcome: TurnOutcome::Recommendation(RecommendationSet {
                recommended_choice: Some(0),
                choices,
            }),
            end_pending: false,
        };
    }

    #[test]
    fn card_content_width_subtracts_chrome_and_floors_at_1() {
        assert_eq!(card_content_width(80), 80 - CARD_H_CHROME as usize);
        assert_eq!(card_content_width(CARD_H_CHROME + 1), 1);
        assert_eq!(card_content_width(CARD_H_CHROME), 1);
        assert_eq!(card_content_width(0), 1);
    }

    #[test]
    fn permission_card_height_single_line_is_card_min() {
        let perm = perm_with("ok");
        assert_eq!(permission_card_height(&perm, 80) as u16, CARD_MIN_SIZE);
    }

    #[test]
    fn permission_card_height_counts_wrap_at_actual_panel_width() {
        let perm = perm_with(&"a".repeat(200));
        // Full-width terminal: wrap at 80 - 8 = 72.
        let inner_full = 80 - CARD_H_CHROME as usize;
        assert_eq!(
            permission_card_height(&perm, 80),
            CARD_MIN_SIZE as usize + 200_usize.div_ceil(inner_full) - 1
        );
        // Debug panel open: 60% of 80 = 48 → wrap at 40.
        let inner_split = 48 - CARD_H_CHROME as usize;
        assert_eq!(
            permission_card_height(&perm, 48),
            CARD_MIN_SIZE as usize + 200_usize.div_ceil(inner_split) - 1
        );
        // The two should differ — proves the panel_width input matters
        // (the PR #20 reviewer-3 bug).
        assert_ne!(
            permission_card_height(&perm, 80),
            permission_card_height(&perm, 48)
        );
    }

    #[test]
    fn permission_card_height_treats_blank_lines_as_one_row() {
        let perm = perm_with("line1\n\nline2");
        // 3 logical lines (blank counts as 1).
        assert_eq!(
            permission_card_height(&perm, 80),
            CARD_MIN_SIZE as usize + 2
        );
    }

    #[test]
    fn rec_card_height_includes_inter_card_gap() {
        let h = rec_card_height(&rec_send("ls"), 80);
        assert_eq!(h as u16, CARD_MIN_SIZE + 1);
    }

    #[test]
    fn rec_card_height_handles_open_action_synthesis() {
        let choice = RecommendationChoice {
            choice: 0,
            title: "t".into(),
            rationale: String::new(),
            actions: vec![RecommendedAction::Open {
                target: OpenTarget::Tab,
                parent: None,
                cwd: Some("C:/repo".into()),
                title: Some("logs".into()),
                direction: None,
                profile: None,
            }],
        };
        let h = rec_card_height(&choice, 80);
        // "New tab (logs) in C:/repo" fits on one row at width 72.
        assert_eq!(h as u16, CARD_MIN_SIZE + 1);
    }

    #[test]
    fn permission_panel_height_zero_when_no_permission() {
        let mut app = test_app();
        app.terminal_rows = 30;
        assert_eq!(app.permission_panel_height(80), 0);
    }

    #[test]
    fn permission_panel_height_falls_back_to_compact_below_card_min() {
        let mut app = test_app();
        app.terminal_rows = 7; // ceiling = 7-3 = 4 < CARD_MIN_SIZE
        app.current_tab_mut().permission.push_back(perm_with("ok"));
        // Must stay visible — agent flow blocks on this prompt. 1-row strip
        // is the compact fallback rendered by `ui::permission::render`.
        assert_eq!(app.permission_panel_height(80), 1);
    }

    #[test]
    fn permission_panel_height_admits_at_card_min_ceiling() {
        let mut app = test_app();
        app.terminal_rows = 8; // ceiling = 5 == CARD_MIN_SIZE
        app.current_tab_mut().permission.push_back(perm_with("ok"));
        assert_eq!(app.permission_panel_height(80), CARD_MIN_SIZE);
    }

    #[test]
    fn rec_panel_height_floor_lets_tallest_card_render() {
        let mut app = test_app();
        app.terminal_rows = 20;
        let tall = "x".repeat(500);
        install_recs(&mut app, vec![rec_send(&tall)]);
        let tall_h = rec_card_height(
            &app.current_tab().turn.recommendations().unwrap().choices[0],
            80,
        ) as u16;
        // ceiling = 20 - 5 = 15; tall card is much larger; floor wins.
        assert_eq!(app.rec_panel_height(80), tall_h);
    }

    #[test]
    fn rec_panel_height_caps_at_ceiling_when_total_exceeds() {
        let mut app = test_app();
        app.terminal_rows = 30;
        // Three short cards, each h=6 → total 18; ceiling 30-5=25.
        install_recs(&mut app, vec![rec_send("a"), rec_send("b"), rec_send("c")]);
        assert_eq!(app.rec_panel_height(80), 18);
    }

    #[test]
    fn rec_panel_height_zero_when_no_recs() {
        let app = test_app();
        assert_eq!(app.rec_panel_height(80), 0);
    }

    #[test]
    fn main_area_width_reflects_debug_panel_split() {
        let mut app = test_app();
        app.terminal_cols = 100;
        assert_eq!(app.main_area_width(), 100);
        app.show_debug_panel = true;
        assert_eq!(app.main_area_width(), 60);
    }

    /// Regression: `ui::recommendations::render` used `area.width` (= `h_rec[1]`
    /// = `main_area.width - 2`) when calling `rec_card_height`, while
    /// `rec_panel_height` / `sync_rec_scroll_max` used `main_area.width`. The
    /// 2-cell desync clipped the bottom card and undercounted scroll bounds
    /// whenever a card's wrap row count differed between the two widths.
    ///
    /// This test pins both code paths to `main_area.width`, and picks a
    /// text length that lies in the critical window `(W-10, W-8]` so the
    /// old buggy width (`W-2`, content `W-10`) would wrap to a different
    /// row count than the correct width (`W`, content `W-8`).
    #[test]
    fn rec_card_height_matches_predict_and_render_paths() {
        let w: u16 = 50;
        // text length 42 sits exactly at the boundary: fits on 1 row at
        // inner_width 42 (W=50, chrome=8), but spills to 2 rows at
        // inner_width 40 (the old buggy basis).
        let text = "a".repeat(42);
        let choice = rec_send(&text);
        let mut app = test_app();
        app.terminal_cols = w;
        app.terminal_rows = 30;
        install_recs(&mut app, vec![choice.clone()]);

        let predict = app.rec_panel_height(app.main_area_width()) as usize;
        // Same width the renderer now uses (`app.main_area_width()`).
        let render = rec_card_height(&choice, app.main_area_width());
        assert_eq!(predict, render);

        // Sanity: confirm the chosen text *is* a sensitive input — i.e. the
        // old buggy basis (h_rec[1] width = W-2) would have produced a
        // different height. If this ever fails the test no longer guards
        // the regression.
        let buggy = rec_card_height(&choice, app.main_area_width() - 2);
        assert_ne!(
            render, buggy,
            "text length 42 should wrap differently at width 50 vs 48 — \
             pick a different critical input"
        );
    }

    // ─── Up/Down chat-scroll fallback ──────────────────────────────────
    //
    // After dropping crossterm mouse capture (so users can drag-select &
    // copy text in the agent pane), wheel scrolling relies on the host
    // terminal translating wheel notches into Up/Down arrow keystrokes
    // while in the alt-screen buffer. The fallback in `handle_key`
    // forwards those arrows to `chat_scroll.by(±1)` ONLY when none of
    // the existing arrow-key consumers is active:
    //   * input box is empty
    //   * no recommendation card is shown
    //   * slash-command popup is not visible
    // These tests pin down each branch.

    #[test]
    fn up_arrow_scrolls_chat_when_input_empty_no_recs_no_popup() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = test_app();
        // Fresh chat tab — Idle turn, empty input, no popup candidates.
        assert!(app.current_tab().input.is_empty());
        assert!(app.current_tab().turn.recommendations().is_none());
        assert!(!app.command_popup_visible());
        assert_eq!(app.current_tab().chat_scroll.offset, 0);

        app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(
            app.current_tab().chat_scroll.offset,
            1,
            "↑ on empty input should scroll chat up by one line",
        );
        app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.current_tab().chat_scroll.offset, 2);
    }

    #[test]
    fn down_arrow_scrolls_chat_when_input_empty_no_recs_no_popup() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = test_app();
        // Prime scroll offset so Down has somewhere to go (saturating sub
        // would otherwise leave it at 0 and the assert couldn't tell the
        // arm from a no-op).
        app.current_tab_mut().chat_scroll.offset = 5;

        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(
            app.current_tab().chat_scroll.offset,
            4,
            "↓ on empty input should scroll chat down by one line",
        );
    }

    #[test]
    fn up_down_does_not_scroll_chat_when_input_non_empty() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = test_app();
        // Non-empty input: arrows belong to the input-box editor, not
        // the chat-scroll fallback.
        app.current_tab_mut().input.push_str("hi");
        app.current_tab_mut().cursor_pos = 2;
        app.current_tab_mut().chat_scroll.offset = 3;

        app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(
            app.current_tab().chat_scroll.offset,
            3,
            "non-empty input must NOT trigger the chat-scroll fallback",
        );
    }

    #[test]
    fn recommendation_card_keeps_focus_when_input_has_draft() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = test_app();
        stage_surfaced_recommendation(
            &mut app,
            vec![send_choice("pane-A", "ls"), send_choice("pane-B", "pwd")],
            0,
            None,
        );
        app.current_tab_mut().input = "draft".into();
        app.current_tab_mut().cursor_pos = "draft".len();
        app.current_tab_mut().chat_scroll.offset = 7;

        assert!(
            !app.current_tab().input_has_nav_focus(),
            "a visible card owns focus even when the input keeps draft text",
        );

        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.current_tab().selected_recommendation, 1);
        assert_eq!(app.current_tab().input, "draft");
        assert_eq!(
            app.current_tab().chat_scroll.offset,
            7,
            "card navigation must not fall through to chat scrolling",
        );

        app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.current_tab().selected_recommendation, 0);

        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(app.current_tab().selected_button, 1);
        app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(app.current_tab().selected_button, 0);

        app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert_eq!(
            app.current_tab().input,
            "draft",
            "typing stays locked while the card owns focus",
        );
    }

    #[test]
    fn recommendation_card_enter_wins_over_draft_input() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = test_app();
        app.state = ConnectionState::Connected;
        app.current_tab_mut().session_id = Some(DEFAULT_TAB_ID.into());
        stage_surfaced_recommendation(&mut app, vec![send_choice("pane-A", "ls")], 0, None);
        app.current_tab_mut().input = "/help".into();
        app.current_tab_mut().cursor_pos = "/help".len();

        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(
            app.current_tab().input,
            "/help",
            "executing the card must preserve the user's draft",
        );
        assert!(
            app.current_tab().turn.recommendations().is_none(),
            "Enter should execute the visible card, not submit or slash-parse the draft",
        );
        assert!(
            !app.help_overlay_visible,
            "draft slash commands must not run while a recommendation card owns focus",
        );
    }

    #[test]
    fn typing_is_ignored_while_a_past_turn_is_selected() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = test_app();
        app.current_tab_mut().completed_turns.push(CompletedTurn {
            prompt: "old prompt".into(),
            details: Vec::new(),
            expanded: false,
            trailing_marker: None,
        });
        // Highlight the past turn, as Tab would.
        app.current_tab_mut().selected_completed_turn_idx = Some(0);
        assert!(!app.current_tab().input_has_nav_focus());

        app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));

        assert!(
            app.current_tab().input.is_empty(),
            "typing must be ignored while a past turn is highlighted (input locked)",
        );
        assert_eq!(
            app.current_tab().selected_completed_turn_idx,
            Some(0),
            "selection must survive the keystroke so Tab/↑ history nav keeps working",
        );
    }

    #[test]
    fn typing_returns_to_input_after_clearing_selection() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = test_app();
        app.current_tab_mut().completed_turns.push(CompletedTurn {
            prompt: "old prompt".into(),
            details: Vec::new(),
            expanded: false,
            trailing_marker: None,
        });
        app.current_tab_mut().selected_completed_turn_idx = Some(0);

        // Esc backs out of history nav, then typing lands in the input again.
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.current_tab().selected_completed_turn_idx, None);
        app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert_eq!(app.current_tab().input, "x");
    }

    #[test]
    fn up_down_does_not_scroll_chat_when_command_popup_visible() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = test_app();
        // Open the slash-command popup the same way real input would:
        // type "/" and let refresh_command_popup populate candidates.
        app.current_tab_mut().input.push('/');
        app.current_tab_mut().cursor_pos = 1;
        app.current_tab_mut().refresh_command_popup();
        assert!(
            app.command_popup_visible(),
            "test prerequisite: command popup must be visible after typing '/'",
        );
        // Force-clear input WITHOUT calling refresh_command_popup so the
        // candidates list stays populated while input becomes empty. This
        // isolates the !command_popup_visible() guard as the one being
        // tested — without this step, the input-empty guard would
        // independently suppress the fallback and the assertion below
        // could not tell which guard fired.
        app.current_tab_mut().input.clear();
        app.current_tab_mut().cursor_pos = 0;
        assert!(
            app.current_tab().input.is_empty(),
            "test prerequisite: input must be empty so only the popup guard is exercised",
        );
        assert!(
            app.command_popup_visible(),
            "test prerequisite: popup must remain visible after clearing input",
        );
        app.current_tab_mut().chat_scroll.offset = 7;

        app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(
            app.current_tab().chat_scroll.offset,
            7,
            "command popup visibility must suppress the chat-scroll fallback",
        );
    }

    // ─── compute_chip_card_target ───────────────────────────────────────────

    /// Stage a tab into `Surfaced { Recommendation(...) }` with the given
    /// choices and selected index. Mirrors the side-effects the real
    /// `turn_surface_recommendation` would have but skips all the
    /// chat-history / scroll bookkeeping so the resulting state stays
    /// minimal for the chip-target calculator.
    fn stage_surfaced_recommendation(
        app: &mut App,
        choices: Vec<crate::coordinator::RecommendationChoice>,
        selected: usize,
        autofix_target: Option<&str>,
    ) {
        let prompt = SubmittedPrompt {
            id: 1,
            text: "p".into(),
            submitted_at_unix_s: 0.0,
            autofix: autofix_target.map(|t| AutofixContext {
                target_pane_id: t.into(),
                generation: 0,
            }),
        };
        let recs = crate::coordinator::RecommendationSet {
            recommended_choice: Some(selected),
            choices,
        };
        let tab = app.tab_mut(DEFAULT_TAB_ID);
        tab.selected_recommendation = selected;
        tab.turn = TurnState::Surfaced {
            prompt,
            outcome: TurnOutcome::Recommendation(recs),
            end_pending: false,
        };
    }

    fn send_choice(parent: &str, input: &str) -> crate::coordinator::RecommendationChoice {
        crate::coordinator::RecommendationChoice {
            choice: 1,
            title: "Run".into(),
            rationale: String::new(),
            actions: vec![crate::coordinator::RecommendedAction::Send {
                parent: parent.into(),
                input: input.into(),
            }],
        }
    }

    fn open_choice() -> crate::coordinator::RecommendationChoice {
        crate::coordinator::RecommendationChoice {
            choice: 2,
            title: "Open".into(),
            rationale: String::new(),
            actions: vec![crate::coordinator::RecommendedAction::Open {
                target: crate::coordinator::OpenTarget::Tab,
                parent: None,
                cwd: None,
                title: None,
                direction: None,
                profile: None,
            }],
        }
    }

    #[test]
    fn chip_target_returns_none_when_idle() {
        let app = test_app();
        assert_eq!(app.current_tab().compute_chip_card_target(), None);
    }

    #[test]
    fn chip_target_uses_send_parent_when_set() {
        let mut app = test_app();
        stage_surfaced_recommendation(
            &mut app,
            vec![send_choice("pane-A", "ls")],
            0,
            None,
        );
        assert_eq!(
            app.current_tab().compute_chip_card_target(),
            Some("pane-A".to_string()),
        );
    }

    #[test]
    fn chip_target_falls_back_to_autofix_target_when_send_parent_empty() {
        let mut app = test_app();
        // Planner-emitted Send actions in autofix turns leave `parent`
        // blank — `turn_execute_card` fills it from `target_pane_id` at
        // execute time. The chip should already point there now.
        stage_surfaced_recommendation(
            &mut app,
            vec![send_choice("", "fix --auto")],
            0,
            Some("pane-failing"),
        );
        assert_eq!(
            app.current_tab().compute_chip_card_target(),
            Some("pane-failing".to_string()),
        );
    }

    #[test]
    fn chip_target_filters_empty_autofix_target() {
        // C++ treats `pane_session_id == ""` as "no override", so emitting
        // Some("") would let the helper's dedupe believe it pinned the chip
        // while WT silently ignores the event.
        let mut app = test_app();
        stage_surfaced_recommendation(
            &mut app,
            vec![send_choice("", "fix")],
            0,
            Some(""),
        );
        assert_eq!(app.current_tab().compute_chip_card_target(), None);
    }

    #[test]
    fn chip_target_is_none_for_non_send_card() {
        let mut app = test_app();
        stage_surfaced_recommendation(&mut app, vec![open_choice()], 0, None);
        assert_eq!(app.current_tab().compute_chip_card_target(), None);
    }

    #[test]
    fn chip_target_tracks_selected_index() {
        let mut app = test_app();
        stage_surfaced_recommendation(
            &mut app,
            vec![send_choice("pane-A", "ls"), send_choice("pane-B", "pwd")],
            0,
            None,
        );
        assert_eq!(
            app.current_tab().compute_chip_card_target(),
            Some("pane-A".to_string()),
        );
        app.current_tab_mut().selected_recommendation = 1;
        assert_eq!(
            app.current_tab().compute_chip_card_target(),
            Some("pane-B".to_string()),
        );
    }

    #[test]
    fn chip_recompute_dedupes_and_releases_on_idle() {
        // After surfacing a Send card, recompute should record an override.
        // Transitioning back to Idle (here: clear the recs) should make
        // the next recompute observe a different value and clear the
        // last_emitted slot.
        let mut app = test_app();
        stage_surfaced_recommendation(
            &mut app,
            vec![send_choice("pane-A", "ls")],
            0,
            None,
        );
        app.recompute_chip_override(DEFAULT_TAB_ID);
        assert_eq!(
            app.tab_mut(DEFAULT_TAB_ID).last_emitted_chip_override,
            Some("pane-A".to_string()),
        );

        // Drop the surfaced state — chip target now resolves to None and
        // the dedupe slot must follow so a fresh surface re-emits cleanly.
        app.tab_mut(DEFAULT_TAB_ID).turn = TurnState::Idle;
        app.recompute_chip_override(DEFAULT_TAB_ID);
        assert_eq!(
            app.tab_mut(DEFAULT_TAB_ID).last_emitted_chip_override,
            None,
        );
    }

    #[test]
    fn known_cli_id_returns_some_for_all_first_party_clis() {
        use crate::agent_sessions::CliSource;
        assert_eq!(known_cli_id(&CliSource::Claude),  Some("claude"));
        assert_eq!(known_cli_id(&CliSource::Codex),   Some("codex"));
        assert_eq!(known_cli_id(&CliSource::Copilot), Some("copilot"));
        assert_eq!(known_cli_id(&CliSource::Gemini),  Some("gemini"));
    }

    #[test]
    fn known_cli_id_returns_none_for_unknown_variant() {
        use crate::agent_sessions::CliSource;
        assert_eq!(known_cli_id(&CliSource::Unknown("anything".to_string())), None);
    }

    #[test]
    fn enter_on_wsl_history_row_resumes_inside_distro() {
        use crate::agent_sessions::{AgentStatus, CliSource, SessionLocation, SessionOrigin};
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let row = crate::agent_sessions::AgentSession {
            key:              "abc-123".to_string(),
            cli_source:       CliSource::Copilot,
            pane_session_id:  None,
            window_id:        None,
            tab_id:           None,
            title:            "t".to_string(),
            cwd:              std::path::PathBuf::from("/home/u/proj"),
            started_at:       std::time::SystemTime::UNIX_EPOCH,
            last_activity_at: std::time::SystemTime::UNIX_EPOCH,
            status:           AgentStatus::Historical,
            last_error:       None,
            current_tool:     None,
            attention_reason: None,
            log_path:         None,
            origin:           SessionOrigin::Unknown,
            location:         SessionLocation::Wsl { distro: "Ubuntu".to_string() },
        };
        let mut app = test_app();
        app.agent_sessions.merge_historical(vec![row]);
        app.current_tab_mut().current_view = View::Agents;
        app.current_tab_mut().agents_list_state.select(Some(0));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        let cmd = app
            .last_dispatched_command_for_test()
            .expect("a command was dispatched");
        assert_eq!(cmd.kind, DispatchedCommandKind::NewTabResume);
        let argv = cmd.argv.join(" ");
        assert!(
            argv.contains("wsl -d Ubuntu --cd \"/home/u/proj\" -- bash -lc \"copilot --resume abc-123\""),
            "expected in-distro resume; argv: {argv}"
        );
        // The loading banner keeps the short session id and also names the
        // distro for WSL rows.
        assert!(
            argv.contains("Resuming copilot session abc-123 in Ubuntu (WSL)"),
            "expected distro-named WSL banner; argv: {argv}"
        );
        // WSL rows must not also pass the Windows `-d <cwd>` flag.
        assert!(!argv.contains(" -d /home"), "WSL row must not pass Windows -d cwd");
    }
}
