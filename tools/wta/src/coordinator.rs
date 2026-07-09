use std::collections::BTreeSet;
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};

use crate::app::AppEvent;
use crate::shell::ShellManager;

use crate::agent_registry::{self, PromptFlag};

#[derive(Debug, Clone, Serialize)]
pub struct SupportedDelegateAgent {
    pub id: String,
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone)]
pub struct DelegateAgentRuntime {
    pub id: String,
    pub name: String,
    pub description: String,
    pub commandline: String,
    pub prompt_delivery: DelegatePromptDelivery,
    pub model: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DelegatePromptDelivery {
    LaunchThenSend,
    LaunchWithStartupPrompt,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecommendationSet {
    #[serde(default)]
    pub recommended_choice: Option<usize>,
    pub choices: Vec<RecommendationChoice>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecommendationChoice {
    pub choice: usize,
    pub title: String,
    #[serde(default)]
    pub rationale: String,
    pub actions: Vec<RecommendedAction>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenTarget {
    Tab,
    Panel,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RecommendedAction {
    Send {
        #[serde(default)]
        parent: String,
        input: String,
    },
    OpenAndSend {
        target: OpenTarget,
        #[serde(default)]
        parent: Option<String>,
        input: String,
        #[serde(default)]
        agent: Option<String>,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default)]
        title: Option<String>,
        /// Split direction for panel target: "right" | "left" | "up" | "down" | "auto".
        /// Ignored for tab target. None = COM default ("right" historically; the
        /// fixed wtcli passes "automatic" when neither is set).
        #[serde(default)]
        direction: Option<String>,
        #[serde(default)]
        profile: Option<String>,
    },
    Open {
        target: OpenTarget,
        #[serde(default)]
        parent: Option<String>,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default)]
        title: Option<String>,
        /// Split direction for panel target: "right" | "left" | "up" | "down" | "auto".
        /// Ignored for tab target.
        #[serde(default)]
        direction: Option<String>,
        #[serde(default)]
        profile: Option<String>,
    },
}

/// Wraps a chosen recommendation with execution options (e.g. insert-only mode).
#[derive(Debug, Clone)]
pub struct ChoiceExecution {
    pub choice: RecommendationChoice,
    /// When true, Send actions paste text without a trailing Enter (insert-only).
    pub insert_only: bool,
}

pub fn default_supported_delegate_agents() -> Vec<SupportedDelegateAgent> {
    agent_registry::supported_delegate_agents()
}

pub fn default_delegate_agent_runtimes(
    delegate_agent_cmd: Option<&str>,
    agent_cmd: Option<&str>,
    delegate_model: Option<&str>,
) -> Vec<DelegateAgentRuntime> {
    let commandline = resolve_delegate_runtime_commandline(delegate_agent_cmd, agent_cmd)
        .unwrap_or_else(|| agent_registry::KNOWN_AGENTS[0].id.to_string());
    let (id, name) = derive_agent_identity(&commandline);
    vec![DelegateAgentRuntime {
        id,
        name,
        description: format!(
            "Launches `{}` directly in a new terminal target with an interactive startup task prompt.",
            commandline.split_whitespace().next().unwrap_or("agent")
        ),
        commandline,
        prompt_delivery: DelegatePromptDelivery::LaunchWithStartupPrompt,
        model: delegate_model.filter(|m| !m.is_empty()).map(str::to_string),
    }]
}

/// Derive a (id, display_name) pair from a delegate agent commandline.
fn derive_agent_identity(commandline: &str) -> (String, String) {
    let first_token = commandline
        .split_whitespace()
        .next()
        .unwrap_or(commandline);
    let unquoted = first_token.trim_matches('"');
    let profile = agent_registry::lookup_profile(unquoted);
    if profile.id != "unknown" {
        return (profile.id.to_string(), profile.display_name.to_string());
    }
    // Unknown agent — use the basename as both id and name.
    let basename = unquoted
        .rsplit(|ch: char| ch == '\\' || ch == '/')
        .next()
        .unwrap_or(unquoted);
    let id = basename
        .strip_suffix(".exe")
        .unwrap_or(basename)
        .to_ascii_lowercase();
    (id.clone(), id)
}

pub fn parse_recommendation_set(text: &str) -> Result<RecommendationSet> {
    let json = extract_json_code_block(text)
        .or_else(|| extract_first_json_object(text))
        .context("no recommendation JSON block found")?;

    let mut parsed: RecommendationSet =
        serde_json::from_str(json).context("failed to parse recommendation JSON")?;
    validate_recommendation_set(&parsed)?;
    parsed.choices.sort_by_key(|c| c.choice);
    Ok(parsed)
}

/// The result of parsing an autofix response.
#[derive(Debug, Clone)]
pub enum AutofixDecision {
    /// AI found a single-command fix.
    Fix(RecommendationSet),
    /// AI cannot auto-fix but has a useful explanation/suggestion. The caller
    /// should surface `explanation` in the agent pane chat history and tell
    /// the bottom bar to show a "Suggestion ready — open agent pane" indicator.
    Explain { title: String, explanation: String },
    /// AI decided no fix is appropriate; caller should silently clear state.
    /// The `explain` action makes this rare — Ignore is now a fail-safe for
    /// malformed responses or empty explanations.
    Ignore,
}

/// Parse a response from the minimal autofix prompt.
///
/// Expected formats:
///   {"action": "fix",     "title": "...", "command": "...",     "rationale": "..."}
///   {"action": "explain", "title": "...", "explanation": "..."}
///   {"action": "ignore"}                            // legacy fallback
///
/// Returns `AutofixDecision::Ignore` for unrecognised JSON or missing required
/// fields (fail-safe: never leave a stale Pending bar).
pub fn parse_autofix_response(text: &str) -> AutofixDecision {
    let json = match extract_json_code_block(text).or_else(|| extract_first_json_object(text)) {
        Some(j) => j,
        None => {
            tracing::warn!(target: "autofix", "no JSON in autofix response, ignoring");
            return AutofixDecision::Ignore;
        }
    };

    let value: serde_json::Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(target: "autofix", "failed to parse autofix JSON: {e}, ignoring");
            return AutofixDecision::Ignore;
        }
    };

    match value.get("action").and_then(|v| v.as_str()) {
        Some("fix") => {
            let command = match value.get("command").and_then(|v| v.as_str()) {
                Some(c) if !c.trim().is_empty() => c.to_string(),
                _ => {
                    tracing::warn!(target: "autofix", "fix response missing 'command', ignoring");
                    return AutofixDecision::Ignore;
                }
            };
            let title = value
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("Fix")
                .to_string();
            let rationale = value
                .get("rationale")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            AutofixDecision::Fix(RecommendationSet {
                recommended_choice: Some(1),
                choices: vec![RecommendationChoice {
                    choice: 1,
                    title,
                    rationale,
                    actions: vec![RecommendedAction::Send {
                        parent: String::new(),
                        input: command,
                    }],
                }],
            })
        }
        Some("explain") => {
            let explanation = match value.get("explanation").and_then(|v| v.as_str()) {
                Some(e) if !e.trim().is_empty() => e.to_string(),
                _ => {
                    tracing::warn!(target: "autofix", "explain response missing 'explanation', ignoring");
                    return AutofixDecision::Ignore;
                }
            };
            let title = value
                .get("title")
                .and_then(|v| v.as_str())
                .filter(|t| !t.trim().is_empty())
                .unwrap_or("Suggestion")
                .to_string();
            AutofixDecision::Explain { title, explanation }
        }
        Some("ignore") | None => AutofixDecision::Ignore,
        Some(other) => {
            tracing::warn!(target: "autofix", "unknown autofix action {other:?}, ignoring");
            AutofixDecision::Ignore
        }
    }
}

/// Filter out choices that target the coordinator's own pane.
/// Returns the filtered set. If all choices are removed, returns an error.
pub fn validate_recommendation_set_for_coordinator_target(
    set: &RecommendationSet,
    coordinator_target: Option<&str>,
) -> Result<RecommendationSet> {
    let Some(coordinator_target) = coordinator_target
        .map(str::trim)
        .filter(|id| !id.is_empty())
    else {
        return Ok(set.clone());
    };

    let filtered: Vec<RecommendationChoice> = set
        .choices
        .iter()
        .filter(|choice| {
            !choice.actions.iter().any(|action| {
                matches!(action, RecommendedAction::Send { parent, .. } if parent == coordinator_target)
            })
        })
        .cloned()
        .collect();

    if filtered.is_empty() {
        bail!(
            "all choices target the current coordinator pane {}",
            coordinator_target
        );
    }

    // Adjust recommended_choice if the original was filtered out.
    let recommended_choice = set.recommended_choice.filter(|rc| {
        filtered.iter().any(|c| c.choice == *rc)
    });

    Ok(RecommendationSet {
        recommended_choice,
        choices: filtered,
    })
}

pub fn recommended_choice_index(set: &RecommendationSet) -> usize {
    if let Some(choice_no) = set.recommended_choice {
        if let Some(idx) = set
            .choices
            .iter()
            .position(|choice| choice.choice == choice_no)
        {
            return idx;
        }
    }
    0
}

pub async fn run_recommendation_executor(
    mut rx: mpsc::UnboundedReceiver<ChoiceExecution>,
    event_tx: mpsc::UnboundedSender<AppEvent>,
    shell_mgr: Arc<ShellManager>,
    // Shared so the App can hot-swap the configured delegate agent/model on
    // an `agent_config_changed` settings update without respawning anything.
    // Snapshotted (cloned) under the lock per choice — the table is tiny
    // (one entry) and the lock is never held across an await.
    delegate_agents: Arc<std::sync::Mutex<Vec<DelegateAgentRuntime>>>,
) {
    while let Some(exec) = rx.recv().await {
        let delegate_agents = delegate_agents.lock().unwrap().clone();
        match execute_choice(&exec.choice, exec.insert_only, &shell_mgr, &delegate_agents, &event_tx).await {
            Ok(()) => {}
            Err(err) => {
                let err_str = format!("{:#}", err);
                let _ = event_tx.send(AppEvent::SystemMessage(
                    t!(
                        "system.choice_execution_failed",
                        choice = exec.choice.choice,
                        error = err_str.as_str()
                    )
                    .into_owned(),
                ));
            }
        }
    }
}

async fn execute_choice(
    choice: &RecommendationChoice,
    insert_only: bool,
    shell_mgr: &ShellManager,
    delegate_agents: &[DelegateAgentRuntime],
    event_tx: &mpsc::UnboundedSender<AppEvent>,
) -> Result<()> {
    for action in &choice.actions {
        match action {
            RecommendedAction::Send { parent, input } => {
                ensure_non_empty("parent", parent)?;
                ensure_non_empty("input", input)?;
                coordinator_log(&format!(
                    "send begin parent={} insert_only={} input_chars={}",
                    parent,
                    insert_only,
                    input.chars().count(),
                ));
                let action_label = if insert_only { "Inserting" } else { "Sending" };
                let _ = event_tx.send(AppEvent::ExecutionInfo(format!(
                    "{} input to pane {}.",
                    action_label, parent
                )));
                let payload = if insert_only {
                    // TerminalPage::SendProtocolInput replaces \n with \r (Enter).
                    // Trim trailing newlines so insert-only mode doesn't accidentally
                    // execute the command in the target pane.
                    input.trim_end_matches(['\r', '\n']).to_string()
                } else {
                    format!("{input}\r")
                };
                let result = shell_mgr
                    .wt_send_input(parent, &payload)
                    .await
                    .with_context(|| format!("failed to send input to pane {}", parent))?;
                let done_label = if insert_only { "Inserted" } else { "Sent" };
                coordinator_log(&format!(
                    "send success parent={} response={}",
                    parent,
                    summarize_json_for_log(&result)
                ));
                let _ = event_tx.send(AppEvent::ExecutionInfo(format!(
                    "{} input to pane {}.",
                    done_label, parent
                )));
                // Run is "the user dispatched a command to pane X" — follow
                // focus to that pane so they can keep typing / observe output
                // without an extra click. Best-effort: log and ignore on
                // failure (focus is UX-nice, not correctness-critical).
                if !insert_only {
                    if let Err(err) = shell_mgr.wt_focus_pane(parent).await {
                        coordinator_log(&format!(
                            "send focus skipped parent={} error={}",
                            parent, err
                        ));
                    }
                }
            }
            RecommendedAction::OpenAndSend {
                target,
                parent,
                input,
                agent,
                cwd,
                title,
                direction,
                profile,
            } => {
                // Resolve profile: prefer explicit from LLM, fall back to active pane's profile
                let active_pane = shell_mgr.wt_get_active_pane().await.ok();
                let profile = resolve_agent_profile(profile.as_deref(), active_pane.as_ref());

                ensure_non_empty("input", input)?;
                let runtime = match agent.as_deref() {
                    Some(agent) => Some(lookup_delegate_agent(delegate_agents, agent)?),
                    None => None,
                };
                let runtime_name = runtime.map(|agent| agent.name.as_str());
                let delivery_mode = runtime
                    .map(|agent| agent.prompt_delivery)
                    .unwrap_or(DelegatePromptDelivery::LaunchThenSend);
                let target_label = open_target_label(target);
                coordinator_log(&format!(
                    "open_and_send begin target={} parent={:?} agent={:?} title={:?} direction={:?} delivery_mode={} input_chars={}",
                    target_label,
                    parent,
                    agent,
                    title,
                    direction,
                    delegate_prompt_delivery_label(delivery_mode),
                    input.chars().count(),
                ));
                let _ = event_tx.send(AppEvent::ExecutionInfo(match runtime_name {
                    Some(name) => format!("Opening {} for {}.", target_label, name),
                    None => format!("Opening {}.", target_label),
                }));
                let commandline = runtime
                    .map(|runtime| build_delegate_launch_commandline(runtime, Some(input), None))
                    .transpose()?;
                let pane_id = match target {
                    OpenTarget::Tab => {
                        // Launch the delegate agent directly as the tab process.
                        let result = shell_mgr
                            .wt_create_tab(
                                commandline.as_deref(),
                                cwd.as_deref(),
                                title.as_deref().or(runtime_name),
                                profile.as_deref(),
                            )
                            .await
                            .context("failed to create tab")?;
                        coordinator_log(&format!(
                            "open_and_send create_tab response={}",
                            summarize_json_for_log(&result)
                        ));
                        resolve_created_pane_id(&result, "create_tab")?
                    }
                    OpenTarget::Panel => {
                        let parent = required_parent(parent.as_deref(), "open_and_send")?;
                        let result = shell_mgr
                            .wt_split_pane(
                                parent,
                                commandline.as_deref(),
                                cwd.as_deref(),
                                direction.as_deref(),
                                None,
                                profile.as_deref(),
                            )
                            .await
                            .with_context(|| format!("failed to split pane {}", parent))?;
                        coordinator_log(&format!(
                            "open_and_send split_pane parent={} direction={:?} response={}",
                            parent,
                            direction,
                            summarize_json_for_log(&result)
                        ));
                        resolve_created_pane_id(&result, "split_pane")?
                    }
                };
                coordinator_log(&format!(
                    "open_and_send resolved target={} pane_id={}",
                    target_label, pane_id
                ));
                let _ = event_tx.send(AppEvent::ExecutionInfo(format!(
                    "Opened {} pane {}.",
                    target_label, pane_id
                )));
                if matches!(delivery_mode, DelegatePromptDelivery::LaunchThenSend) {
                    send_input_to_new_pane(shell_mgr, &pane_id, input, event_tx).await?;
                } else {
                    coordinator_log(&format!(
                        "open_and_send startup_prompt_delivery target={} pane_id={}",
                        target_label, pane_id
                    ));
                    // commandline bakes in the user prompt — trace only.
                    tracing::trace!(target: "coordinator.content", commandline = ?commandline, "open_and_send commandline");
                    let _ = event_tx.send(AppEvent::ExecutionInfo(format!(
                        "Passed startup prompt to pane {} on launch.",
                        pane_id
                    )));
                }
            }
            RecommendedAction::Open {
                target,
                parent,
                cwd,
                title,
                direction,
                profile,
            } => {
                // Resolve profile: prefer explicit from LLM, fall back to active pane's profile
                let active_pane = shell_mgr.wt_get_active_pane().await.ok();
                let profile = resolve_agent_profile(profile.as_deref(), active_pane.as_ref());

                let target_label = open_target_label(target);
                coordinator_log(&format!(
                    "open begin target={} parent={:?} title={:?} direction={:?}",
                    target_label, parent, title, direction
                ));
                let _ = event_tx.send(AppEvent::ExecutionInfo(format!(
                    "Opening {}.",
                    target_label
                )));
                let pane_id = match target {
                    OpenTarget::Tab => {
                        let result = shell_mgr
                            .wt_create_tab(None, cwd.as_deref(), title.as_deref(), profile.as_deref())
                            .await
                            .context("failed to create tab")?;
                        coordinator_log(&format!(
                            "open create_tab response={}",
                            summarize_json_for_log(&result)
                        ));
                        resolve_created_pane_id(&result, "create_tab")?
                    }
                    OpenTarget::Panel => {
                        let parent = required_parent(parent.as_deref(), "open")?;
                        let result = shell_mgr
                            .wt_split_pane(parent, None, cwd.as_deref(), direction.as_deref(), None, profile.as_deref())
                            .await
                            .with_context(|| format!("failed to split pane {}", parent))?;
                        coordinator_log(&format!(
                            "open split_pane parent={} direction={:?} response={}",
                            parent,
                            direction,
                            summarize_json_for_log(&result)
                        ));
                        resolve_created_pane_id(&result, "split_pane")?
                    }
                };
                coordinator_log(&format!(
                    "open resolved target={} pane_id={}",
                    target_label, pane_id
                ));
                let _ = event_tx.send(AppEvent::ExecutionInfo(format!(
                    "Opened {} pane {}.",
                    target_label, pane_id
                )));
            }
        }
    }

    Ok(())
}

fn validate_recommendation_set(set: &RecommendationSet) -> Result<()> {
    if !(1..=3).contains(&set.choices.len()) {
        bail!("expected 1 to 3 choices, got {}", set.choices.len());
    }

    let mut seen = BTreeSet::new();
    for choice in &set.choices {
        if !(1..=3).contains(&choice.choice) {
            bail!("choice numbers must be 1..=3");
        }
        if !seen.insert(choice.choice) {
            bail!("duplicate choice number {}", choice.choice);
        }
        ensure_non_empty("title", &choice.title)?;
        if choice.actions.is_empty() {
            bail!("choice {} has no actions", choice.choice);
        }
        for action in &choice.actions {
            validate_action(action)?;
        }
    }

    Ok(())
}

fn validate_action(action: &RecommendedAction) -> Result<()> {
    match action {
        RecommendedAction::Send { parent: _, input } => {
            // parent may be empty for auto-fix actions (filled in at execution time)
            ensure_non_empty("input", input)?;
        }
        RecommendedAction::OpenAndSend {
            target,
            parent,
            input,
            agent,
            direction,
            ..
        } => {
            ensure_non_empty("input", input)?;
            if let Some(parent) = parent.as_deref() {
                ensure_non_empty("parent", parent)?;
            }
            if let Some(agent) = agent.as_deref() {
                ensure_non_empty("agent", agent)?;
            }
            if matches!(target, OpenTarget::Panel) {
                required_parent(parent.as_deref(), "open_and_send")?;
            }
            validate_direction(direction.as_deref(), target)?;
        }
        RecommendedAction::Open {
            target,
            parent,
            direction,
            ..
        } => {
            if let Some(parent) = parent.as_deref() {
                ensure_non_empty("parent", parent)?;
            }
            if matches!(target, OpenTarget::Panel) {
                required_parent(parent.as_deref(), "open")?;
            }
            validate_direction(direction.as_deref(), target)?;
        }
    }

    Ok(())
}

fn validate_direction(direction: Option<&str>, target: &OpenTarget) -> Result<()> {
    let Some(value) = direction else {
        return Ok(());
    };
    if value.is_empty() {
        bail!("field 'direction' must not be empty");
    }
    if matches!(target, OpenTarget::Tab) {
        bail!("field 'direction' is only valid when target is 'panel'");
    }
    match value {
        "right" | "left" | "up" | "down" | "auto" | "automatic" => Ok(()),
        other => bail!(
            "invalid direction {:?}; expected right|left|up|down|auto",
            other
        ),
    }
}

fn lookup_delegate_agent<'a>(
    delegate_agents: &'a [DelegateAgentRuntime],
    id: &str,
) -> Result<&'a DelegateAgentRuntime> {
    // Try exact match first, then fall back to the first configured runtime.
    // The ACP agent may request "copilot" but the user configured "codex" —
    // honour the user's delegate setting.
    delegate_agents
        .iter()
        .find(|agent| agent.id == id)
        .or_else(|| delegate_agents.first())
        .ok_or_else(|| anyhow!("no delegate agent configured"))
}

/// Build the delegate launch command line, optionally pinning a session id.
/// When `session_id` is `Some` and the resolved agent advertises
/// `new_session_id_flag`, append `<flag> <session_id>` so WTA controls the id
/// the CLI writes its session under (enables hook-independent binding).
pub fn build_delegate_launch_commandline_with_session(
    runtime: &DelegateAgentRuntime,
    input: Option<&str>,
    session_id: Option<&str>,
) -> Result<String> {
    build_delegate_launch_commandline(runtime, input, session_id)
}

fn build_delegate_launch_commandline(
    runtime: &DelegateAgentRuntime,
    input: Option<&str>,
    session_id: Option<&str>,
) -> Result<String> {
    let commandline = runtime.commandline.trim();
    if commandline.is_empty() {
        bail!("delegate agent runtime commandline is empty");
    }
    // Resolve bare names (e.g. "claude" → "claude.exe") at launch time so we
    // always see the current PATH, not a stale snapshot from process startup.
    let resolved = resolve_commandline_executable(commandline);

    // Identify the agent profile from the *raw* command line so flag lookups
    // (model, session id) see the real CLI -- not a later `cmd /c` wrapper, and
    // not the PATH-resolved exe (which may be a quoted path containing spaces
    // that a naive whitespace split would mangle into `"C:\Program`).
    // `resolve_agent_id_from_cmd` tokenizes correctly and also recognizes
    // adapter launches (e.g. `npx -y @agentclientprotocol/claude-agent-acp` -> claude).
    // Using the same raw command line that `delegate_with_context` inspects to
    // decide whether to pin a session id keeps that decision and the flag we
    // append here in agreement -- otherwise we could register a born-bound id
    // that the CLI was never actually launched with.
    let profile = agent_registry::lookup_profile_by_id(agent_registry::resolve_agent_id_from_cmd(
        commandline,
    ));

    // If a model is configured, append --model <value> using the agent's model flags.
    let with_model = if let Some(ref model) = runtime.model {
        if let Some(flag) = profile.model_flags.first() {
            format!("{} {} {}", resolved, flag, model)
        } else {
            resolved.clone()
        }
    } else {
        resolved.clone()
    };

    // Pin a caller-chosen session id when the agent supports it, so the launched
    // CLI writes its session under a known id (enables hook-independent binding).
    // Appended here — before any `cmd /c` wrap below — so the flag lands on the
    // agent CLI, not on `cmd`.
    let with_session = match (session_id, profile.new_session_id_flag) {
        (Some(sid), Some(flag)) => format!("{} {} {}", with_model, flag, sid),
        _ => with_model,
    };
    let resolved_ref = with_session.as_str();

    let raw = match input {
        // Interactive (no prompt): launch the bare agent CLI regardless of
        // the configured prompt-delivery mode.
        None => resolved_ref.to_string(),
        Some(input) => match runtime.prompt_delivery {
            DelegatePromptDelivery::LaunchThenSend => resolved_ref.to_string(),
            DelegatePromptDelivery::LaunchWithStartupPrompt => {
                ensure_non_empty("input", input)?;
                build_delegate_startup_prompt_commandline(resolved_ref, input)?
            }
        },
    };
    // .cmd/.bat shims (e.g. npm-installed CLIs) can't be launched directly
    // via CreateProcess — wrap with cmd /c so the command interpreter finds them.
    if needs_shell_launch(&resolved) {
        Ok(format!("cmd /c {}", raw))
    } else {
        Ok(raw)
    }
}

/// Resolve the first token (executable) of a commandline using the agent CLI
/// registry's PATH search order.  Returns the commandline with the resolved
/// executable, or the original commandline unchanged.
fn resolve_commandline_executable(commandline: &str) -> String {
    let tokens = split_windows_commandline(commandline);
    if let Some(first) = tokens.first() {
        let resolved = agent_registry::resolve_bare_agent_name(first);
        if resolved != *first {
            let mut parts = vec![resolved];
            parts.extend(tokens[1..].iter().cloned());
            let args: Vec<&str> = parts.iter().map(String::as_str).collect();
            return join_windows_commandline(&args);
        }
    }
    commandline.to_string()
}

fn resolve_delegate_runtime_commandline(
    delegate_agent_cmd: Option<&str>,
    agent_cmd: Option<&str>,
) -> Option<String> {
    if let Some(commandline) = delegate_agent_cmd
        .map(str::trim)
        .filter(|cmd| !cmd.is_empty())
    {
        // Strip ACP flags if present — the delegate path uses CLI mode, not ACP.
        // This handles cases where the same custom entry is used for both ACP
        // (agent pane) and delegation (? prompt).
        if let Some(stripped) = agent_registry::strip_acp_flags_for_delegate(commandline) {
            return Some(stripped);
        }
        return Some(commandline.to_string());
    }

    // Strip ACP-specific flags from the agent command to get a clean delegate command.
    if let Some(delegate) = agent_registry::strip_acp_flags_for_delegate(agent_cmd?) {
        return Some(delegate);
    }

    // For non-ACP agents, derive the delegate command from the base executable name.
    let tokens = split_windows_commandline(agent_cmd?);
    let command = tokens.first()?;
    Some(command.clone())
}

/// Returns true if the command needs a `cmd /c` wrapper to run via CreateProcess.
/// .exe files and shells (cmd, powershell, pwsh) don't need wrapping.
/// Bare names like "codex" need wrapping if they resolve to .cmd/.bat on PATH.
fn needs_cmd_wrapper(command: &str) -> bool {
    let unquoted = command.trim_matches('"');
    let lower = unquoted.to_ascii_lowercase();

    // Already a .exe or a shell — no wrapping needed.
    if lower.ends_with(".exe") || lower.ends_with(".com") {
        return false;
    }
    let basename = lower.rsplit(|ch: char| ch == '\\' || ch == '/').next().unwrap_or(&lower);
    if matches!(basename, "cmd" | "cmd.exe" | "powershell" | "powershell.exe" | "pwsh" | "pwsh.exe") {
        return false;
    }

    // If it's an absolute/relative path with an extension, check the extension.
    if let Some(ext) = std::path::Path::new(unquoted).extension() {
        let ext = ext.to_ascii_lowercase();
        return ext == "cmd" || ext == "bat";
    }

    // Bare name (e.g. "codex") — check if <name>.exe exists on PATH.
    // If it does, CreateProcess can find it directly; no wrapper needed.
    use std::env;
    if let Ok(path_var) = env::var("PATH") {
        let exe_name = format!("{}.exe", unquoted);
        for dir in env::split_paths(&path_var) {
            if dir.join(&exe_name).is_file() {
                return false;
            }
        }
    }

    // No .exe found on PATH — likely a .cmd/.bat shim, needs wrapping.
    true
}


pub fn split_windows_commandline(commandline: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;

    for ch in commandline.chars() {
        match ch {
            '"' => in_quotes = !in_quotes,
            ch if ch.is_whitespace() && !in_quotes => {
                if !current.is_empty() {
                    args.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }

    if !current.is_empty() {
        args.push(current);
    }

    args
}

fn required_parent<'a>(parent: Option<&'a str>, action_type: &str) -> Result<&'a str> {
    let parent = parent.context(format!(
        "field 'parent' is required for {} target panel",
        action_type
    ))?;
    ensure_non_empty("parent", parent)?;
    Ok(parent)
}

async fn send_input_to_new_pane(
    shell_mgr: &ShellManager,
    pane_id: &str,
    input: &str,
    event_tx: &mpsc::UnboundedSender<AppEvent>,
) -> Result<()> {
    ensure_non_empty("session_id", pane_id)?;
    ensure_non_empty("input", input)?;
    coordinator_log(&format!(
        "open_and_send send_input_begin pane_id={} wait_ms=700 input_chars={}",
        pane_id,
        input.chars().count(),
    ));
    let _ = event_tx.send(AppEvent::ExecutionInfo(format!(
        "Sending input to pane {}.",
        pane_id
    )));
    sleep(Duration::from_millis(700)).await;
    let result = shell_mgr
        .wt_send_input(pane_id, &format!("{input}\r"))
        .await
        .with_context(|| format!("failed to send input to pane {}", pane_id))?;
    coordinator_log(&format!(
        "open_and_send send_input_success pane_id={} response={}",
        pane_id,
        summarize_json_for_log(&result)
    ));
    let _ = event_tx.send(AppEvent::ExecutionInfo(format!(
        "Sent input to pane {}.",
        pane_id
    )));
    Ok(())
}

pub fn join_windows_commandline(args: &[&str]) -> String {
    args.iter()
        .map(|arg| quote_windows_commandline_arg(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

fn build_delegate_startup_prompt_commandline(commandline: &str, input: &str) -> Result<String> {
    let tokens = split_windows_commandline(commandline);
    if tokens.is_empty() {
        bail!("delegate agent runtime commandline is empty");
    }

    let mut args = Vec::with_capacity(tokens.len() + 2);
    args.extend(tokens.iter().map(String::as_str));

    // Look up the agent's prompt flag from the registry.
    let exe = tokens.first().map(|s| s.trim_matches('"')).unwrap_or("");
    let profile = agent_registry::lookup_profile(exe);
    if let PromptFlag::Flag(flag) = profile.delegate_prompt_flag {
        args.push(flag);
    }
    args.push(input);
    Ok(join_windows_commandline(&args))
}

/// Returns true if the command cannot be launched directly via CreateProcess
/// and should be typed into a shell tab instead (e.g. npm .cmd/.bat shims).
pub fn needs_shell_launch(commandline: &str) -> bool {
    let first_token = split_windows_commandline(commandline)
        .into_iter()
        .next()
        .unwrap_or_default();
    needs_cmd_wrapper(&first_token)
}

/// Returns true if the first token of `commandline` can actually be launched —
/// i.e. it resolves to a real program: a shell, an `.exe`/`.cmd`/`.bat`/`.com`
/// found on PATH, or an existing file. Used to detect a misconfigured /
/// nonexistent delegate agent *before* spawning the tab: without this, WT
/// creates the tab, the not-found command exits instantly, and WT closes the
/// pane before the user can see the error (the tab just flashes shut). Known
/// built-in agents are covered because `resolve_commandline_executable` first
/// resolves them to a concrete path.
pub fn delegate_command_launchable(commandline: &str) -> bool {
    let resolved = resolve_commandline_executable(commandline);
    let token = match split_windows_commandline(&resolved).into_iter().next() {
        Some(t) => t,
        None => return false,
    };
    let unquoted = token.trim_matches('"');
    if unquoted.is_empty() {
        return false;
    }
    let lower = unquoted.to_ascii_lowercase();
    let basename = lower
        .rsplit(|c: char| c == '\\' || c == '/')
        .next()
        .unwrap_or(&lower);
    // Shells are always launchable.
    if matches!(
        basename,
        "cmd" | "cmd.exe" | "powershell" | "powershell.exe" | "pwsh" | "pwsh.exe"
    ) {
        return true;
    }
    let path = std::path::Path::new(unquoted);
    // An explicit path (contains a separator): it must exist as a file — try the
    // common executable extensions when none was given.
    if unquoted.contains('\\') || unquoted.contains('/') {
        if path.is_file() {
            return true;
        }
        if path.extension().is_none() {
            for ext in ["exe", "cmd", "bat", "com"] {
                if path.with_extension(ext).is_file() {
                    return true;
                }
            }
        }
        return false;
    }
    // A bare name: search PATH. With an extension look for it verbatim; otherwise
    // try the common executable extensions.
    let Ok(path_var) = std::env::var("PATH") else {
        return false;
    };
    let has_ext = path.extension().is_some();
    for dir in std::env::split_paths(&path_var) {
        if has_ext {
            if dir.join(unquoted).is_file() {
                return true;
            }
        } else {
            for ext in ["exe", "cmd", "bat", "com"] {
                if dir.join(format!("{unquoted}.{ext}")).is_file() {
                    return true;
                }
            }
        }
    }
    false
}

// Quote arguments using the standard Windows CommandLineToArgvW escaping rules.
fn quote_windows_commandline_arg(arg: &str) -> String {
    if arg.is_empty() {
        return "\"\"".to_string();
    }

    let needs_quotes = arg.chars().any(|ch| ch.is_whitespace() || ch == '"');
    if !needs_quotes {
        return arg.to_string();
    }

    let mut quoted = String::with_capacity(arg.len() + 2);
    quoted.push('"');
    let mut backslashes = 0usize;
    for ch in arg.chars() {
        match ch {
            '\\' => backslashes += 1,
            '"' => {
                quoted.push_str(&"\\".repeat(backslashes * 2 + 1));
                quoted.push('"');
                backslashes = 0;
            }
            _ => {
                if backslashes > 0 {
                    quoted.push_str(&"\\".repeat(backslashes));
                    backslashes = 0;
                }
                quoted.push(ch);
            }
        }
    }

    if backslashes > 0 {
        quoted.push_str(&"\\".repeat(backslashes * 2));
    }
    quoted.push('"');
    quoted
}

fn ensure_non_empty(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("field '{}' must not be empty", field);
    }
    Ok(())
}

/// Resolve the profile for an agent-created terminal.
///
/// Prefers an explicit, non-empty profile supplied by the LLM; otherwise falls
/// back to the `profile` field of the active pane (the shell the user is
/// working in). An empty profile from either source is treated as "no profile"
/// so downstream callers let Windows Terminal pick the default — this keeps the
/// behavior consistent with `ShellManager::create_terminal_wt`.
pub(crate) fn resolve_agent_profile(
    explicit: Option<&str>,
    active_pane: Option<&serde_json::Value>,
) -> Option<String> {
    match explicit {
        Some(p) if !p.is_empty() => Some(p.to_owned()),
        _ => active_pane
            .and_then(|v| v.get("profile"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_owned()),
    }
}

/// A bare POSIX absolute path (`/home/user`, `/mnt/c/...`) — starts with a
/// single `/`. A `//`-prefixed path is a forward-slash UNC path (e.g.
/// `//wsl.localhost/Ubuntu/...`), which *is* a valid Windows path, so it's not
/// treated as POSIX here.
fn is_posix_absolute_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.first() == Some(&b'/') && bytes.get(1) != Some(&b'/')
}

/// Choose the working directory to hand a **Windows** agent CLI process.
///
/// A WSL pane reports a POSIX cwd (e.g. `/home/user`), which is not a valid
/// working directory for a Windows process — handing it to a Windows agent CLI
/// (Copilot/Claude/Gemini) makes it fail to launch. When the provided cwd is a
/// bare POSIX path we fall back to the Windows home (`%USERPROFILE%`, passed in
/// as `windows_home`) if available; otherwise drop it so Windows Terminal picks
/// a sensible default. Windows paths (drive-letter, UNC) and relative paths pass
/// through unchanged, and a missing/blank cwd stays `None`.
///
/// Running an agent *natively inside WSL* (so it honors the Linux cwd and uses
/// the WSL toolchain) is a separate future feature; this is only a guard so the
/// Windows agent CLI doesn't crash on an unusable cwd.
pub(crate) fn sanitize_windows_agent_cwd(
    cwd: Option<&str>,
    windows_home: Option<&str>,
) -> Option<String> {
    let cwd = cwd.map(str::trim).filter(|c| !c.is_empty())?;
    if is_posix_absolute_path(cwd) {
        windows_home
            .map(str::trim)
            .filter(|h| !h.is_empty())
            .map(str::to_owned)
    } else {
        Some(cwd.to_owned())
    }
}

fn resolve_created_pane_id(result: &serde_json::Value, action_name: &str) -> Result<String> {
    value_to_string(result.get("session_id"))
        .filter(|pane_id| !pane_id.trim().is_empty())
        .ok_or_else(|| {
            anyhow!(
                "{} response missing pane_id: {}",
                action_name,
                summarize_json_for_log(result)
            )
        })
}

fn value_to_string(value: Option<&serde_json::Value>) -> Option<String> {
    match value {
        Some(serde_json::Value::String(s)) => Some(s.clone()),
        Some(serde_json::Value::Number(n)) => Some(n.to_string()),
        _ => None,
    }
}

fn open_target_label(target: &OpenTarget) -> &'static str {
    match target {
        OpenTarget::Tab => "tab",
        OpenTarget::Panel => "panel",
    }
}

fn delegate_prompt_delivery_label(delivery: DelegatePromptDelivery) -> &'static str {
    match delivery {
        DelegatePromptDelivery::LaunchThenSend => "launch_then_send",
        DelegatePromptDelivery::LaunchWithStartupPrompt => "launch_with_startup_prompt",
    }
}

fn summarize_json_for_log(value: &serde_json::Value) -> String {
    let json = serde_json::to_string(value).unwrap_or_else(|_| "<unserializable json>".to_string());
    truncate_for_log(&json, 512)
}

fn truncate_for_log(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let truncated: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{}...", truncated)
    } else {
        truncated
    }
}

fn coordinator_log(msg: &str) {
    tracing::debug!(target: "coordinator", "{}", msg);
}

fn extract_json_code_block(text: &str) -> Option<&str> {
    let start = text.find("```json").or_else(|| text.find("```JSON"))?;
    let mut body = &text[start + 7..];
    if let Some(b) = body.strip_prefix('\r') {
        body = b;
    }
    if let Some(b) = body.strip_prefix('\n') {
        body = b;
    }
    extract_balanced_json_object(body)
}

fn extract_first_json_object(text: &str) -> Option<&str> {
    extract_balanced_json_object(text)
}

/// Returns the substring spanning the first balanced JSON object in `text`.
///
/// Walks the input as bytes, tracking string state and brace depth so that
/// braces or fence markers (```) inside JSON string values do not terminate
/// the scan early. Byte indexing is safe because we only land on ASCII
/// characters (`{`, `}`, `"`, `\`).
fn extract_balanced_json_object(text: &str) -> Option<&str> {
    let bytes = text.as_bytes();
    let start = bytes.iter().position(|&b| b == b'{')?;

    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut escape = false;
    for j in start..bytes.len() {
        let c = bytes[j];
        if in_string {
            if escape {
                escape = false;
            } else if c == b'\\' {
                escape = true;
            } else if c == b'"' {
                in_string = false;
            }
        } else {
            match c {
                b'"' => in_string = true,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(text[start..=j].trim());
                    }
                }
                _ => {}
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{
        build_delegate_launch_commandline,
        build_delegate_launch_commandline_with_session, default_delegate_agent_runtimes,
        parse_autofix_response, parse_recommendation_set, resolve_agent_profile,
        resolve_created_pane_id, sanitize_windows_agent_cwd,
        validate_recommendation_set_for_coordinator_target, AutofixDecision, DelegateAgentRuntime,
        DelegatePromptDelivery, OpenTarget, RecommendedAction,
    };
    use serde_json::json;

    #[test]
    fn default_delegate_runtime_uses_cli_default_model() {
        let runtime = default_delegate_agent_runtimes(None, None, None)
            .into_iter()
            .find(|runtime| runtime.id == "copilot")
            .expect("copilot runtime should exist");

        assert_eq!(runtime.commandline, "copilot");
        assert_eq!(
            runtime.prompt_delivery,
            DelegatePromptDelivery::LaunchWithStartupPrompt
        );
    }

    #[test]
    fn delegate_launch_commandline_omits_model_when_not_configured() {
        let runtime = default_delegate_agent_runtimes(None, None, None)
            .into_iter()
            .find(|runtime| runtime.id == "copilot")
            .expect("copilot runtime should exist");

        let commandline =
            build_delegate_launch_commandline(&runtime, Some("Fix the build and report back"), None)
                .unwrap();

        assert!(!commandline.contains("--model"));
        // May be wrapped as "cmd /c copilot ..." if copilot.exe isn't on PATH.
        assert!(commandline.contains("copilot"));
        assert!(commandline.contains("-i \"Fix the build and report back\""));
    }

    #[test]
    fn delegate_runtime_inherits_model_from_agent_command() {
        let runtime = default_delegate_agent_runtimes(
            None,
            Some("copilot --acp --stdio --model claude-haiku-4.5"),
            None,
        )
        .into_iter()
        .find(|runtime| runtime.id == "copilot")
        .expect("copilot runtime should exist");

        assert_eq!(runtime.commandline, "copilot --model claude-haiku-4.5");
    }

    #[test]
    fn delegate_runtime_preserves_explicit_copilot_exe_path() {
        let runtime = default_delegate_agent_runtimes(None, Some(
            "\"C:\\Users\\kaitao\\AppData\\Local\\Microsoft\\WinGet\\Links\\copilot.exe\" --acp --stdio --model=claude-haiku-4.5",
        ), None)
        .into_iter()
        .find(|runtime| runtime.id == "copilot")
        .expect("copilot runtime should exist");

        assert_eq!(
            runtime.commandline,
            "C:\\Users\\kaitao\\AppData\\Local\\Microsoft\\WinGet\\Links\\copilot.exe --model claude-haiku-4.5"
        );
    }

    #[test]
    fn delegate_runtime_prefers_explicit_delegate_command() {
        let runtime = default_delegate_agent_runtimes(
            Some("copilot --model claude-haiku-4.5"),
            Some("copilot --acp --stdio --model gpt-5.2"),
            None,
        )
        .into_iter()
        .find(|runtime| runtime.id == "copilot")
        .expect("copilot runtime should exist");

        assert_eq!(runtime.commandline, "copilot --model claude-haiku-4.5");
    }

    #[test]
    fn delegate_launch_commandline_appends_startup_prompt_and_model() {
        let runtime = default_delegate_agent_runtimes(
            Some("copilot --model claude-haiku-4.5"),
            Some("copilot --acp --stdio --model gpt-5.2"),
            None,
        )
        .into_iter()
        .find(|runtime| runtime.id == "copilot")
        .expect("copilot runtime should exist");

        let commandline = build_delegate_launch_commandline(
            &runtime,
            Some("Fix the Rust build error and run cargo build"),
            None,
        )
        .unwrap();

        // May be wrapped as "cmd /c copilot ..." if copilot.exe isn't on PATH.
        assert!(commandline.contains("copilot"));
        assert!(commandline.contains("--model claude-haiku-4.5"));
        assert!(commandline.contains("-i \"Fix the Rust build error and run cargo build\""));
    }

    #[test]
    fn delegate_interactive_commandline_omits_startup_prompt() {
        let runtime = default_delegate_agent_runtimes(
            Some("copilot --model claude-haiku-4.5"),
            Some("copilot --acp --stdio --model gpt-5.2"),
            None,
        )
        .into_iter()
        .find(|runtime| runtime.id == "copilot")
        .expect("copilot runtime should exist");

        let commandline =
            build_delegate_launch_commandline_with_session(&runtime, None, None).unwrap();

        // May be wrapped as "cmd /c copilot ..." if copilot.exe isn't on PATH.
        assert!(commandline.contains("copilot"));
        // Model is still applied for the interactive launch.
        assert!(commandline.contains("--model claude-haiku-4.5"));
        // No startup-prompt flag is appended when there's no prompt.
        assert!(!commandline.contains("-i "));
    }

    #[test]
    fn delegate_command_launchable_detects_missing_agent() {
        // A shell is always launchable; a real system exe resolves on PATH; a
        // bogus bare name does not — this is the up-front check that lets the
        // delegate open a persistent error tab instead of a tab that flashes shut.
        assert!(super::delegate_command_launchable("cmd"));
        assert!(super::delegate_command_launchable("cmd.exe /c echo hi"));
        assert!(
            !super::delegate_command_launchable("wt-nonexistent-delegate-xyz"),
            "a nonexistent bare command must be reported as not launchable"
        );
        assert!(
            !super::delegate_command_launchable("wt-nonexistent-delegate-xyz -i \"hi\""),
            "extra args must not make a missing agent look launchable"
        );
    }

    #[test]
    fn delegate_commandline_appends_pinned_session_id_for_copilot() {
        let runtime = DelegateAgentRuntime {
            id: "copilot".to_string(),
            name: "Copilot".to_string(),
            description: "Launches copilot as a delegate agent.".to_string(),
            commandline: "copilot".to_string(),
            prompt_delivery: DelegatePromptDelivery::LaunchWithStartupPrompt,
            model: None,
        };

        let commandline = build_delegate_launch_commandline_with_session(
            &runtime,
            Some("hi"),
            Some("11111111-2222-3333-4444-555555555555"),
        )
        .unwrap();

        assert!(commandline.contains("--session-id 11111111-2222-3333-4444-555555555555"));
    }

    #[test]
    fn pinned_session_id_follows_agent_not_cmd_wrapper() {
        // copilot may or may not be `cmd /c`-wrapped depending on whether
        // copilot.exe vs copilot.cmd is on PATH. Either way the pinned flag must
        // be present and positioned *after* the agent name — never lost to a
        // first-token lookup that sees `cmd` instead of the agent.
        let runtime = DelegateAgentRuntime {
            id: "copilot".to_string(),
            name: "Copilot".to_string(),
            description: "Launches copilot as a delegate agent.".to_string(),
            commandline: "copilot".to_string(),
            prompt_delivery: DelegatePromptDelivery::LaunchWithStartupPrompt,
            model: None,
        };

        let commandline = build_delegate_launch_commandline_with_session(
            &runtime,
            Some("hi"),
            Some("11111111-2222-3333-4444-555555555555"),
        )
        .unwrap();

        assert!(
            commandline.contains("--session-id 11111111-2222-3333-4444-555555555555"),
            "pinned flag missing: {commandline}"
        );
        let agent_pos = commandline.find("copilot").expect("agent name present");
        let flag_pos = commandline.find("--session-id").unwrap();
        assert!(
            flag_pos > agent_pos,
            "--session-id must follow the agent, not attach to a cmd wrapper: {commandline}"
        );
    }

    #[test]
    fn pinned_session_id_appended_for_adapter_launch_command() {
        // Regression for the agent-identification bug behind PR review: an
        // adapter-style launch ("npx -y @agentclientprotocol/claude-agent-acp" ->
        // claude) must still be recognized as a pinnable agent. The old
        // `split_whitespace().next()` + lookup_profile saw "npx" ->
        // DEFAULT_PROFILE -> no --session-id; `resolve_agent_id_from_cmd`
        // resolves the adapter command to claude, which advertises the flag.
        let runtime = DelegateAgentRuntime {
            id: "claude".to_string(),
            name: "Claude".to_string(),
            description: "Launches claude as a delegate agent.".to_string(),
            commandline: "npx -y @agentclientprotocol/claude-agent-acp".to_string(),
            prompt_delivery: DelegatePromptDelivery::LaunchWithStartupPrompt,
            model: None,
        };

        let commandline = build_delegate_launch_commandline_with_session(
            &runtime,
            Some("hi"),
            Some("11111111-2222-3333-4444-555555555555"),
        )
        .unwrap();

        assert!(
            commandline.contains("--session-id 11111111-2222-3333-4444-555555555555"),
            "adapter launch must be identified as a pinnable agent: {commandline}"
        );
    }

    #[test]
    fn delegate_commandline_omits_session_id_when_unset() {
        let runtime = DelegateAgentRuntime {
            id: "copilot".to_string(),
            name: "Copilot".to_string(),
            description: "Launches copilot as a delegate agent.".to_string(),
            commandline: "copilot".to_string(),
            prompt_delivery: DelegatePromptDelivery::LaunchWithStartupPrompt,
            model: None,
        };

        let commandline =
            build_delegate_launch_commandline_with_session(&runtime, Some("hi"), None).unwrap();

        assert!(!commandline.contains("--session-id"));
    }

    #[test]
    fn delegate_launch_commandline_preserves_explicit_exe_path_with_startup_prompt() {
        let runtime = default_delegate_agent_runtimes(
            Some(
                "\"C:\\Users\\kaitao\\AppData\\Local\\Microsoft\\WinGet\\Links\\copilot.exe\" --model claude-haiku-4.5",
            ),
            None,
            None,
        )
        .into_iter()
        .find(|runtime| runtime.id == "copilot")
        .expect("copilot runtime should exist");

        let commandline =
            build_delegate_launch_commandline(&runtime, Some("Inspect the repo and summarize"), None)
                .unwrap();

        assert_eq!(
            commandline,
            "C:\\Users\\kaitao\\AppData\\Local\\Microsoft\\WinGet\\Links\\copilot.exe --model claude-haiku-4.5 -i \"Inspect the repo and summarize\""
        );
    }

    #[test]
    fn parse_recommendations_accepts_open_and_send_tab_actions_without_parent() {
        let text = r#"```json
{
  "recommended_choice": 1,
  "choices": [
    {
      "choice": 1,
      "title": "Open a shell tab",
      "actions": [
        {
          "type": "open_and_send",
          "target": "tab",
          "input": "pwd",
          "cwd": "C:\\repo",
          "title": "Repo shell"
        }
      ]
    },
    {
      "choice": 2,
      "title": "Delegate in a new tab",
      "actions": [
        {
          "type": "open_and_send",
          "target": "tab",
          "input": "Inspect the repo",
          "agent": "copilot",
          "cwd": "C:\\repo",
          "title": "Copilot delegate"
        }
      ]
    },
    {
      "choice": 3,
      "title": "Run locally",
      "actions": [
        {
          "type": "send",
          "parent": "1",
          "input": "pwd"
        }
      ]
    }
  ]
}
```"#;

        let parsed = parse_recommendation_set(text).expect("recommendation set should parse");

        assert!(matches!(
            parsed.choices[0].actions[0],
            RecommendedAction::OpenAndSend {
                target: OpenTarget::Tab,
                ..
            }
        ));
        assert!(matches!(
            parsed.choices[1].actions[0],
            RecommendedAction::OpenAndSend {
                target: OpenTarget::Tab,
                ..
            }
        ));
    }

    #[test]
    fn parses_open_action_without_input() {
        let text = r#"```json
{
  "recommended_choice": 1,
  "choices": [
    {
      "choice": 1,
      "title": "Open an empty tab",
      "actions": [
        {
          "type": "open",
          "target": "tab",
          "cwd": "C:\\repo"
        }
      ]
    },
    {
      "choice": 2,
      "title": "Split a panel here",
      "actions": [
        {
          "type": "open",
          "target": "panel",
          "parent": "12"
        }
      ]
    }
  ]
}
```"#;

        let parsed = parse_recommendation_set(text).expect("open recommendation should parse");
        assert!(matches!(
            parsed.choices[0].actions[0],
            RecommendedAction::Open {
                target: OpenTarget::Tab,
                ..
            }
        ));
        assert!(matches!(
            parsed.choices[1].actions[0],
            RecommendedAction::Open {
                target: OpenTarget::Panel,
                ..
            }
        ));
    }

    #[test]
    fn parses_open_panel_with_direction() {
        let text = r#"```json
{
  "recommended_choice": 1,
  "choices": [
    {
      "choice": 1,
      "title": "Split right",
      "actions": [
        {
          "type": "open",
          "target": "panel",
          "parent": "12",
          "direction": "right"
        }
      ]
    }
  ]
}
```"#;

        let parsed = parse_recommendation_set(text).expect("open with direction should parse");
        match &parsed.choices[0].actions[0] {
            RecommendedAction::Open { direction, .. } => {
                assert_eq!(direction.as_deref(), Some("right"));
            }
            other => panic!("expected Open, got {other:?}"),
        }
    }

    // ── profile inheritance (PR #366) ──────────────────────────────────────

    #[test]
    fn open_action_defaults_profile_to_none_when_absent() {
        // The `profile` field is optional; an LLM emitting the pre-#366 schema
        // (no `profile` key) must still parse, with profile == None.
        let text = r#"```json
{
  "recommended_choice": 1,
  "choices": [
    {
      "choice": 1,
      "title": "Open a tab",
      "actions": [ { "type": "open", "target": "tab" } ]
    }
  ]
}
```"#;
        let parsed = parse_recommendation_set(text).expect("open without profile should parse");
        match &parsed.choices[0].actions[0] {
            RecommendedAction::Open { profile, .. } => assert_eq!(profile.as_deref(), None),
            other => panic!("expected Open, got {other:?}"),
        }
    }

    #[test]
    fn open_action_parses_explicit_profile() {
        let text = r#"```json
{
  "recommended_choice": 1,
  "choices": [
    {
      "choice": 1,
      "title": "Open Ubuntu tab",
      "actions": [ { "type": "open", "target": "tab", "profile": "Ubuntu" } ]
    }
  ]
}
```"#;
        let parsed = parse_recommendation_set(text).expect("open with profile should parse");
        match &parsed.choices[0].actions[0] {
            RecommendedAction::Open { profile, .. } => {
                assert_eq!(profile.as_deref(), Some("Ubuntu"));
            }
            other => panic!("expected Open, got {other:?}"),
        }
    }

    #[test]
    fn open_and_send_parses_explicit_profile() {
        let text = r#"```json
{
  "recommended_choice": 1,
  "choices": [
    {
      "choice": 1,
      "title": "Open Ubuntu tab and run",
      "actions": [
        {
          "type": "open_and_send",
          "target": "tab",
          "input": "ls -la",
          "profile": "Ubuntu"
        }
      ]
    }
  ]
}
```"#;
        let parsed = parse_recommendation_set(text).expect("open_and_send should parse");
        match &parsed.choices[0].actions[0] {
            RecommendedAction::OpenAndSend { profile, input, .. } => {
                assert_eq!(profile.as_deref(), Some("Ubuntu"));
                assert_eq!(input, "ls -la");
            }
            other => panic!("expected OpenAndSend, got {other:?}"),
        }
    }

    #[test]
    fn resolve_profile_prefers_explicit_over_active_pane() {
        let active = json!({ "profile": "PowerShell" });
        assert_eq!(
            resolve_agent_profile(Some("Ubuntu"), Some(&active)),
            Some("Ubuntu".to_string())
        );
    }

    #[test]
    fn resolve_profile_falls_back_to_active_pane_when_explicit_missing() {
        let active = json!({ "profile": "Ubuntu" });
        assert_eq!(
            resolve_agent_profile(None, Some(&active)),
            Some("Ubuntu".to_string())
        );
    }

    #[test]
    fn resolve_profile_falls_back_when_explicit_empty() {
        // An empty explicit profile is treated as "unspecified" and must not
        // shadow the active pane's profile.
        let active = json!({ "profile": "Ubuntu" });
        assert_eq!(
            resolve_agent_profile(Some(""), Some(&active)),
            Some("Ubuntu".to_string())
        );
    }

    #[test]
    fn resolve_profile_none_when_active_pane_profile_empty() {
        // An empty active-pane profile must resolve to None (not Some("")), so
        // downstream lets Windows Terminal pick the default. This mirrors
        // ShellManager::create_terminal_wt's non-empty guard.
        let active = json!({ "profile": "" });
        assert_eq!(resolve_agent_profile(None, Some(&active)), None);
    }

    #[test]
    fn resolve_profile_none_when_active_pane_has_no_profile_field() {
        let active = json!({ "session_id": "abc" });
        assert_eq!(resolve_agent_profile(None, Some(&active)), None);
    }

    #[test]
    fn resolve_profile_none_when_active_pane_unavailable() {
        // wt_get_active_pane() failing (COM error) yields None; with no explicit
        // profile the result is None (default profile downstream).
        assert_eq!(resolve_agent_profile(None, None), None);
    }

    #[test]
    fn resolve_profile_uses_explicit_when_active_pane_unavailable() {
        assert_eq!(
            resolve_agent_profile(Some("Ubuntu"), None),
            Some("Ubuntu".to_string())
        );
    }

    #[test]
    fn resolve_profile_ignores_non_string_active_pane_profile() {
        let active = json!({ "profile": 42 });
        assert_eq!(resolve_agent_profile(None, Some(&active)), None);
    }

    // ── Windows agent CLI cwd sanitize (WSL POSIX cwd guard) ───────────────

    #[test]
    fn sanitize_cwd_passes_through_windows_drive_path() {
        assert_eq!(
            sanitize_windows_agent_cwd(Some(r"C:\repo"), Some(r"C:\Users\me")),
            Some(r"C:\repo".to_string())
        );
    }

    #[test]
    fn sanitize_cwd_falls_back_for_posix_path() {
        // A WSL pane's "/home/user" is unusable for a Windows agent CLI → home.
        assert_eq!(
            sanitize_windows_agent_cwd(Some("/home/user"), Some(r"C:\Users\me")),
            Some(r"C:\Users\me".to_string())
        );
    }

    #[test]
    fn sanitize_cwd_falls_back_for_mnt_path() {
        assert_eq!(
            sanitize_windows_agent_cwd(Some("/mnt/c/repo"), Some(r"C:\Users\me")),
            Some(r"C:\Users\me".to_string())
        );
    }

    #[test]
    fn sanitize_cwd_root_posix_falls_back() {
        assert_eq!(
            sanitize_windows_agent_cwd(Some("/"), Some(r"C:\Users\me")),
            Some(r"C:\Users\me".to_string())
        );
    }

    #[test]
    fn sanitize_cwd_posix_without_home_is_dropped() {
        // No Windows home available → drop the unusable cwd (None), letting WT
        // pick a default rather than crashing the agent CLI on a POSIX path.
        assert_eq!(sanitize_windows_agent_cwd(Some("/home/user"), None), None);
        assert_eq!(sanitize_windows_agent_cwd(Some("/home/user"), Some("   ")), None);
    }

    #[test]
    fn sanitize_cwd_keeps_unc_path() {
        // Forward-slash and backslash UNC paths are valid Windows paths — keep.
        assert_eq!(
            sanitize_windows_agent_cwd(Some("//wsl.localhost/Ubuntu/home/user"), Some(r"C:\Users\me")),
            Some("//wsl.localhost/Ubuntu/home/user".to_string())
        );
        assert_eq!(
            sanitize_windows_agent_cwd(Some(r"\\wsl.localhost\Ubuntu\home\user"), Some(r"C:\Users\me")),
            Some(r"\\wsl.localhost\Ubuntu\home\user".to_string())
        );
    }

    #[test]
    fn sanitize_cwd_none_and_blank_stay_none() {
        assert_eq!(sanitize_windows_agent_cwd(None, Some(r"C:\Users\me")), None);
        assert_eq!(sanitize_windows_agent_cwd(Some("   "), Some(r"C:\Users\me")), None);
    }

    #[test]
    fn sanitize_cwd_keeps_relative_path() {
        assert_eq!(
            sanitize_windows_agent_cwd(Some("subdir"), Some(r"C:\Users\me")),
            Some("subdir".to_string())
        );
    }

    #[test]
    fn rejects_open_with_invalid_direction() {
        let text = r#"```json
{
  "recommended_choice": 1,
  "choices": [
    {
      "choice": 1,
      "title": "Split sideways",
      "actions": [
        {
          "type": "open",
          "target": "panel",
          "parent": "12",
          "direction": "sideways"
        }
      ]
    }
  ]
}
```"#;

        assert!(parse_recommendation_set(text).is_err());
    }

    #[test]
    fn rejects_open_tab_with_direction() {
        let text = r#"```json
{
  "recommended_choice": 1,
  "choices": [
    {
      "choice": 1,
      "title": "Open tab right?",
      "actions": [
        {
          "type": "open",
          "target": "tab",
          "direction": "right"
        }
      ]
    }
  ]
}
```"#;

        assert!(parse_recommendation_set(text).is_err());
    }

    #[test]
    fn rejects_open_panel_without_parent() {
        let text = r#"```json
{
  "recommended_choice": 1,
  "choices": [
    {
      "choice": 1,
      "title": "Open a panel",
      "actions": [
        {
          "type": "open",
          "target": "panel"
        }
      ]
    }
  ]
}
```"#;

        assert!(parse_recommendation_set(text).is_err());
    }

    #[test]
    fn rejects_send_to_current_coordinator_target() {
        let text = r#"```json
{
  "recommended_choice": 1,
  "choices": [
    {
      "choice": 1,
      "title": "Reply in the current pane",
      "actions": [
        {
          "type": "send",
          "parent": "14",
          "input": "Continue in this pane"
        }
      ]
    },
    {
      "choice": 2,
      "title": "Run locally",
      "actions": [
        {
          "type": "send",
          "parent": "1",
          "input": "pwd"
        }
      ]
    },
    {
      "choice": 3,
      "title": "Delegate",
      "actions": [
        {
          "type": "open_and_send",
          "target": "tab",
          "input": "Inspect the repo",
          "agent": "copilot",
          "cwd": "C:\\repo"
        }
      ]
    }
  ]
}
```"#;

        let parsed = parse_recommendation_set(text).expect("recommendation set should parse");
        let filtered = validate_recommendation_set_for_coordinator_target(&parsed, Some("14"))
            .expect("should filter instead of rejecting");

        // Choice 1 (self-targeted) should be removed, choices 2 and 3 remain.
        assert_eq!(filtered.choices.len(), 2);
        assert_eq!(filtered.choices[0].choice, 2);
        assert_eq!(filtered.choices[1].choice, 3);
        // recommended_choice was 1 (now filtered out), so it should be None.
        assert_eq!(filtered.recommended_choice, None);
    }

    #[test]
    fn rejects_open_and_send_panel_without_parent() {
        let text = r#"```json
{
  "recommended_choice": 1,
  "choices": [
    {
      "choice": 1,
      "title": "Split a panel",
      "actions": [
        {
          "type": "open_and_send",
          "target": "panel",
          "input": "pwd"
        }
      ]
    },
    {
      "choice": 2,
      "title": "Run locally",
      "actions": [
        {
          "type": "send",
          "parent": "1",
          "input": "pwd"
        }
      ]
    },
    {
      "choice": 3,
      "title": "Open a tab",
      "actions": [
        {
          "type": "open_and_send",
          "target": "tab",
          "input": "pwd"
        }
      ]
    }
  ]
}
```"#;

        let err =
            parse_recommendation_set(text).expect_err("panel without parent should be rejected");
        assert!(format!("{err:#}")
            .contains("field 'parent' is required for open_and_send target panel"));
    }

    #[test]
    fn parse_recommendations_accepts_single_choice() {
        let text = r#"```json
{
  "recommended_choice": 1,
  "choices": [
    {
      "choice": 1,
      "title": "Run locally",
      "actions": [
        {
          "type": "send",
          "parent": "1",
          "input": "pwd"
        }
      ]
    }
  ]
}
```"#;

        let parsed =
            parse_recommendation_set(text).expect("single-choice recommendation should parse");
        assert_eq!(parsed.choices.len(), 1);
        assert_eq!(parsed.choices[0].choice, 1);
    }

    #[test]
    fn parse_recommendations_handles_backticks_inside_string_values() {
        // Regression: a JSON string value that contains a triple-backtick fence
        // marker (e.g. an `input` prompt asking another agent to emit a
        // ```mermaid block) used to terminate the ```json fence early, leaving
        // the JSON truncated and unparseable.
        let text = r#"Sure, here's the plan.

```json
{
  "recommended_choice": 1,
  "choices": [
    {
      "choice": 1,
      "title": "Delegate to Copilot",
      "actions": [
        {
          "type": "open_and_send",
          "target": "tab",
          "agent": "copilot",
          "cwd": "C:\\repo",
          "input": "Produce a Mermaid flowchart (```mermaid) showing the main flow.",
          "title": "Explore project"
        }
      ]
    }
  ]
}
```"#;

        let parsed = parse_recommendation_set(text)
            .expect("recommendation with backticks in string should parse");
        assert_eq!(parsed.choices.len(), 1);
        match &parsed.choices[0].actions[0] {
            RecommendedAction::OpenAndSend { input, .. } => {
                assert!(input.contains("```mermaid"));
            }
            other => panic!("expected OpenAndSend, got {other:?}"),
        }
    }

    #[test]
    fn parse_recommendations_rejects_four_choices() {
        let text = r#"```json
{
  "recommended_choice": 1,
  "choices": [
    {
      "choice": 1,
      "title": "One",
      "actions": [
        {
          "type": "send",
          "parent": "1",
          "input": "pwd"
        }
      ]
    },
    {
      "choice": 2,
      "title": "Two",
      "actions": [
        {
          "type": "send",
          "parent": "1",
          "input": "pwd"
        }
      ]
    },
    {
      "choice": 3,
      "title": "Three",
      "actions": [
        {
          "type": "send",
          "parent": "1",
          "input": "pwd"
        }
      ]
    },
    {
      "choice": 4,
      "title": "Four",
      "actions": [
        {
          "type": "send",
          "parent": "1",
          "input": "pwd"
        }
      ]
    }
  ]
}
```"#;

        let err =
            parse_recommendation_set(text).expect_err("four-choice recommendation should fail");
        assert!(format!("{err:#}").contains("expected 1 to 3 choices"));
    }

    #[test]
    fn resolve_created_pane_id_accepts_numeric_ids() {
        let result = json!({ "session_id": 42 });

        let pane_id = resolve_created_pane_id(&result, "create_tab").unwrap();

        assert_eq!(pane_id, "42");
    }

    #[test]
    fn resolve_created_pane_id_rejects_missing_pane_id() {
        let result = json!({ "tab_id": "7" });

        let err = resolve_created_pane_id(&result, "create_tab")
            .expect_err("missing pane_id should fail");

        assert!(format!("{err:#}").contains("create_tab response missing pane_id"));
    }

    #[test]
    fn parse_autofix_explain_with_title_and_explanation() {
        let text = r#"```json
{"action": "explain", "title": "claude is not installed",
 "explanation": "The `claude` command isn't on PATH.\n\nInstall with `npm install -g @anthropic-ai/claude-code`."}
```"#;
        match parse_autofix_response(text) {
            AutofixDecision::Explain { title, explanation } => {
                assert_eq!(title, "claude is not installed");
                assert!(explanation.contains("npm install"));
            }
            other => panic!("expected Explain, got {other:?}"),
        }
    }

    #[test]
    fn parse_autofix_explain_falls_back_to_ignore_when_explanation_empty() {
        let text = r#"```json
{"action": "explain", "title": "Something", "explanation": "   "}
```"#;
        assert!(matches!(
            parse_autofix_response(text),
            AutofixDecision::Ignore
        ));
    }

    #[test]
    fn parse_autofix_explain_uses_default_title_when_missing() {
        let text = r#"```json
{"action": "explain", "explanation": "Some useful suggestion goes here."}
```"#;
        match parse_autofix_response(text) {
            AutofixDecision::Explain { title, .. } => assert_eq!(title, "Suggestion"),
            other => panic!("expected Explain with default title, got {other:?}"),
        }
    }

    #[test]
    fn parse_autofix_legacy_ignore_still_supported() {
        let text = r#"```json
{"action": "ignore"}
```"#;
        assert!(matches!(
            parse_autofix_response(text),
            AutofixDecision::Ignore
        ));
    }
}
