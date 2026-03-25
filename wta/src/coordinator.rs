use std::collections::BTreeSet;
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};

use crate::app::AppEvent;
use crate::shell::ShellManager;

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
    pub command: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecommendationSet {
    #[serde(default)]
    pub recommended_choice: Option<usize>,
    pub choices: Vec<RecommendationChoice>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecommendationChoice {
    pub choice: usize,
    pub title: String,
    #[serde(default)]
    pub rationale: String,
    pub actions: Vec<RecommendedAction>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RecommendedAction {
    RunCommand {
        parent: String,
        command: String,
    },
    SendPrompt {
        parent: String,
        prompt: String,
    },
    CreateShellTab {
        parent: String,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default)]
        commandline: Option<String>,
    },
    CreateShellPanel {
        parent: String,
        #[serde(default)]
        direction: Option<String>,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default)]
        commandline: Option<String>,
    },
    DelegateTab {
        parent: String,
        agent: String,
        prompt: String,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default)]
        title: Option<String>,
    },
}

pub fn default_supported_delegate_agents() -> Vec<SupportedDelegateAgent> {
    vec![SupportedDelegateAgent {
        id: "copilot".to_string(),
        name: "GitHub Copilot".to_string(),
        description:
            "Launches `copilot` in a new terminal target, optionally sets cwd, then sends a self-contained task prompt."
                .to_string(),
    }]
}

pub fn default_delegate_agent_runtimes() -> Vec<DelegateAgentRuntime> {
    vec![DelegateAgentRuntime {
        id: "copilot".to_string(),
        name: "GitHub Copilot".to_string(),
        description:
            "Launches `copilot` directly in a new terminal target and receives the task through typed prompt injection."
                .to_string(),
        command: "copilot".to_string(),
    }]
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
    mut rx: mpsc::UnboundedReceiver<RecommendationChoice>,
    event_tx: mpsc::UnboundedSender<AppEvent>,
    shell_mgr: Arc<ShellManager>,
    delegate_agents: Vec<DelegateAgentRuntime>,
) {
    while let Some(choice) = rx.recv().await {
        let _ = event_tx.send(AppEvent::SystemMessage(format!(
            "Executing choice {}: {}",
            choice.choice, choice.title
        )));

        match execute_choice(&choice, &shell_mgr, &delegate_agents, &event_tx).await {
            Ok(()) => {
                let _ = event_tx.send(AppEvent::SystemMessage(format!(
                    "Choice {} completed.",
                    choice.choice
                )));
            }
            Err(err) => {
                let _ = event_tx.send(AppEvent::SystemMessage(format!(
                    "Choice {} failed: {:#}",
                    choice.choice, err
                )));
            }
        }
    }
}

async fn execute_choice(
    choice: &RecommendationChoice,
    shell_mgr: &ShellManager,
    delegate_agents: &[DelegateAgentRuntime],
    event_tx: &mpsc::UnboundedSender<AppEvent>,
) -> Result<()> {
    for action in &choice.actions {
        match action {
            RecommendedAction::RunCommand { parent, command } => {
                ensure_non_empty("parent", parent)?;
                ensure_non_empty("command", command)?;
                let payload = format!("{command}\r");
                shell_mgr
                    .wt_send_input(parent, &payload)
                    .await
                    .with_context(|| format!("failed to send command to pane {}", parent))?;
                let _ = event_tx.send(AppEvent::SystemMessage(format!(
                    "Sent command to pane {}.",
                    parent
                )));
            }
            RecommendedAction::SendPrompt { parent, prompt } => {
                ensure_non_empty("parent", parent)?;
                ensure_non_empty("prompt", prompt)?;
                let payload = format!("{prompt}\r");
                shell_mgr
                    .wt_send_input(parent, &payload)
                    .await
                    .with_context(|| format!("failed to send prompt to pane {}", parent))?;
                let _ = event_tx.send(AppEvent::SystemMessage(format!(
                    "Sent prompt to pane {}.",
                    parent
                )));
            }
            RecommendedAction::CreateShellTab {
                parent: _,
                title,
                cwd,
                commandline,
            } => {
                let result = shell_mgr
                    .wt_create_tab(commandline.as_deref(), cwd.as_deref(), title.as_deref())
                    .await
                    .context("failed to create shell tab")?;
                let pane_id =
                    value_to_string(result.get("pane_id")).unwrap_or_else(|| "?".to_string());
                let tab_id =
                    value_to_string(result.get("tab_id")).unwrap_or_else(|| "?".to_string());
                let _ = event_tx.send(AppEvent::SystemMessage(format!(
                    "Created shell tab {} (pane {}).",
                    tab_id, pane_id
                )));
            }
            RecommendedAction::CreateShellPanel {
                parent,
                direction,
                title: _,
                cwd: _,
                commandline,
            } => {
                ensure_non_empty("parent", parent)?;
                let result = shell_mgr
                    .wt_split_pane(
                        parent,
                        commandline.as_deref(),
                        normalize_direction(direction.as_deref())?,
                        None,
                    )
                    .await
                    .with_context(|| format!("failed to split pane {}", parent))?;
                let pane_id =
                    value_to_string(result.get("pane_id")).unwrap_or_else(|| "?".to_string());
                let _ = event_tx.send(AppEvent::SystemMessage(format!(
                    "Created shell pane {} from {}.",
                    pane_id, parent
                )));
            }
            RecommendedAction::DelegateTab {
                parent: _,
                agent,
                prompt,
                cwd,
                title,
            } => {
                let runtime = lookup_delegate_agent(delegate_agents, agent)?;
                let result = shell_mgr
                    .wt_create_tab(
                        Some(&runtime.command),
                        cwd.as_deref(),
                        title.as_deref().or(Some(runtime.name.as_str())),
                    )
                    .await
                    .with_context(|| {
                        format!("failed to create delegate tab for {}", runtime.name)
                    })?;
                let pane_id =
                    value_to_string(result.get("pane_id")).unwrap_or_else(|| "?".to_string());
                let tab_id =
                    value_to_string(result.get("tab_id")).unwrap_or_else(|| "?".to_string());
                send_delegate_prompt(shell_mgr, &pane_id, prompt).await?;
                let _ = event_tx.send(AppEvent::SystemMessage(format!(
                    "Created delegate tab {} (pane {}) for {} and sent the task prompt.",
                    tab_id, pane_id, runtime.name
                )));
            }
        }
    }

    Ok(())
}

fn validate_recommendation_set(set: &RecommendationSet) -> Result<()> {
    if set.choices.len() != 3 {
        bail!("expected exactly 3 choices, got {}", set.choices.len());
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
        RecommendedAction::RunCommand { parent, command } => {
            ensure_non_empty("parent", parent)?;
            ensure_non_empty("command", command)?;
        }
        RecommendedAction::SendPrompt { parent, prompt } => {
            ensure_non_empty("parent", parent)?;
            ensure_non_empty("prompt", prompt)?;
        }
        RecommendedAction::CreateShellTab { parent, .. } => {
            ensure_non_empty("parent", parent)?;
        }
        RecommendedAction::CreateShellPanel {
            parent, direction, ..
        } => {
            ensure_non_empty("parent", parent)?;
            normalize_direction(direction.as_deref())?;
        }
        RecommendedAction::DelegateTab {
            parent,
            agent,
            prompt,
            ..
        } => {
            ensure_non_empty("parent", parent)?;
            ensure_non_empty("agent", agent)?;
            ensure_non_empty("prompt", prompt)?;
        }
    }

    Ok(())
}

fn lookup_delegate_agent<'a>(
    delegate_agents: &'a [DelegateAgentRuntime],
    id: &str,
) -> Result<&'a DelegateAgentRuntime> {
    delegate_agents
        .iter()
        .find(|agent| agent.id == id)
        .ok_or_else(|| anyhow!("unsupported delegate agent '{}'", id))
}

async fn send_delegate_prompt(shell_mgr: &ShellManager, pane_id: &str, prompt: &str) -> Result<()> {
    ensure_non_empty("prompt", prompt)?;
    sleep(Duration::from_millis(700)).await;
    shell_mgr
        .wt_send_input(pane_id, &format!("{prompt}\r"))
        .await
        .with_context(|| format!("failed to send delegate prompt to pane {}", pane_id))?;
    Ok(())
}

fn normalize_direction(direction: Option<&str>) -> Result<Option<&str>> {
    match direction {
        None => Ok(None),
        Some("right" | "left" | "up" | "down" | "automatic") => Ok(direction),
        Some(other) => bail!("unsupported panel direction '{}'", other),
    }
}

fn ensure_non_empty(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("field '{}' must not be empty", field);
    }
    Ok(())
}

fn value_to_string(value: Option<&serde_json::Value>) -> Option<String> {
    match value {
        Some(serde_json::Value::String(s)) => Some(s.clone()),
        Some(serde_json::Value::Number(n)) => Some(n.to_string()),
        _ => None,
    }
}

fn extract_json_code_block(text: &str) -> Option<&str> {
    let start = text.find("```json").or_else(|| text.find("```JSON"))?;
    let after_marker = &text[start + 7..];
    let trimmed = after_marker.strip_prefix('\r').unwrap_or(after_marker);
    let trimmed = trimmed.strip_prefix('\n').unwrap_or(trimmed);
    let end = trimmed.find("```")?;
    Some(trimmed[..end].trim())
}

fn extract_first_json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end <= start {
        return None;
    }
    Some(text[start..=end].trim())
}
