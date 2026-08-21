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
    agent_source: crate::agent_source::AgentSource,
    source_cwd: Option<String>,
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

fn agent_command_on_enter(input: &str, selected: Option<&AvailableAgent>) -> Option<ParsedCommand> {
    commands::agent_id_prefix(input)?;
    Some(ParsedCommand {
        kind: CommandKind::Agent,
        spec: commands::lookup("agent").expect("/agent is registered"),
        rest: selected?.id.clone(),
    })
}

mod attachments;
mod autofix;
mod input_edit;
mod tab_state;
mod turn_state;
use autofix::*;

pub use crate::turn_context::TurnContext;
#[cfg(test)]
use input_edit::{next_word_boundary, prev_word_boundary, INPUT_HISTORY_MAX_ENTRIES};
pub(crate) use tab_state::DEFAULT_TAB_ID;
pub use tab_state::{
    ChatMessage, CompletedTurn, ConfigPickerState, NoticeKind, PermissionState,
    RecommendationFocus, TabSession, ToolCallContent, ToolCallKind, ToolCallLocation,
    ToolCallOutput, UserInputState, View,
};
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

pub use crate::app_contracts::{
    AcpModelInfo, AppEvent, AvailableAgent, CheckStatus, DebugDir, DebugMessage, PlanEntryStatus,
    PreflightResult,
};
use crate::commands::{self, CommandKind, ParseOutcome, ParsedCommand};
use crate::coordinator::{recommended_choice_index, RecommendationChoice, RecommendationSet};
use crate::pane_context::PaneContext;

use crate::protocol::acp::client::{
    CancelRequest, DropSessionRequest, LoadSessionForTab, NewSessionForTab, PromptSubmission,
    RenameSessionRequest, RestartRequest,
};
use crate::protocol::acp::turn_metrics::prompt_timing_log;
use crate::ui;
use crate::ui_trace;
use crate::wt_protocol_events::send as send_wt_protocol_event;

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
    /// Open the same source-aware picker as `/agent`.
    ChooseAgentSource,
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

/// Decide whether a failed post-login reconnect should respawn the shared
/// master. External-auth agents need this when the old process retained stale
/// credentials through `session/new`; every agent needs it when the old master
/// is no longer reachable. Other handshake failures follow normal error policy.
fn should_trigger_post_login_recovery(
    post_login_auth: bool,
    is_external_auth_agent: bool,
    failure: &crate::protocol::acp::failure::AgentFailure,
) -> bool {
    use crate::protocol::acp::failure::HandshakeStage;

    post_login_auth
        && ((is_external_auth_agent
            && (failure.is_auth() || failure.failed_at(HandshakeStage::NewSession)))
            || failure.failed_at(HandshakeStage::PipeConnect))
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
        } else if *reason == SetupReason::AgentError {
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
    opts.push(SetupOption::ChooseAgentSource);
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

/// Members of `params.payload` that [`route_agent_event_to_registry_with_hook_sink`]
/// actually reads.
///
/// This is the other half of the cross-language contract described on
/// [`crate::agent_sessions::USER_INPUT_TOOL_NAMES`]. The hook producer
/// (`BuildAgentHookEventJson` in `src/tools/wtcli/wtcli_functions.h`) redacts
/// the payload with a denylist before broadcasting it, so a key that lands in
/// that denylist while still being read here goes silently empty — no error,
/// no log, just a blank cwd or a lost notification message.
/// `hook_contract_tests` asserts the denylist and this list stay disjoint.
///
/// Nested lookups (`tool_input.question` / `.prompt` / `.message`) are covered
/// by their `tool_input` parent, which the producer strips only for tools
/// outside `USER_INPUT_TOOL_NAMES`.
pub const CONSUMED_PAYLOAD_KEYS: &[&str] = &[
    "cwd",
    "tool_name",
    "toolName",
    "tool_input",
    "message",
    "notification_type",
    "reason",
    "error",
];

/// Members of `payload.tool_input` the routing function reads when the tool is
/// a user-input tool.
///
/// The producer projects `tool_input` down to exactly these when an event
/// overflows its wire budget (`ReduceOversizedHookPayload` in
/// `src/tools/wtcli/wtcli_functions.h`), so a name missing here is dropped
/// from the degraded payload and the notification loses its question text.
pub const CONSUMED_TOOL_INPUT_KEYS: &[&str] = &["question", "prompt", "message"];

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
    // Copilot Memory runs internal sidekick workers in the parent CLI process.
    // Their hooks inherit the parent's WT pane but carry a distinct
    // `sidekick-*` session id. Treating those ids as user sessions rebinds the
    // pane away from its real owner and creates a duplicate `/sessions` row.
    if cli_source == CliSource::Copilot && asid.starts_with("sidekick-") {
        tracing::debug!(
            target: "agent_route",
            event = %event,
            asid = %asid,
            pane_session_id = %pane_session_id,
            cli_source = ?cli_source,
            "skipped: internal Copilot sidekick session"
        );
        return false;
    }
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
                let tool_event = SessionEvent::ToolStarting {
                    key: key.clone(),
                    tool_name,
                };
                reg.apply(tool_event.clone());
                hook_sink(tool_event);
                let message = payload
                    .get("tool_input")
                    .and_then(|ti| {
                        ti.get("question")
                            .or_else(|| ti.get("prompt"))
                            .or_else(|| ti.get("message"))
                    })
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
        // ignore `tool.execution_complete`.
        //
        // Because these are dropped, no bundle subscribes them any more: each
        // one cost a shell spawn (~400ms under PowerShell) plus a COM round
        // trip, per tool call, to be discarded here. Copilot's `PostToolUse` /
        // `PostToolUseFailure`, Gemini's `AfterTool`, and OpenCode's
        // `tool.execute.after` were all removed. The arm stays so an older
        // installed bundle that still emits them is ignored rather than
        // mis-routed.
        "agent.tool.completed" | "agent.tool.finished" | "agent.tool.failed" => {
            return reg.take_dirty();
        }
        "agent.stop" | "agent.subagent.stop" => SessionEvent::ToolCompleted { key },
        "agent.notification" => {
            // Not every notification means "the agent is blocked on you".
            // Claude's `Notification` hook covers two cases, distinguished by
            // `notification_type`:
            //
            //   * `permission_prompt` — the agent wants approval for a tool.
            //     Genuinely Attention: nothing proceeds until the user acts.
            //   * `idle_prompt` — the agent already finished its turn and has
            //     simply been sitting at an empty prompt for 60s. It fires
            //     *after* `agent.stop` has correctly moved the row to Idle, so
            //     treating it as Attention parks every session at "Claude is
            //     waiting for your input" between turns.
            //
            // Drop `idle_prompt` and let `agent.stop` keep ownership of the
            // turn-end transition, the same division of labour the tool
            // completion arm above relies on. Deliberately *not* mapped to
            // Idle: an unanswered `permission_prompt` also goes idle after
            // 60s, and demoting that row would hide a pending approval.
            //
            // Unknown types stay Attention on purpose. Over-reporting costs a
            // stale badge the next event clears; under-reporting loses a
            // permission request with no other signal that it happened.
            let notification_type = payload
                .get("notification_type")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if notification_type == "idle_prompt" {
                return reg.take_dirty();
            }
            SessionEvent::Notification {
                key,
                message: payload
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            }
        }
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
                    };
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
        "set_agent_state" | "agent_paste_text" => {
            // handle_event consumes these at the top of WtEvent
            // before classification runs, so classify normally never sees
            // it. Add an explicit arm anyway so a future refactor that
            // drops the early return doesn't surface a stray
            // "Pane: <method>" banner via the default catch-all.
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

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomModelCatalogEntry {
    pub selection_id: String,
    #[serde(default)]
    pub provider_id: String,
    #[serde(default)]
    pub provider_name: String,
    #[serde(
        default = "default_custom_model_api_contract",
        deserialize_with = "deserialize_custom_model_api_contract"
    )]
    pub api_contract: String,
    #[serde(default)]
    pub location: String,
    pub model_id: String,
    #[serde(default)]
    pub name: String,
}

fn default_custom_model_api_contract() -> String {
    crate::custom_model_provider::CANONICAL_API_CONTRACT.to_string()
}

fn deserialize_custom_model_api_contract<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = <String as serde::Deserialize>::deserialize(deserializer)?;
    crate::custom_model_provider::normalize_api_contract(&value)
        .map(str::to_string)
        .ok_or_else(|| {
            serde::de::Error::custom(format!(
                "unsupported custom model API contract {value:?}; expected {:?}",
                crate::custom_model_provider::CANONICAL_API_CONTRACT
            ))
        })
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
    /// Execution source paired with `current_agent_id`.
    pub current_agent_source: crate::agent_source::AgentSource,
    /// Agent ids supplied by Windows Terminal after GPO filtering.
    allowed_agent_ids: Vec<String>,
    /// Distinguishes a manual/legacy launch from an explicitly empty policy.
    host_agent_allowlist_present: bool,
    /// Installed subset of the host-allowed agents, refreshed by `/agent`.
    pub available_agents: Vec<AvailableAgent>,
    agent_source_probe_generation: u64,
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
    /// BYOK-only projection used by the `/model` command. Cloud/native models
    /// remain in `available_models` for Settings but are not shown in-pane.
    model_picker_models: Vec<AcpModelInfo>,
    pub current_model_id: Option<String>,
    /// Latest ACP model config for each session. Notifications can race ahead
    /// of the event that attaches their session to a tab.
    session_model_configs: HashMap<String, (Vec<AcpModelInfo>, Option<String>)>,
    /// Complete ordered ACP select config options, keyed by SessionId.
    session_config_options: HashMap<String, Vec<crate::app_contracts::AcpSessionConfigOption>>,
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
    pub(crate) text_selection: crate::text_selection::TextSelection,
    pub(crate) completed_turn_hits: Vec<CompletedTurnHitRegion>,
    pub(crate) pressed_completed_turn: Option<PressedCompletedTurn>,
    pub(crate) last_completed_turn_click: Option<CompletedTurnClickRecord>,
    pub(crate) input_dialog_area: Option<Rect>,
    pub(crate) pressed_input_dialog_tab: Option<String>,
    pub(crate) completed_turn_action_links: Vec<crate::action_links::CompletedTurnActionLink>,
    pub(crate) painted_completed_turn_action_links:
        Vec<crate::action_links::CompletedTurnActionLink>,
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
    /// Whether agent responses are projected as Markdown instead of raw text.
    pub render_agent_markdown: bool,
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
    /// truth) and the `agents_view::render` call in `ui/layout.rs`. See
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
    delegate_agents: Option<Arc<std::sync::Mutex<Vec<crate::coordinator::DelegateAgentRuntime>>>>,
    /// The helper's own `--agent` cmdline. Needed to re-derive the delegate
    /// runtime commandline when only the delegate agent/model change.
    delegate_base_agent_cmd: String,
    /// The configured ACP model override (the `--acp-model` setting). Seeded
    /// from the spawn cmdline and updated on `agent_config_changed`. Re-applied
    /// to every freshly-created session (via `SessionAttached`) so `/new` and
    /// lazy-first-prompt sessions stay on the configured model, not just the
    /// bootstrap one. None = "agent default" (no override).
    acp_model: Option<String>,
    /// Whether this helper was created from the global ACP agent/model
    /// settings. Per-tab/profile-pinned helpers keep this false so a hot
    /// global model update cannot inject another tab's model into their CLI.
    follows_global_acp_model: bool,
    /// True after the host has delivered its credential-free cloud/custom
    /// catalogs over `agent_config_changed`. Published in `agent_status` so
    /// C++ can send the catalog once after each helper reaches Connected.
    host_catalog_ready: bool,
    /// Shared-provider selection id supplied directly to this helper by WT.
    /// Kept separate from the master-only provider environment because this
    /// process owns the `/model` UI but never receives provider credentials.
    custom_model_selection: Option<String>,
    /// Credential-free provider/model metadata exposed through `/model`.
    custom_model_catalog: Vec<CustomModelCatalogEntry>,
    /// Last successful cloud/native model catalog supplied by Windows Terminal.
    cloud_models: Vec<AcpModelInfo>,
    /// Native model catalog advertised by the current ACP session. Kept
    /// separate from the merged picker rows so hot catalog removal cannot
    /// accidentally preserve stale custom entries.
    agent_models: Vec<AcpModelInfo>,
    /// Current model last reported or successfully selected for the ACP
    /// session, before the configured custom selection is projected on top.
    agent_current_model_id: Option<String>,
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
    /// (localized via `system.close_pane_hint`). Auto-clears at the
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
    pub proposal_channels:
        Arc<crate::agent_tools::action_proposal::channel::ProposalChannelManager>,
}

/// How long the close-pane arm (localized via `system.close_pane_hint`) stays live. Long
/// enough that the user can react after seeing the hint; short enough that
/// a stale arm doesn't bite the next time they want to clear input.
pub const CLOSE_PANE_ARM_WINDOW: std::time::Duration = std::time::Duration::from_millis(1500);
pub const SELECTION_COPIED_HINT_WINDOW: std::time::Duration =
    std::time::Duration::from_millis(1500);

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
        CliSource::Claude => Some("claude"),
        CliSource::Codex => Some("codex"),
        CliSource::Copilot => Some("copilot"),
        CliSource::Gemini => Some("gemini"),
        CliSource::OpenCode => Some("opencode"),
        CliSource::Unknown(_) => None,
    }
}

pub(crate) fn session_info_to_agent_session(
    info: &crate::session_registry::SessionInfo,
) -> crate::agent_sessions::AgentSession {
    use crate::agent_sessions::{AgentSession, AgentStatus, CliSource, SessionOrigin};
    let status = info.status.clone().unwrap_or(AgentStatus::Historical);
    let origin = info.origin.clone().unwrap_or(SessionOrigin::Unknown);
    let cli_source = info
        .cli_source
        .clone()
        .unwrap_or(CliSource::Unknown(String::new()));
    let title = info
        .title
        .clone()
        .filter(|title| !crate::agent_sessions::title_is_placeholder(&cli_source, title))
        .unwrap_or_else(|| {
            if cli_source == CliSource::OpenCode {
                String::new()
            } else {
                "—".to_string()
            }
        });
    let last_activity_at = info
        .last_activity_at_ms
        .map(|ms| std::time::UNIX_EPOCH + std::time::Duration::from_millis(ms))
        .unwrap_or_else(std::time::SystemTime::now);
    AgentSession {
        key: info.session_id.0.to_string(),
        cli_source,
        pane_session_id: info.pane_session_id.clone(),
        window_id: None,
        tab_id: None,
        title,
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
            current_agent_source: crate::agent_source::AgentSource::Host,
            allowed_agent_ids: Vec::new(),
            host_agent_allowlist_present: false,
            available_agents: Vec::new(),
            agent_source_probe_generation: 0,
            preflight_setup_active: false,
            agent_name: String::new(),
            agent_model: None,
            agent_version: None,
            available_models: Vec::new(),
            model_picker_models: Vec::new(),
            current_model_id: None,
            session_model_configs: HashMap::new(),
            session_config_options: HashMap::new(),
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
            text_selection: crate::text_selection::TextSelection::default(),
            completed_turn_hits: Vec::new(),
            pressed_completed_turn: None,
            last_completed_turn_click: None,
            input_dialog_area: None,
            pressed_input_dialog_tab: None,
            completed_turn_action_links: Vec::new(),
            painted_completed_turn_action_links: Vec::new(),
            pane_id: None,
            tab_id: None,
            owner_tab_id: None,
            window_id: None,
            wt_notifications: VecDeque::new(),
            show_notification_banner: false,
            autofix_enabled,
            render_agent_markdown: true,
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
            follows_global_acp_model: false,
            host_catalog_ready: false,
            custom_model_selection: None,
            custom_model_catalog: Vec::new(),
            cloud_models: Vec::new(),
            agent_models: Vec::new(),
            agent_current_model_id: None,
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
            proposal_channels: Arc::new(
                crate::agent_tools::action_proposal::channel::ProposalChannelManager::new(),
            ),
            shell_mgr,
        }
    }

    pub fn set_proposal_channels(
        &mut self,
        proposal_channels: Arc<
            crate::agent_tools::action_proposal::channel::ProposalChannelManager,
        >,
    ) {
        self.proposal_channels = proposal_channels;
    }

    pub fn set_render_agent_markdown(&mut self, render_agent_markdown: bool) {
        self.render_agent_markdown = render_agent_markdown;
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
        agent_source: crate::agent_source::AgentSource,
        source_cwd: Option<String>,
        owner_tab_id: Option<String>,
        shell_mgr: Arc<crate::shell::ShellManager>,
        wt_connected: bool,
    ) {
        self.deferred_acp = Some(DeferredAcpParams {
            agent_cmd,
            acp_model,
            agent_source,
            source_cwd,
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

        let cloud_models = self.cloud_models.clone();
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
                let agent_source = params.agent_source.clone();
                let source_cwd = params.source_cwd.clone();
                let event_tx = tx.clone();
                let shell_mgr = Arc::clone(&params.shell_mgr);
                let wt_connected = params.wt_connected;
                let pipe_name_opt = params.master_pipe_name.clone();
                let owner_tab_opt = params.owner_tab_id.clone();
                // Per-tab agent identity for the multi-agent master: declare
                // which agent this reconnecting helper wants. Derived from the
                // configured agent_cmd — the master reconstructs the command
                // from the id and never executes a string off the pipe.
                let agent_cmd_opt = Some(params.agent_cmd.clone()).filter(|s| !s.trim().is_empty());
                let agent_id_opt = agent_cmd_opt
                    .as_deref()
                    .map(|c| crate::agent_registry::resolve_agent_id_from_cmd(c).to_string());

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
                    let proposal_channels = Arc::clone(&self.proposal_channels);
                    tokio::task::spawn_local(async move {
                        if let Err(e) = crate::protocol::acp::client::run_acp_client_over_pipe(
                            pipe_name,
                            acp_model,
                            cloud_models,
                            agent_id_opt,
                            agent_source,
                            source_cwd,
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
                            proposal_channels,
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
                            // A post-login reconnect may fail because the old
                            // shared master is stale/dead after login:
                            //   * External-auth agent still AuthRequired after
                            //     authenticate/new_session → the long-lived CLI
                            //     cached unauthenticated state.
                            //   * PipeConnect failure → the master died before
                            //     login (e.g. Copilot was missing during IT
                            //     install flow), so the saved pipe no longer
                            //     exists.
                            // Both need a fresh master rather than another
                            // sign-in screen.
                            let is_external = matches!(
                                crate::agent_registry::lookup_profile_by_id(&recovery_agent_id)
                                    .acp_auth_flow,
                                crate::agent_registry::AcpAuthFlow::External
                            );
                            if should_trigger_post_login_recovery(
                                post_login_auth,
                                is_external,
                                &failure,
                            ) {
                                tracing::warn!(
                                    target: "auth_recovery",
                                    agent_id = %recovery_agent_id,
                                    tab_id = ?recovery_tab_id,
                                    failure_class = failure.class(),
                                    "post-login reconnect needs fresh master; requesting auth recovery"
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
        follows_global_acp_model: bool,
    ) {
        self.delegate_agents = Some(delegate_agents);
        self.delegate_base_agent_cmd = base_agent_cmd;
        self.acp_model = acp_model.filter(|s| !s.trim().is_empty());
        self.follows_global_acp_model = follows_global_acp_model;
    }

    pub fn set_host_catalog_ready(&mut self, ready: bool) {
        self.host_catalog_ready = ready;
    }

    pub fn set_cloud_models(&mut self, models: Vec<AcpModelInfo>) {
        self.cloud_models = models;
        self.rebuild_model_catalog();
    }

    pub fn set_custom_model_config(
        &mut self,
        models: Vec<CustomModelCatalogEntry>,
        selection_id: Option<String>,
    ) {
        self.seed_agent_models_from_available_if_needed();
        let requested_selection = selection_id
            .map(|id| id.trim().to_string())
            .filter(|id| !id.is_empty());
        self.custom_model_catalog = models
            .into_iter()
            .filter_map(|model| {
                let api_contract =
                    crate::custom_model_provider::normalize_api_contract(&model.api_contract)?;
                Some(CustomModelCatalogEntry {
                    selection_id: model.selection_id.trim().to_string(),
                    provider_id: model.provider_id.trim().to_string(),
                    provider_name: model.provider_name.trim().to_string(),
                    api_contract: api_contract.to_string(),
                    location: model.location.trim().to_string(),
                    model_id: model.model_id.trim().to_string(),
                    name: model.name.trim().to_string(),
                })
            })
            .filter(|model| !model.selection_id.is_empty() && !model.model_id.is_empty())
            .collect();
        self.custom_model_selection = requested_selection.filter(|selection| {
            self.custom_model_catalog
                .iter()
                .any(|model| model.selection_id == *selection)
        });
        self.rebuild_model_catalog();
    }

    fn seed_agent_models_from_available_if_needed(&mut self) {
        if self.agent_models.is_empty() && !self.available_models.is_empty() {
            self.agent_models = self.available_models.clone();
        }
    }

    fn merge_custom_models(&self, advertised: Vec<AcpModelInfo>) -> Vec<AcpModelInfo> {
        let mut merged = self.cloud_models.clone();
        for model in advertised {
            if !merged.iter().any(|existing| existing.id == model.id) {
                merged.push(model);
            }
        }
        for custom in &self.custom_model_catalog {
            merged.retain(|model| {
                model.id != custom.selection_id
                    && model.id != format!("intelligent-terminal/{}", custom.model_id)
            });
            merged.push(AcpModelInfo {
                id: custom.selection_id.clone(),
                name: format!("{} (BYOK)", custom.model_id),
                description: None,
            });
        }
        merged
    }

    fn selected_custom_model_id(&self) -> Option<&str> {
        self.custom_model_selection.as_deref().and_then(|selected| {
            self.custom_model_catalog
                .iter()
                .find(|model| model.selection_id == selected)
                .map(|model| model.selection_id.as_str())
        })
    }

    fn resolve_current_model_id(&self, agent_model_id: Option<String>) -> Option<String> {
        self.selected_custom_model_id()
            .map(str::to_string)
            .or(agent_model_id)
    }

    fn capture_model_picker_selections(&self) -> HashMap<String, Option<String>> {
        self.tab_sessions
            .iter()
            .map(|(tab_id, tab)| {
                let selected = self
                    .model_picker_models
                    .get(tab.model_picker_selected)
                    .map(|model| model.id.clone());
                (tab_id.clone(), selected)
            })
            .collect()
    }

    fn recompute_current_model_id(&mut self) {
        let pane_override = self.current_tab().model_override.clone();
        let previous_current = self.current_model_id.take().filter(|id| {
            !id.starts_with("custom:")
                && (self.available_models.is_empty()
                    || self.available_models.iter().any(|model| model.id == *id))
        });
        self.current_model_id = self.resolve_current_model_id(
            pane_override
                .or_else(|| self.acp_model.clone())
                .or_else(|| self.agent_current_model_id.clone())
                .or(previous_current),
        );
    }

    fn reconcile_model_picker_selections(
        &mut self,
        old_selected_ids: HashMap<String, Option<String>>,
    ) {
        let selected_custom = self.selected_custom_model_id().map(str::to_string);
        let current_model = self.current_model_id.clone();
        let model_ids = self
            .model_picker_models
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>();

        for (tab_id, tab) in &mut self.tab_sessions {
            if self.model_picker_models.is_empty() {
                tab.model_picker_open = false;
                tab.model_picker_selected = 0;
                continue;
            }

            let effective_current = tab
                .model_override
                .as_deref()
                .or(selected_custom.as_deref())
                .or(current_model.as_deref())
                .or(self.acp_model.as_deref());
            let enabled = |id: &str| effective_current == Some(id);
            let selected = old_selected_ids
                .get(tab_id)
                .and_then(|id| id.as_deref())
                .and_then(|id| {
                    model_ids
                        .iter()
                        .position(|candidate| *candidate == id && enabled(candidate))
                })
                .or_else(|| {
                    effective_current
                        .and_then(|id| model_ids.iter().position(|candidate| *candidate == id))
                })
                .or_else(|| model_ids.iter().position(|id| enabled(id)))
                .unwrap_or(0);
            tab.model_picker_selected = selected;
        }
    }

    fn rebuild_model_catalog(&mut self) {
        let old_selected_ids = self.capture_model_picker_selections();
        self.available_models = self.merge_custom_models(self.agent_models.clone());
        let selected_custom = self.selected_custom_model_id().map(str::to_owned);
        self.model_picker_models = self
            .available_models
            .iter()
            .filter(|model| match selected_custom.as_deref() {
                Some(selected) => model.id == selected,
                None => !self
                    .custom_model_catalog
                    .iter()
                    .any(|custom| custom.selection_id == model.id),
            })
            .cloned()
            .collect();
        self.recompute_current_model_id();
        self.reconcile_model_picker_selections(old_selected_ids);
    }

    fn rebuild_model_catalog_from_agent_state(&mut self) {
        self.current_model_id = None;
        self.rebuild_model_catalog();
    }

    /// Low-level: ask the ACP client task to apply `model` via
    /// `set_session_model`. `session_id == Some` targets exactly that live
    /// session (the per-pane `/model` pick); `None` fans out to every session
    /// this helper owns. No-op on an empty/whitespace model — an empty
    /// override means "agent default", which `set_session_model` can't
    /// express.
    fn send_session_model(
        &self,
        session_id: Option<String>,
        model: String,
        pane_override: bool,
    ) {
        if model.trim().is_empty() {
            return;
        }
        let _ = self.master_request_tx.send(
            crate::protocol::acp::client::MasterExtRequest::SetSessionModel {
                session_id: session_id.map(agent_client_protocol::schema::v1::SessionId::new),
                model,
                pane_override,
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

    /// Push the global `acpModel` to each live session that has not been pinned
    /// by the pane-local `/model` picker.
    fn send_acp_model_update(&self) {
        let Some(model) = self.acp_model.as_ref().filter(|s| !s.trim().is_empty()) else {
            return;
        };
        for tab in self.tab_sessions.values() {
            if tab.model_override.is_some() {
                continue;
            }
            if let Some(sid) = tab.session_id.clone() {
                self.send_session_model(Some(sid), model.clone(), false);
            }
        }
    }

    /// Apply a global `acpModel` settings change only when this helper follows
    /// that exact global agent. Pane/profile-pinned helpers and pane-local
    /// `/model` overrides remain untouched. An empty value means "agent
    /// default"; no live switch is sent because ACP has no portable reset
    /// operation.
    fn apply_global_acp_model(&mut self, target_agent_id: &str, new_model: Option<String>) -> bool {
        if !self.follows_global_acp_model
            || !self.current_agent_id.eq_ignore_ascii_case(target_agent_id)
        {
            return false;
        }

        self.acp_model = new_model.filter(|s| !s.trim().is_empty());
        self.send_acp_model_update();
        self.publish_agent_status();
        true
    }

    // ── /agent picker ───────────────────────────────────────────────────

    /// Seed the helper-side policy snapshot. An absent flag permits all known
    /// agents for manual/legacy launches; a present-but-empty flag permits none.
    pub fn set_allowed_agent_ids(&mut self, raw_ids: Vec<String>) {
        self.host_agent_allowlist_present = !raw_ids.is_empty();
        self.allowed_agent_ids = raw_ids
            .into_iter()
            .map(|id| id.trim().to_ascii_lowercase())
            .filter(|id| !id.is_empty() && crate::agent_registry::is_known_id(id))
            .collect();
        self.allowed_agent_ids.sort();
        self.allowed_agent_ids.dedup();
        self.refresh_available_agents();
    }

    fn refresh_available_agents(&mut self) {
        self.available_agents = crate::agent_registry::KNOWN_AGENTS
            .iter()
            .filter(|profile| {
                (!self.host_agent_allowlist_present
                    || self.allowed_agent_ids.iter().any(|id| id == profile.id))
                    && crate::agent_check::find_exe(profile.id).is_some()
            })
            .map(|profile| {
                let source = crate::agent_source::AgentSource::Host;
                AvailableAgent {
                    id: profile.id.to_string(),
                    display_name: format!("{} — {}", profile.display_name, source.display_suffix()),
                    source,
                }
            })
            .collect();
    }

    fn request_agent_source_picker(&mut self) {
        self.refresh_available_agents();
        self.agent_source_probe_generation = self.agent_source_probe_generation.wrapping_add(1);
        let generation = self.agent_source_probe_generation;
        let Some(event_tx) = self.event_tx.clone() else {
            return;
        };
        let shell_mgr = Arc::clone(&self.shell_mgr);
        let allowed_agent_ids = self.allowed_agent_ids.clone();
        let allowlist_present = self.host_agent_allowlist_present;
        tokio::task::spawn_local(async move {
            let active_pane = shell_mgr.wt_get_active_pane().await.ok();
            let Some(distro) = crate::agent_source::active_pane_wsl_distro(active_pane.as_ref())
                .map(str::to_string)
            else {
                let _ = event_tx.send(AppEvent::AgentSourcesDiscovered {
                    generation,
                    wsl_sources: Vec::new(),
                });
                return;
            };

            use futures::StreamExt;
            let candidates = crate::agent_registry::KNOWN_AGENTS
                .iter()
                .filter(|profile| {
                    !allowlist_present || allowed_agent_ids.iter().any(|id| id == profile.id)
                })
                .map(|profile| (profile.id, profile.display_name));
            let wsl_sources = futures::stream::iter(candidates)
                .map(|(id, display_name)| {
                    let distro = distro.clone();
                    async move {
                        crate::agent_check::wsl_agent_available(&distro, id)
                            .await
                            .then(|| {
                                let source = crate::agent_source::AgentSource::Wsl { distro };
                                AvailableAgent {
                                    id: id.to_string(),
                                    display_name: format!(
                                        "{display_name} — {}",
                                        source.display_suffix()
                                    ),
                                    source,
                                }
                            })
                    }
                })
                .buffer_unordered(crate::agent_registry::KNOWN_AGENTS.len())
                .filter_map(async move |source| source)
                .collect()
                .await;
            let _ = event_tx.send(AppEvent::AgentSourcesDiscovered {
                generation,
                wsl_sources,
            });
        });
    }

    fn agent_picker_visible(&self) -> bool {
        self.current_tab().agent_picker_open
    }

    fn find_host_agent_for_command<'a>(
        available_agents: &'a [AvailableAgent],
        arg: &str,
    ) -> Option<&'a AvailableAgent> {
        available_agents.iter().find(|agent| {
            agent.source == crate::agent_source::AgentSource::Host
                && crate::agent_registry::KNOWN_AGENTS.iter().any(|profile| {
                    profile.id.eq_ignore_ascii_case(&agent.id)
                        && (profile.id.eq_ignore_ascii_case(arg)
                            || profile.display_name.eq_ignore_ascii_case(arg))
                })
        })
    }

    fn cmd_agent(&mut self, arg: String) {
        let arg = arg.trim();
        if arg.is_empty() {
            self.request_agent_source_picker();
            return;
        }

        self.refresh_available_agents();
        let selected = Self::find_host_agent_for_command(&self.available_agents, arg).cloned();
        match selected {
            Some(agent) => self.apply_agent_pick(agent),
            None => {
                let tab = self.current_tab_mut();
                tab.messages.push(ChatMessage::error(
                    t!("system.agent_unknown", agent = arg).into_owned(),
                ));
                tab.scroll_to_bottom();
            }
        }
    }

    fn close_agent_picker(&mut self) {
        self.current_tab_mut().agent_picker_open = false;
    }

    fn open_agent_picker(&mut self, selected: usize) {
        let tab = self.current_tab_mut();
        tab.model_picker_open = false;
        tab.config_picker = ConfigPickerState::Closed;
        tab.agent_picker_open = true;
        tab.agent_picker_selected = selected;
    }

    fn agent_picker_up(&mut self) {
        let tab = self.current_tab_mut();
        tab.agent_picker_selected = tab.agent_picker_selected.saturating_sub(1);
    }

    fn agent_picker_down(&mut self) {
        let last = self.available_agents.len().saturating_sub(1);
        let tab = self.current_tab_mut();
        if tab.agent_picker_selected < last {
            tab.agent_picker_selected += 1;
        }
    }

    fn commit_agent_pick(&mut self) {
        let selected = self.current_tab().agent_picker_selected;
        let agent = self.available_agents.get(selected).cloned();
        self.close_agent_picker();
        if let Some(agent) = agent {
            self.apply_agent_pick(agent);
        }
    }

    fn apply_agent_pick(&mut self, agent: AvailableAgent) {
        if agent.id == self.current_agent_id && agent.source == self.current_agent_source {
            return;
        }
        let Some(tab_id) = self.owner_tab_id.as_deref() else {
            self.push_agent_switch_unavailable();
            return;
        };
        let Some(window_id) = self.window_id.as_deref() else {
            self.push_agent_switch_unavailable();
            return;
        };
        send_wt_protocol_event(build_switch_agent_event(
            window_id,
            tab_id,
            &agent.id,
            &agent.source,
        ));
    }

    fn push_agent_switch_unavailable(&mut self) {
        let tab = self.current_tab_mut();
        tab.messages.push(ChatMessage::warning(
            t!("system.agent_switch_unavailable").into_owned(),
        ));
        tab.scroll_to_bottom();
    }

    // ── /config picker ──────────────────────────────────────────────────

    fn config_picker_visible(&self) -> bool {
        self.current_tab().config_picker.is_open()
    }

    fn current_session_config_options(&self) -> &[crate::app_contracts::AcpSessionConfigOption] {
        self.current_tab()
            .session_id
            .as_deref()
            .and_then(|session_id| self.session_config_options.get(session_id))
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    fn cmd_config(&mut self) {
        if self.current_session_config_options().is_empty() {
            let tab = self.current_tab_mut();
            tab.messages.push(ChatMessage::info(
                t!("system.no_config_options").into_owned(),
            ));
            tab.scroll_to_bottom();
            return;
        }
        self.open_config_picker();
    }

    fn open_config_picker(&mut self) {
        if self.current_session_config_options().is_empty() {
            return;
        }
        let tab = self.current_tab_mut();
        tab.agent_picker_open = false;
        tab.model_picker_open = false;
        tab.config_picker = ConfigPickerState::Options { selected: 0 };
    }

    fn open_config_value_picker(&mut self, config_id: String) {
        let Some(option) = self
            .current_session_config_options()
            .iter()
            .find(|option| option.id == config_id)
        else {
            return;
        };
        let selected = option
            .values
            .iter()
            .position(|value| value.id == option.current_value)
            .unwrap_or(0);
        let tab = self.current_tab_mut();
        tab.agent_picker_open = false;
        tab.model_picker_open = false;
        tab.config_picker = ConfigPickerState::Values {
            option_id: config_id,
            selected,
            parent_selected: None,
        };
    }

    fn config_picker_row_count(&self) -> usize {
        let options = self.current_session_config_options();
        self.current_tab()
            .config_picker
            .option_id()
            .and_then(|config_id| options.iter().find(|option| option.id == config_id))
            .map(|option| option.values.len())
            .unwrap_or(options.len())
    }

    fn config_picker_up(&mut self) {
        match &mut self.current_tab_mut().config_picker {
            ConfigPickerState::Options { selected }
            | ConfigPickerState::Values { selected, .. } => {
                *selected = selected.saturating_sub(1);
            }
            ConfigPickerState::Closed => {}
        }
    }

    fn config_picker_down(&mut self) {
        let max = self.config_picker_row_count().saturating_sub(1);
        match &mut self.current_tab_mut().config_picker {
            ConfigPickerState::Options { selected }
            | ConfigPickerState::Values { selected, .. } => {
                *selected = (*selected + 1).min(max);
            }
            ConfigPickerState::Closed => {}
        }
    }

    fn config_picker_escape(&mut self) {
        let next = match &self.current_tab().config_picker {
            ConfigPickerState::Values {
                parent_selected: Some(selected),
                ..
            } => ConfigPickerState::Options {
                selected: *selected,
            },
            _ => ConfigPickerState::Closed,
        };
        self.current_tab_mut().config_picker = next;
    }

    fn config_picker_enter(&mut self) {
        let picker = self.current_tab().config_picker.clone();
        let options = self.current_session_config_options();

        if let ConfigPickerState::Values {
            option_id: config_id,
            selected,
            parent_selected,
        } = picker
        {
            let Some(option) = options.iter().find(|option| option.id == config_id) else {
                self.config_picker_escape();
                return;
            };
            let Some(value) = option.values.get(selected) else {
                return;
            };
            if value.id == option.current_value {
                self.config_picker_escape();
                return;
            }

            let session_id = self.current_tab().session_id.clone();
            let config_id = option.id.clone();
            let value_id = value.id.clone();
            if let Some(session_id) = session_id {
                let tab = self.current_tab_mut();
                if tab.config_pending_id.is_some() {
                    return;
                }
                tab.config_pending_id = Some(config_id.clone());
                tab.config_picker = parent_selected
                    .map(|selected| ConfigPickerState::Options { selected })
                    .unwrap_or(ConfigPickerState::Closed);
                let _ = self.master_request_tx.send(
                    crate::protocol::acp::client::MasterExtRequest::SetSessionConfigOption {
                        session_id: agent_client_protocol::schema::v1::SessionId::new(session_id),
                        config_id,
                        value: value_id,
                    },
                );
            }
            return;
        }

        let ConfigPickerState::Options { selected } = picker else {
            return;
        };
        let Some(option) = options.get(selected) else {
            return;
        };
        let value_selected = option
            .values
            .iter()
            .position(|value| value.id == option.current_value)
            .unwrap_or(0);
        let option_id = option.id.clone();
        self.current_tab_mut().config_picker = ConfigPickerState::Values {
            option_id,
            selected: value_selected,
            parent_selected: Some(selected),
        };
    }

    // ── /model picker ───────────────────────────────────────────────────

    /// True while the model picker modal is up for the active tab.
    fn model_picker_visible(&self) -> bool {
        self.current_tab().model_picker_open
    }

    /// `/model [id]` — show models from the mode selected in Settings. Cloud
    /// mode shows only agent/cloud models; custom-provider mode shows only BYOK models.
    /// Crossing modes requires changing Settings and restarting the agent.
    fn cmd_model(&mut self, arg: String) {
        let arg = arg.trim().to_string();
        let model_config_id = self
            .current_session_config_options()
            .iter()
            .find(|option| option.is_model())
            .map(|option| option.id.clone());
        if arg.is_empty() {
            if let Some(config_id) = model_config_id {
                self.open_config_value_picker(config_id);
                return;
            }
        }
        if self.model_picker_models.is_empty() {
            let tab = self.current_tab_mut();
            tab.messages
                .push(ChatMessage::info(t!("system.no_models").into_owned()));
            tab.scroll_to_bottom();
            return;
        }
        if arg.is_empty() {
            self.open_model_picker();
            return;
        }
        // Direct switch: exact id first, then case-insensitive id/name.
        let matched = self
            .model_picker_models
            .iter()
            .find(|m| m.id == arg)
            .or_else(|| {
                self.model_picker_models
                    .iter()
                    .find(|m| m.id.eq_ignore_ascii_case(&arg) || m.name.eq_ignore_ascii_case(&arg))
            })
            .map(|m| m.id.clone());
        match matched {
            Some(id) if self.model_pick_enabled(&id) => self.apply_model_pick(id),
            Some(_) => self.open_model_picker(),
            None => {
                let tab = self.current_tab_mut();
                tab.messages.push(ChatMessage::error(
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
        if self.model_picker_models.is_empty() {
            return;
        }
        let current = self.current_model_id_for_picker().map(str::to_string);
        let selected = current
            .and_then(|cur| self.model_picker_models.iter().position(|m| m.id == cur))
            .or_else(|| {
                self.model_picker_models
                    .iter()
                    .position(|model| self.model_pick_enabled(&model.id))
            })
            .unwrap_or(0);
        let tab = self.current_tab_mut();
        tab.agent_picker_open = false;
        tab.config_picker = ConfigPickerState::Closed;
        tab.model_picker_open = true;
        tab.model_picker_selected = selected;
    }

    fn close_model_picker(&mut self) {
        self.current_tab_mut().model_picker_open = false;
    }

    fn model_picker_up(&mut self) {
        let selected = self.current_tab().model_picker_selected;
        if let Some(next) = (0..selected)
            .rev()
            .find(|&index| self.model_pick_enabled(&self.model_picker_models[index].id))
        {
            self.current_tab_mut().model_picker_selected = next;
        }
    }

    fn model_picker_down(&mut self) {
        let selected = self.current_tab().model_picker_selected;
        if let Some(next) = ((selected + 1)..self.model_picker_models.len())
            .find(|&index| self.model_pick_enabled(&self.model_picker_models[index].id))
        {
            self.current_tab_mut().model_picker_selected = next;
        }
    }

    /// Commit the highlighted row in the open picker.
    fn commit_model_pick(&mut self) {
        let idx = self.current_tab().model_picker_selected;
        let id = self.model_picker_models.get(idx).map(|m| m.id.clone());
        if let Some(id) = id.filter(|id| self.model_pick_enabled(id)) {
            self.close_model_picker();
            self.apply_model_pick(id);
        }
    }

    fn current_model_id_for_picker(&self) -> Option<&str> {
        self.current_tab()
            .model_override
            .as_deref()
            .or_else(|| self.selected_custom_model_id())
            .or(self.current_model_id.as_deref())
            .or(self.acp_model.as_deref())
    }

    fn model_pick_enabled(&self, _model_id: &str) -> bool {
        true
    }

    /// Pin the active pane to `model_id` and hot-apply it to the live ACP
    /// session. The picker is already scoped to the local/cloud mode selected
    /// in Settings, so slash-command changes never cross modes or restart the
    /// agent CLI.
    fn apply_model_pick(&mut self, model_id: String) {
        if self.current_model_id_for_picker() == Some(model_id.as_str()) {
            return;
        }
        if !self.model_pick_enabled(&model_id) {
            return;
        }

        let session_id = self.current_tab().session_id.clone();
        if let Some(session_id) = session_id {
            self.send_session_model(Some(session_id), model_id, true);
            return;
        }

        let name = self.model_display_name(&model_id);
        let tab = self.current_tab_mut();
        tab.model_override = Some(model_id.clone());
        tab.messages.push(ChatMessage::success(
            t!("system.model_set", model = name.as_str()).into_owned(),
        ));
        tab.scroll_to_bottom();
        self.current_model_id = Some(model_id.clone());
        self.agent_current_model_id = Some(model_id);
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
    /// CLI so only matching rows are listed. Returns `None` when no agent
    /// has been selected yet or the agent is not one the session registry
    /// tracks (custom / unknown) — in that
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
        // Claude / Codex / Copilot / Gemini / OpenCode (all five CLIs accept some
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
                        t!("system.cannot_focus_session", session_id = s.key.as_str()).into_owned()
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
                tab.messages.push(ChatMessage::warning(msg));
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
        let launch_commandline = format!("cmd /c echo \x1b[2;37m{banner}\x1b[0m && {commandline}");
        let mut argv = vec![
            "new-tab".to_string(),
            "-c".to_string(),
            launch_commandline.clone(),
        ];
        if !s.title.is_empty() {
            argv.push("--title".to_string());
            argv.push(s.title.clone());
        }
        if let Some(ref cwd) = valid_cwd {
            argv.push("-d".to_string());
            argv.push(cwd.clone());
        }
        // Optimistic state flip: bump Historical/Ended -> Idle so a rapid
        // second Enter on the same row sees a non-terminal status and
        // skips this branch (idempotent: ResumeDispatched no-ops on live
        // rows). See `agent_sessions::SessionEvent::ResumeDispatched`.
        let resume_event =
            crate::agent_sessions::SessionEvent::ResumeDispatched { key: key.clone() };
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
            tab.messages.push(ChatMessage::warning(msg));
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
        let resume_event =
            crate::agent_sessions::SessionEvent::ResumeDispatched { key: key.clone() };
        self.agent_sessions.apply(resume_event.clone());
        self.publish_session_hook(resume_event);
        self.dispatch_session_resume_dispatched_rpc(&key);

        let mut params = serde_json::Map::new();
        params.insert(
            "session_id".to_string(),
            serde_json::Value::String(key.clone()),
        );
        if !cwd_string.is_empty() {
            params.insert(
                "cwd".to_string(),
                serde_json::Value::String(cwd_string.clone()),
            );
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
        let sid = agent_client_protocol::schema::v1::SessionId::new(sid.to_string());
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
        let sid = agent_client_protocol::schema::v1::SessionId::new(sid.to_string());
        let _ = self.master_request_tx.send(
            crate::protocol::acp::client::MasterExtRequest::SessionResumeDispatched {
                request_id,
                sid,
            },
        );
    }

    pub(crate) fn open_agents_view_for_tab(&mut self, tab_id: String) {
        {
            let tab = self.tab_mut(&tab_id);
            tab.agents_view.search_query.clear();
            tab.agents_view.search_focused = false;
        }
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
        tab.agents_view.search_query.clear();
        tab.agents_view.search_focused = false;
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
        tab.agents_view.focused_sid = Some(agent_client_protocol::schema::v1::SessionId::new(
            rows[idx].key.clone(),
        ));
    }

    fn update_agents_focus_for_tab(&mut self, tab_id: &str) {
        let rows = self.agents_rows_for_tab(tab_id);
        let selected = self
            .tab_sessions
            .get(tab_id)
            .and_then(|t| t.agents_list_state.selected());
        let focused = selected.and_then(|idx| {
            rows.get(idx)
                .map(|s| agent_client_protocol::schema::v1::SessionId::new(s.key.clone()))
        });
        self.tab_mut(tab_id).agents_view.focused_sid = focused;
    }

    fn reset_agents_search_selection(&mut self, tab_id: &str) {
        {
            let tab = self.tab_mut(tab_id);
            tab.agents_list_state.select(None);
            tab.agents_view.focused_sid = None;
        }
        self.restore_agents_selection(tab_id, 0);
    }

    fn agents_rows_for_tab(&self, tab_id: &str) -> Vec<crate::agent_sessions::AgentSession> {
        let filter = self.current_cli_filter();
        let origin = self.sessions_origin_filter;
        let query = self
            .tab_sessions
            .get(tab_id)
            .map(|tab| tab.agents_view.search_query.as_str())
            .unwrap_or_default();
        let folded_query = query.to_lowercase();
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
            rows.retain(|s| crate::ui::agents_view::matches_folded_query(s, &folded_query));
            rows
        } else {
            let mut rows: Vec<_> = self
                .agent_sessions
                .iter_sorted_with_filters(filter.as_ref(), origin)
                .into_iter()
                .cloned()
                .collect();
            rows.retain(|s| crate::ui::agents_view::matches_folded_query(s, &folded_query));
            rows
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
                tracing::info!(
                    "login: spawn_blocking returned, sending LoginComplete success={}",
                    success
                );
                let send_result = tx.send(AppEvent::LoginComplete {
                    agent_id: id,
                    success,
                    error,
                });
                tracing::info!("login: LoginComplete send result={:?}", send_result.is_ok());
            });
        }
    }

    pub(crate) fn show_copilot_auth_screen(&mut self) {
        let agent_id = "copilot";
        let profile = crate::agent_registry::lookup_profile_by_id(agent_id);
        let (enterprise_mode, enterprise_host) = copilot_enterprise_prefill(agent_id);
        self.current_agent_id = agent_id.to_string();
        self.mode = AppMode::Auth;
        self.setup = None;
        self.auth = Some(AuthState {
            agent_id: agent_id.to_string(),
            agent_name: profile.display_name.to_string(),
            login_command: crate::agent_check::build_login_cmd(agent_id, None),
            checking: false,
            status_message: String::new(),
            enterprise_mode,
            enterprise_host,
        });
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
            SetupOption::ChooseAgentSource => {
                self.request_agent_source_picker();
            }
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
                display_name: _,
            } => {
                if agent_id == "copilot" {
                    self.show_copilot_auth_screen();
                } else {
                    tracing::warn!(
                        target: "setup_key",
                        agent_id = %agent_id,
                        "ignoring SignIn option for non-Copilot agent"
                    );
                }
            }
            SetupOption::Retry => {
                // Re-run preflight detection and try to reconnect
                if let Some(ref setup) = self.setup {
                    let agent_id = setup.preflight.agent_id.clone();
                    if !agent_id.is_empty() {
                        if matches!(
                            self.current_agent_source,
                            crate::agent_source::AgentSource::Wsl { .. }
                        ) {
                            self.update_deferred_acp_agent(&agent_id);
                            self.state = ConnectionState::Connecting(
                                t!("connection.reconnecting").into_owned(),
                            );
                            self.preflight_setup_active = false;
                            if self.deferred_acp.is_some() {
                                self.pending_acp_start = true;
                            }
                            return;
                        }
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

    fn bound_tab_for_session(&self, session_id: &str) -> Option<String> {
        self.session_to_tab.get(session_id).cloned().or_else(|| {
            (self.current_tab().session_id.as_deref() == Some(session_id))
                .then(|| self.active_tab_key().to_string())
        })
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
        self.text_selection.snapshot_and_render(frame.buffer_mut());
        let action_overlay = crate::action_links::build_overlay(
            frame.buffer_mut(),
            &self.painted_completed_turn_action_links,
            &self.completed_turn_action_links,
        );
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

        crate::action_links::paint(terminal.backend_mut(), &action_overlay)?;
        self.painted_completed_turn_action_links
            .clone_from(&self.completed_turn_action_links);

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
            AppEvent::Mouse(_) => "mouse",
            AppEvent::Tick => "tick",
            AppEvent::Resize(_, _) => "resize",
            AppEvent::FocusChanged(_) => "focus_changed",
            AppEvent::ConnectionStage(_) => "connection_stage",
            AppEvent::CloudModelsAvailable(_) => "cloud_models_available",
            AppEvent::AgentConnected { .. } => "agent_connected",
            AppEvent::SessionAttached { .. } => "session_attached",
            AppEvent::UsageReported { .. } => "usage_reported",
            AppEvent::UsageCleared { .. } => "usage_cleared",
            AppEvent::ModelConfigUpdated { .. } => "model_config_updated",
            AppEvent::ModelSetCompleted { .. } => "model_set_completed",
            AppEvent::ModelSetFailed { .. } => "model_set_failed",
            AppEvent::SessionConfigUpdated { .. } => "session_config_updated",
            AppEvent::SessionConfigSetCompleted { .. } => "session_config_set_completed",
            AppEvent::SessionConfigSetFailed { .. } => "session_config_set_failed",
            AppEvent::TabError { .. } => "tab_error",
            AppEvent::TabSystemMessage { .. } => "tab_system_message",
            AppEvent::AgentPasteTextReady { .. } => "agent_paste_text_ready",
            AppEvent::AgentPasteTextFailed { .. } => "agent_paste_text_failed",
            AppEvent::PromptTemplateLoaded { .. } => "prompt_template_loaded",
            AppEvent::PromptTargetResolved { .. } => "prompt_target_resolved",
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
            AppEvent::ToolTerminalOutput { .. } => "tool_terminal_output",
            AppEvent::HideToolCall { .. } => "hide_tool_call",
            AppEvent::Plan { .. } => "plan",
            AppEvent::PermissionRequest { .. } => "permission_request",
            AppEvent::UserInputRequest { .. } => "user_input_request",
            AppEvent::CancelUserInputRequest { .. } => "cancel_user_input_request",
            AppEvent::SystemMessage(_) => "system_message",
            AppEvent::DebugPipeMessage(_) => "debug_pipe_message",
            AppEvent::WtEvent { .. } => "wt_event",
            AppEvent::AgentInstallComplete => "agent_install_complete",
            AppEvent::LoginProgress { .. } => "login_progress",
            AppEvent::LoginComplete { .. } => "login_complete",
            AppEvent::PostLoginAuthRecovery { .. } => "post_login_auth_recovery",
            AppEvent::AuthRecoveryTimedOut { .. } => "auth_recovery_timed_out",
            AppEvent::AgentSourcesDiscovered { .. } => "agent_sources_discovered",
            AppEvent::PreflightComplete(_) => "preflight_complete",
            AppEvent::AgentSessionEvent(_) => "agent_session_event",
            AppEvent::AliveSnapshotLoaded(_) => "alive_snapshot_loaded",
            AppEvent::AliveSessionAdded(_) => "alive_session_added",
            AppEvent::AliveSessionRemoved(_) => "alive_session_removed",
            AppEvent::AliveJoinUpgrade(_) => "alive_join_upgrade",
            AppEvent::SessionsChanged => "sessions_changed",
            AppEvent::AgentsSnapshotLoaded { .. } => "agents_snapshot_loaded",
            AppEvent::AgentsSnapshotFailed { .. } => "agents_snapshot_failed",
            AppEvent::RegisterBornBoundSession { .. } => "register_born_bound_session",
            AppEvent::MasterMutationCompleted { .. } => "master_mutation_completed",
            AppEvent::DirectTerminalActionProposal { .. } => "direct_terminal_action_proposal",
            AppEvent::DirectTerminalActionProposalCommit { .. } => {
                "direct_terminal_action_proposal_commit"
            }
            AppEvent::DirectTerminalActionProposalInvalidate { .. } => {
                "direct_terminal_action_proposal_invalidate"
            }
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
            tab.streaming_agent_text()
                .map(|text| text.chars().count())
                .unwrap_or(0),
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
        let reason = SetupReason::AgentError;
        let is_wsl_source = matches!(
            self.current_agent_source,
            crate::agent_source::AgentSource::Wsl { .. }
        );
        let agent_status = if is_wsl_source {
            crate::agent_check::AgentStatus {
                id: profile.id.to_string(),
                display_name: profile.display_name.to_string(),
                cli_found: true,
                cli_path: None,
                install_hint: profile.install_hint.to_string(),
                auth_hint: profile.auth_hint.to_string(),
                auto_installable: false,
            }
        } else {
            crate::agent_check::check_agent(profile.id)
        };
        let options = if is_wsl_source {
            build_setup_options(&reason, None)
        } else {
            build_setup_options(&reason, Some(&agent_status))
        };
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
                auth_status: CheckStatus::Failed(t!("system.authentication_failed").into_owned()),
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
                t!("setup.subtitle.copilot_auth", agent = profile.display_name).into_owned()
            } else {
                t!("setup.subtitle.agent_auth", agent = profile.display_name).into_owned()
            },
        });
        let tab = self.current_tab_mut();
        tab.messages.retain(|m| !matches!(m, ChatMessage::Error(_)));
    }

    fn handle_agent_paste_text(&mut self, params: &serde_json::Value) {
        let Some(target_tab) = self.agent_paste_target_tab(params) else {
            return;
        };

        if self.mode != AppMode::Chat {
            tracing::debug!(
                target: "agent_paste",
                mode = ?self.mode,
                tab_id = target_tab,
                "dropping paste because app is not in chat mode"
            );
            return;
        }

        if !self.agent_paste_input_is_live(target_tab) {
            tracing::debug!(
                target: "agent_paste",
                tab_id = target_tab,
                "dropping paste because chat input is not live"
            );
            return;
        }

        let Some(tx) = self.event_tx.clone() else {
            tracing::warn!(
                target: "agent_paste",
                tab_id = target_tab,
                "cannot read clipboard: app event channel is not initialized"
            );
            return;
        };
        let target_tab = target_tab.to_string();
        let generation = {
            let tab = self.tab_mut(&target_tab);
            tab.paste_generation = tab.paste_generation.wrapping_add(1);
            tab.paste_pending = true;
            tab.paste_generation
        };
        tokio::task::spawn_local(async move {
            let tab_for_result = target_tab.clone();
            let result =
                tokio::task::spawn_blocking(crate::win32::read_paste_string_from_clipboard).await;
            let event = match result {
                Ok(Ok(text)) => AppEvent::AgentPasteTextReady {
                    tab_id: tab_for_result,
                    generation,
                    text,
                },
                Ok(Err(e)) => AppEvent::AgentPasteTextFailed {
                    tab_id: tab_for_result,
                    generation,
                    error: e.to_string(),
                },
                Err(e) => AppEvent::AgentPasteTextFailed {
                    tab_id: tab_for_result,
                    generation,
                    error: e.to_string(),
                },
            };
            let _ = tx.send(event);
        });
    }

    fn agent_paste_target_tab<'a>(&self, params: &'a serde_json::Value) -> Option<&'a str> {
        let target_window = params
            .get("window_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let target_tab = params.get("tab_id").and_then(|v| v.as_str()).unwrap_or("");
        let target_pane = params.get("pane_id").and_then(|v| v.as_str()).unwrap_or("");
        let our_window = self.window_id.as_deref().unwrap_or("");
        let owner_tab = self.owner_tab_id.as_deref().unwrap_or("");
        let our_pane = self.pane_id.as_deref().unwrap_or("");
        let pane_matches = !target_pane.is_empty()
            && !our_pane.is_empty()
            && target_pane
                .trim_matches(['{', '}'])
                .eq_ignore_ascii_case(our_pane.trim_matches(['{', '}']));

        if target_window.is_empty()
            || target_tab.is_empty()
            || owner_tab.is_empty()
            || target_tab != owner_tab
            || !pane_matches
            || (!our_window.is_empty() && target_window != our_window)
        {
            tracing::debug!(
                target: "agent_paste",
                target_window,
                our_window,
                target_tab,
                owner_tab,
                target_pane,
                our_pane,
                "ignoring paste event not targeted at this helper"
            );
            return None;
        }

        Some(target_tab)
    }

    fn agent_paste_input_is_live(&self, target_tab: &str) -> bool {
        self.tab_sessions
            .get(target_tab)
            .map(|tab| tab.pane_open && tab.current_view == View::Chat && tab.input_has_nav_focus())
            .unwrap_or(false)
    }

    fn insert_agent_paste_text(&mut self, target_tab: &str, generation: u64, text: &str) {
        if self.mode != AppMode::Chat {
            if let Some(tab) = self.tab_sessions.get_mut(target_tab) {
                if tab.paste_generation == generation {
                    tab.paste_pending = false;
                }
            }
            tracing::debug!(
                target: "agent_paste",
                mode = ?self.mode,
                tab_id = target_tab,
                "dropping paste because app is not in chat mode"
            );
            return;
        }

        let text = normalize_agent_paste_text(text);
        let Some(tab) = self.tab_sessions.get_mut(target_tab) else {
            return;
        };
        if tab.paste_generation != generation {
            tracing::debug!(
                target: "agent_paste",
                tab_id = target_tab,
                generation,
                current_generation = tab.paste_generation,
                "dropping stale paste completion"
            );
            return;
        }
        {
            tab.paste_pending = false;
        }
        if text.is_empty() {
            tracing::debug!(target: "agent_paste", tab_id = target_tab, "ignoring empty paste");
            return;
        }

        let byte_len = text.len();
        let line_count = text.split('\n').count();
        let tab = self.tab_mut(target_tab);
        if !tab.pane_open || tab.current_view != View::Chat || !tab.input_has_nav_focus() {
            tracing::debug!(
                target: "agent_paste",
                tab_id = target_tab,
                pane_open = tab.pane_open,
                view = ?tab.current_view,
                input_live = tab.input_has_nav_focus(),
                byte_len,
                line_count,
                "dropping paste because chat input is not live"
            );
            return;
        }

        tab.insert_input_str(&text);
        tracing::info!(
            target: "agent_paste",
            tab_id = target_tab,
            byte_len,
            line_count,
            "inserted pasted text into agent input"
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompletedTurnHitKind {
    Triangle,
    UserInput,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CompletedTurnHitRegion {
    pub(crate) start_column: u16,
    pub(crate) end_column: u16,
    pub(crate) row: u16,
    pub(crate) turn_index: usize,
    pub(crate) kind: CompletedTurnHitKind,
}

impl CompletedTurnHitRegion {
    pub(crate) fn contains(self, column: u16, row: u16) -> bool {
        self.row == row && column >= self.start_column && column < self.end_column
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PressedCompletedTurn {
    pub(crate) tab_id: String,
    pub(crate) hit: CompletedTurnHitRegion,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompletedTurnClickRecord {
    pub(crate) tab_id: String,
    pub(crate) column: u16,
    pub(crate) row: u16,
    pub(crate) turn_index: usize,
    pub(crate) previous_selected_index: Option<usize>,
    pub(crate) previous_selection_pending: bool,
    pub(crate) previous_expanded: bool,
}

#[path = "app_events.rs"]
mod app_events;

impl App {
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

    /// Number of user-visible grapheme clusters in the active assistant segment.
    fn tab_visible_stream_len(tab: &TabSession) -> Option<usize> {
        crate::ui::chat::user_visible_stream_text(tab.streaming_agent_text()?)?;
        tab.streaming_grapheme_count()
    }

    /// True iff the current (visible) tab has streaming text that the reveal
    /// cursor hasn't caught up to yet. Used to gate `RevealTick` redraws.
    fn has_reveal_backlog(&self) -> bool {
        let tab = self.current_tab();
        matches!(Self::tab_visible_stream_len(tab), Some(len) if tab.reveal_graphemes < len)
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
            if tab.reveal_graphemes >= len {
                // Clamp down if the visible text shrank (e.g. a fenced JSON
                // block replaced the streamed prose).
                tab.reveal_graphemes = len;
                continue;
            }
            let backlog = len - tab.reveal_graphemes;
            let step = REVEAL_MIN_STEP.max(backlog / REVEAL_CATCHUP_FRAMES);
            tab.reveal_graphemes = (tab.reveal_graphemes + step).min(len);
        }
    }
}

#[path = "app_keys.rs"]
mod app_keys;

impl App {
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
            tab.messages.push(ChatMessage::warning(
                t!("system.image_not_supported").into_owned(),
            ));
            tab.scroll_to_bottom();
            return;
        }
        match crate::clipboard_image::read_clipboard_image() {
            Some(image) => {
                let tab = self.current_tab_mut();
                tab.insert_image_attachment(image);
            }
            None => {
                let tab = self.current_tab_mut();
                tab.messages.push(ChatMessage::info(
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
        if matches!(self.state, ConnectionState::Connecting(_)) {
            return true; // connecting shimmer
        }
        if self.agents_view_awaiting_snapshot() {
            return true; // agents-view "Loading" shimmer
        }
        self.current_tab().should_show_thinking()
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
        if !self.command_popup_visible() {
            return None;
        }
        // When the transport to master is lost, only /restart can run — so the
        // popup simply doesn't show the other commands (rather than greying
        // them). Collapse the candidate list to /restart if it's among the
        // search matches; otherwise show nothing (the typed query excludes
        // it, e.g. "/new"), and the Enter handler surfaces the reconnect hint.
        // Static command and move candidates borrow the tab's lists. Agent
        // candidates are filtered from the small cached available-agent list.
        let agent_candidates: Vec<_> = self.agent_command_candidates().collect();
        let candidates = if self.transport_lost {
            let filtered: Vec<&'static crate::commands::CommandSpec> = tab
                .command_popup_candidates
                .iter()
                .copied()
                .filter(|s| s.kind == crate::commands::CommandKind::Restart)
                .collect();
            if filtered.is_empty() {
                return None;
            }
            crate::ui::PopupCandidates::Commands(std::borrow::Cow::Owned(filtered))
        } else if !agent_candidates.is_empty() {
            crate::ui::PopupCandidates::Agents(agent_candidates)
        } else if !tab.move_position_candidates.is_empty() {
            crate::ui::PopupCandidates::MovePositions(tab.move_position_candidates.as_slice())
        } else {
            crate::ui::PopupCandidates::Commands(std::borrow::Cow::Borrowed(
                tab.command_popup_candidates.as_slice(),
            ))
        };
        Some(crate::ui::PopupState {
            candidates,
            selected: tab.command_popup_selected,
            pane_focused: self.pane_focused,
            command_query: tab.input.trim_start().strip_prefix('/').unwrap_or_default(),
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
        Some(self.model_display_name(&id))
    }

    fn confirmed_model_display(&self) -> Option<String> {
        let id = self
            .current_tab()
            .model_override
            .as_deref()
            .or(self.agent_current_model_id.as_deref())
            .or_else(|| self.selected_custom_model_id())
            .map(str::trim)
            .filter(|id| !id.is_empty())?;
        Some(self.model_display_name(id))
    }

    fn model_display_name(&self, id: &str) -> String {
        self.available_models
            .iter()
            .find(|model| model.id == id)
            .map(|model| model.name.clone())
            .unwrap_or_else(|| id.to_string())
    }

    /// Whether the command popup is *effectively* visible — i.e. actually
    /// rendered. This is the same condition `command_popup_state()` uses to
    /// decide whether to draw, so key handlers gate on the real on-screen
    /// state: in degraded mode the candidate list is filtered to `/restart`,
    /// so when the typed prefix excludes it (e.g. `/new`) nothing is drawn and
    /// this returns false — the Up/Down/Tab/Enter arms then fall through to
    /// their normal behavior instead of swallowing the key against an
    /// invisible popup.
    pub(super) fn command_popup_visible(&self) -> bool {
        if !self.current_tab().command_popup_visible()
            && self.agent_command_candidates().next().is_none()
        {
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

    fn agent_command_candidates(&self) -> impl Iterator<Item = &AvailableAgent> {
        let prefix = if self.transport_lost {
            None
        } else {
            commands::agent_id_prefix(&self.current_tab().input)
        };
        self.available_agents.iter().filter(move |agent| {
            prefix.is_some_and(|prefix| {
                agent
                    .id
                    .get(..prefix.len())
                    .is_some_and(|candidate| candidate.eq_ignore_ascii_case(prefix))
            })
        })
    }

    fn selected_agent_command_candidate(&self) -> Option<&AvailableAgent> {
        let selected = self.current_tab().command_popup_selected;
        self.agent_command_candidates()
            .take(selected.saturating_add(1))
            .last()
    }

    fn command_popup_candidate_count(&self) -> usize {
        let agent_count = self.agent_command_candidates().count();
        if agent_count > 0 {
            agent_count
        } else {
            let tab = self.current_tab();
            tab.command_popup_candidates.len() + tab.move_position_candidates.len()
        }
    }

    pub(super) fn command_popup_up(&mut self) {
        self.current_tab_mut().command_popup_up();
    }

    pub(super) fn command_popup_down(&mut self) {
        let candidate_count = self.command_popup_candidate_count();
        let tab = self.current_tab_mut();
        if tab.command_popup_selected + 1 < candidate_count {
            tab.command_popup_selected += 1;
        }
    }

    pub(crate) fn command_ghost_suffix(&self) -> Option<&str> {
        let tab = self.current_tab();
        if tab.cursor_pos != tab.input.len() {
            return None;
        }
        let prefix = commands::agent_id_prefix(&tab.input)?;
        let candidate = self.selected_agent_command_candidate()?;
        candidate
            .id
            .get(prefix.len()..)
            .filter(|suffix| !suffix.is_empty())
    }

    /// Per-frame state for the `/model` picker modal, or `None` when it's not
    /// open on the active tab. Cloud/native models are excluded from this
    /// in-pane surface and remain selectable through Settings.
    pub fn model_popup_state(&self) -> Option<crate::ui::ModelPopupState<'_>> {
        let tab = self.current_tab();
        if !tab.model_picker_open || self.model_picker_models.is_empty() {
            return None;
        }
        Some(crate::ui::ModelPopupState {
            models: &self.model_picker_models,
            selected: tab.model_picker_selected,
            pane_focused: self.pane_focused,
            disabled: self
                .model_picker_models
                .iter()
                .map(|model| !self.model_pick_enabled(&model.id))
                .collect(),
        })
    }

    pub fn config_popup_state(&self) -> Option<crate::ui::ConfigPopupState<'_>> {
        let tab = self.current_tab();
        if !tab.config_picker.is_open() {
            return None;
        }
        let options = self.current_session_config_options();
        if options.is_empty() {
            return None;
        }
        let value_option = tab
            .config_picker
            .option_id()
            .and_then(|config_id| options.iter().find(|option| option.id == config_id));
        Some(crate::ui::ConfigPopupState {
            options,
            value_option,
            selected: tab.config_picker.selected(),
            pending_config_id: tab.config_pending_id.as_deref(),
            pane_focused: self.pane_focused,
        })
    }

    pub fn agent_popup_state(&self) -> Option<crate::ui::AgentPopupState<'_>> {
        let tab = self.current_tab();
        if !tab.agent_picker_open || self.available_agents.is_empty() {
            return None;
        }
        Some(crate::ui::AgentPopupState {
            agents: &self.available_agents,
            selected: tab.agent_picker_selected,
            pane_focused: self.pane_focused,
            current_id: &self.current_agent_id,
            current_source: &self.current_agent_source,
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
            if !self.transport_lost {
                let selected_agent = self.selected_agent_command_candidate();
                if let Some(parsed) =
                    agent_command_on_enter(&self.current_tab().input, selected_agent)
                {
                    self.current_tab_mut().clear_input();
                    self.handle_slash_command(parsed);
                    return true;
                }
                if let Some(position) = self.current_tab().selected_move_position() {
                    let spec = commands::lookup("move").expect("/move is registered");
                    let parsed = ParsedCommand {
                        kind: CommandKind::Move,
                        spec,
                        rest: position.name.to_string(),
                    };
                    self.current_tab_mut().clear_input();
                    self.handle_slash_command(parsed);
                    return true;
                }
            }

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
                tab.messages.push(ChatMessage::warning(
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
        self.current_tab_mut()
            .messages
            .push(ChatMessage::warning(msg));
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
            CommandKind::Agent => self.cmd_agent(cmd.rest),
            CommandKind::Model => self.cmd_model(cmd.rest),
            CommandKind::Config => self.cmd_config(),
            CommandKind::Move => self.cmd_move(cmd.rest),
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
        tab.clear_completed_turns();
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
                .push(ChatMessage::success(t!("system.cancelled").into_owned()));
            tab.scroll_to_bottom();
        } else {
            let tab = self.current_tab_mut();
            tab.messages.push(ChatMessage::info(
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
            tab.messages.push(ChatMessage::warning(
                t!("system.busy_use_stop").into_owned(),
            ));
            tab.scroll_to_bottom();
            return;
        }
        let tab_id = self
            .tab_id
            .clone()
            .unwrap_or_else(|| DEFAULT_TAB_ID.to_string());
        let _ = self.new_session_tx.send(NewSessionForTab {
            tab_id,
            cwd: self.source_cwd.clone(),
        });
        if let Some(session_id) = self.current_tab().session_id.clone() {
            self.session_model_configs.remove(&session_id);
            self.session_config_options.remove(&session_id);
        }
        let tab = self.current_tab_mut();
        tab.clear_chat_history();
        tab.usage = None;
        tab.usage_staleness = crate::usage::UsageStaleness::default();
        tab.clear_completed_turns();
        tab.session_id = None;
        tab.scroll_to_bottom();
    }

    /// `/fix [hint]` — run the auto-fix prompt on demand against the active
    /// terminal pane. Reuses the error-triggered autofix pipeline
    /// (`PromptSubmission::is_autofix`): the agent receives the `auto-fix.md`
    /// instruction overlay plus the working pane's recent output, and any `hint` typed
    /// after `/fix` is appended as an extra steer.
    ///
    /// Differences from auto-triggered autofix (`maybe_trigger_autofix`):
    /// there is no failing-pane notification, so (1) the helper's captured
    /// source pane is resolved in the ACP client task; and
    /// (2) the turn context starts without a target and is late-bound once
    /// the client task resolves that working pane (`AppEvent::PromptTargetResolved` →
    /// `apply_prompt_target_resolved`), so `turn_execute_card` fills
    /// `Send.parent` with a real pane. The bottom-bar Pending pill is *not*
    /// armed — that UI is tied to a specific failing pane, and a command typed
    /// into the agent pane surfaces its result there directly.
    ///
    /// Refuses while a turn is in flight; the user should `/stop` first.
    fn cmd_fix(&mut self, in_flight: bool, hint: String) {
        if in_flight {
            let tab = self.current_tab_mut();
            tab.messages.push(ChatMessage::warning(
                t!("system.busy_use_stop").into_owned(),
            ));
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

        let source_pane_id = self.source_session_id.clone();
        let pane_context = PaneContext {
            pane_id: self.pane_id.clone(),
            tab_id: Some(target_tab_id.clone()),
            window_id: self.window_id.clone(),
            cwd: None,
            source_pane_id: source_pane_id.clone(),
        };

        let hint = hint.trim().to_string();
        let prompt = PromptSubmission::new_autofix(hint.clone(), Some(pane_context));
        let submitted = SubmittedPrompt {
            id: prompt.id,
            text: prompt.text.clone(),
            submitted_at_unix_s: prompt.submitted_at_unix_s,
            context: TurnContext {
                // Normally captured when the helper starts. If unavailable,
                // the ACP client resolves the active source and late-binds it.
                target_pane_id: source_pane_id,
            },
            autofix: Some(AutofixContext { generation }),
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
    /// plumbed back via [`AppEvent::PromptTargetResolved`]. The same event
    /// binds ordinary planner turns so execution never trusts a
    /// model-generated pane target.
    ///
    /// Routed by `prompt_id`: a superseded turn (the user fired a newer `/fix`)
    /// won't match, so a stale resolution is dropped. The event is emitted
    /// before the agent responds, so the patch lands while the turn is still
    /// `Submitted` — well before the fix card surfaces or the user executes it.
    fn apply_prompt_target_resolved(
        &mut self,
        tab_id: Option<String>,
        prompt_id: u64,
        pane_id: String,
    ) {
        if pane_id.is_empty() {
            return;
        }
        let preferred_key = tab_id.unwrap_or_else(|| self.active_tab_key().to_string());
        let key = self
            .tab_sessions
            .get(&preferred_key)
            .filter(|tab| {
                tab.turn
                    .prompt()
                    .is_some_and(|prompt| prompt.id == prompt_id)
            })
            .map(|_| preferred_key)
            .or_else(|| {
                self.tab_sessions.iter().find_map(|(key, tab)| {
                    tab.turn
                        .prompt()
                        .is_some_and(|prompt| prompt.id == prompt_id)
                        .then(|| key.clone())
                })
            });
        let Some(key) = key else {
            return;
        };
        let tab = self
            .tab_sessions
            .get_mut(&key)
            .expect("resolved tab exists");
        let Some(prompt) = tab.turn.prompt_mut() else {
            return;
        };
        if prompt.id != prompt_id {
            return;
        }
        prompt.context.target_pane_id = Some(pane_id.clone());
        tracing::info!(
            target: "pane_routing",
            tab = %key,
            prompt_id,
            pane = %pane_id,
            "bound authoritative prompt target pane",
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

    /// `/move <position>` — move only this tab's agent pane. Positions accept
    /// full names (`left`, `right`, `up`, `down`) or `l/r/u/d`. Bare or
    /// invalid input reopens the position completion popup.
    fn cmd_move(&mut self, position: String) {
        let Some(position) = commands::lookup_move_position(&position) else {
            self.current_tab_mut().replace_input("/move ".to_string());
            return;
        };

        self.current_tab_mut().agent_pane_position = Some(position.pane_position);
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
        self.session_model_configs.clear();
        self.session_config_options.clear();
        self.session_id.clear();
        for (_, tab) in self.tab_sessions.iter_mut() {
            tab.clear_chat_history();
            tab.usage = None;
            tab.usage_staleness = crate::usage::UsageStaleness::default();
            tab.clear_completed_turns();
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

    /// Recompute `rec_scroll.max` from the current card heights and the
    /// panel's available cards region. Called from layout.rs before
    /// `recommendations::render` so the renderer stays `&App` and any
    /// wheel-driven over-scroll is clamped before paint.
    pub fn sync_rec_scroll_max(&mut self, panel_width: u16, panel_height: u16) {
        let panel_cards_h = panel_height as usize;
        self.current_tab_mut().rec_viewport_height = panel_height;
        let Some(recs) = self.current_tab().turn.recommendations() else {
            return;
        };
        let total: usize = recs
            .choices
            .iter()
            .map(|c| ui::action_panel::recommendation_card_height(c, panel_width))
            .sum();
        self.current_tab_mut()
            .rec_scroll
            .set_max(total.saturating_sub(panel_cards_h));
    }

    fn clear_recommendations(&mut self) {
        self.current_tab_mut().clear_recommendations();
    }

    /// Keep the selected recommendation visible without moving a card that
    /// already fits in the full panel viewport. Compact mode renders only the
    /// selected card, so it never needs a canvas offset.
    fn scroll_rec_to_selected(&mut self, panel_width: u16) {
        let Some(recs) = self.current_tab().turn.recommendations().cloned() else {
            return;
        };
        let viewport_height = self.current_tab().rec_viewport_height as usize;
        if viewport_height < ui::card::CARD_MIN_SIZE as usize {
            self.current_tab_mut().rec_scroll.set(0);
            return;
        }

        let mut line_top = 0usize;
        for (idx, choice) in recs.choices.iter().enumerate() {
            let card_h = ui::action_panel::recommendation_card_height(choice, panel_width);
            if idx == self.current_tab().selected_recommendation {
                let offset = self.current_tab().rec_scroll.offset;
                // `card_h` includes the inter-card gap, so subtracting it
                // yields the rendered card's exclusive bottom row.
                let rendered_bottom_exclusive = line_top.saturating_add(card_h.saturating_sub(1));
                let viewport_bottom = offset.saturating_add(viewport_height);
                if line_top < offset || rendered_bottom_exclusive > viewport_bottom {
                    self.current_tab_mut().rec_scroll.set(line_top);
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
        self.pressed_completed_turn = None;
        self.last_completed_turn_click = None;
        self.pressed_input_dialog_tab = None;
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

        let (models, current) = self
            .current_tab()
            .session_id
            .as_deref()
            .and_then(|session_id| self.session_model_configs.get(session_id))
            .cloned()
            .unwrap_or_default();
        self.agent_models = models;
        self.agent_current_model_id = current;
        self.rebuild_model_catalog_from_agent_state();
        self.publish_agent_status();

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
        if let Some(session_id) = removed.as_ref().and_then(|tab| tab.session_id.as_ref()) {
            self.session_model_configs.remove(session_id);
            self.session_config_options.remove(session_id);
        }
        self.session_to_tab.retain(|_, tab| tab != closed_tab_id);

        // Tell the ACP client to release the binding for this tab so
        // master can `session/close` the orphaned session.
        // Without this, every closed tab leaves a live ACP session
        // behind on the CLI side — `tab_sessions` and `session_to_tab`
        // are cleaned above but the ACP layer's own `tab_to_session`
        // map and the agent's session state are not.
        self.request_tab_session_close(closed_tab_id);

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

    /// Ask master to close the ACP session for a destroyed stable tab id
    /// without touching this helper's local per-window tab state. WT events
    /// are process-wide, so helpers in other windows use this path to cover
    /// the race where every helper in the owning window exits before handling
    /// `tab_closed`.
    pub(crate) fn request_tab_session_close(&self, closed_tab_id: &str) {
        let _ = self.drop_session_tx.send(DropSessionRequest {
            tab_id: closed_tab_id.to_string(),
            notify_master: true,
        });
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
            entry.invalidate_pending_paste();
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
    ///   - The ACP client task drops the binding in `tab_to_session` and
    ///     cancels any in-flight prompt for the old SessionId. The master
    ///     consumes the same process-wide WT event and owns the physical
    ///     close; the next prompt lazily creates a fresh ACP session.
    /// Unlike `drop_tab_session`, this preserves the HashMap key — the
    /// next tab_changed back into this tab finds an empty-but-present
    /// `TabSession` and just renders an empty chat.
    fn reset_tab_session_for(&mut self, tab_id: &str) {
        let mut removed_session_id = None;
        // Same wipe as the `/clear` slash command: clear in-flight chat state
        // via `clear_chat_history` AND the completed-turn history that
        // `clear_chat_history` deliberately leaves alone.
        if let Some(tab) = self.tab_sessions.get_mut(tab_id) {
            removed_session_id = tab.session_id.take();
            tab.clear_chat_history();
            tab.usage = None;
            tab.usage_staleness = crate::usage::UsageStaleness::default();
            tab.clear_completed_turns();
            tab.scroll_to_bottom();
        }
        if let Some(session_id) = removed_session_id {
            self.session_model_configs.remove(&session_id);
            self.session_config_options.remove(&session_id);
        }

        // Prune the reverse SessionId → tab routing so late ACP chunks for
        // the dropped session can't land on this tab's slot.
        self.session_to_tab.retain(|_, t| t != tab_id);

        // Ask the ACP client task to release the binding for this tab.
        let _ = self.drop_session_tx.send(DropSessionRequest {
            tab_id: tab_id.to_string(),
            notify_master: false,
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

    fn focus_next_recommendation_action(&mut self) {
        let button_count = self.button_count_for_selected();
        let tab = self.current_tab_mut();
        tab.selected_button = (tab.selected_button + 1) % button_count;
    }

    fn focus_previous_recommendation_action(&mut self) {
        let button_count = self.button_count_for_selected();
        let tab = self.current_tab_mut();
        tab.selected_button = (tab.selected_button + button_count - 1) % button_count;
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

#[path = "app_turn.rs"]
mod app_turn;

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

#[path = "app_status_projection.rs"]
mod app_status_projection;

fn build_switch_agent_event(
    window_id: &str,
    tab_id: &str,
    agent_id: &str,
    source: &crate::agent_source::AgentSource,
) -> String {
    serde_json::json!({
        "type": "event",
        "method": "switch_agent",
        "params": {
            "window_id": window_id,
            "tab_id": tab_id,
            "agent_id": agent_id,
            "agent_source": source.kind(),
            "wsl_distro": source.distro(),
        }
    })
    .to_string()
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

fn normalize_agent_paste_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\r' => {
                if matches!(chars.peek(), Some('\n')) {
                    chars.next();
                }
                out.push('\n');
            }
            '\n' | '\u{0085}' | '\u{2028}' | '\u{2029}' => out.push('\n'),
            '\t' => out.push('\t'),
            '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' => {}
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

fn now_unix_s() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
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
#[path = "app_tests.rs"]
mod tests;
