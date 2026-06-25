//! Model-list extraction and model-switch dispatch across the two ways an
//! ACP agent can advertise its model selector.
//!
//! * **Legacy channel** — `NewSessionResponse.models` (a `SessionModelState`)
//!   plus the `session/set_model` method. Used by Copilot, Gemini, and the
//!   deprecated `@zed-industries/claude-code-acp` adapter.
//! * **Config-option channel** — `NewSessionResponse.config_options[]` with a
//!   `Select` entry whose category is `Model`, switched via
//!   `session/set_config_option`. Used by the renamed
//!   `@agentclientprotocol/claude-agent-acp` adapter (>= 0.24), which returns
//!   `Method not found` for `session/set_model`.
//!
//! A single `wta` process drives exactly one agent CLI, so the channel is
//! uniform for the whole process: [`models_from_new_session`] records it the
//! first time a `new_session` response is parsed and [`apply_session_model`]
//! reads it back when the user hot-swaps the model from a decoupled call site.

use std::sync::RwLock;

use agent_client_protocol as acp;
use agent_client_protocol::Agent as _;

use crate::app::AcpModelInfo;

/// How the current agent expects a model switch to be delivered. Refreshed on
/// every `new_session` parse (see [`models_from_new_session`]) so an in-process
/// agent restart — or a session whose model selector advertises a different
/// config-option id — is always reflected, rather than frozen at first write.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ModelSwitchChannel {
    /// Legacy `session/set_model`.
    Legacy,
    /// `session/set_config_option` carrying this config id (e.g. `"model"`).
    Config { config_id: String },
}

/// The switch channel for this process's currently-connected agent. One `wta`
/// process drives one agent CLI, but the agent can restart in-process and a
/// later `new_session` may advertise a different channel/id — so this is a
/// mutable cell overwritten on every extraction, not a write-once latch.
static MODEL_SWITCH: RwLock<ModelSwitchChannel> = RwLock::new(ModelSwitchChannel::Legacy);

fn record_channel_legacy() {
    *MODEL_SWITCH.write().unwrap() = ModelSwitchChannel::Legacy;
}

fn record_channel_config(config_id: &str) {
    *MODEL_SWITCH.write().unwrap() = ModelSwitchChannel::Config {
        config_id: config_id.to_string(),
    };
}

/// Extract the model list and current model id from a `new_session` response,
/// preferring the legacy `models` field and falling back to a `config_options`
/// `Select` with `category == Model`. Records the switch channel as a side
/// effect so [`apply_session_model`] later dispatches correctly.
pub(crate) fn models_from_new_session(
    resp: &acp::NewSessionResponse,
) -> (Vec<AcpModelInfo>, Option<String>) {
    if let Some(state) = &resp.models {
        record_channel_legacy();
        let models = state
            .available_models
            .iter()
            .map(|m| AcpModelInfo {
                id: m.model_id.0.to_string(),
                name: m.name.clone(),
                description: m.description.clone(),
                kind: crate::agent::ModelKind::Cloud,
            })
            .collect();
        return (models, Some(state.current_model_id.0.to_string()));
    }

    if let Some(opts) = &resp.config_options {
        if let Some((config_id, models, current)) = model_option_from_config(opts) {
            record_channel_config(&config_id);
            return (models, current);
        }
    }

    (Vec::new(), None)
}

/// Find the model selector among a session's config options and flatten it
/// into `(config_id, models, current_model_id)`.
fn model_option_from_config(
    opts: &[acp::SessionConfigOption],
) -> Option<(String, Vec<AcpModelInfo>, Option<String>)> {
    // Pick the first option that is BOTH a model selector AND a Select. A
    // plain `find` on the category/id alone would bail out if a same-named
    // non-Select entry happened to come first, hiding a valid Select later in
    // the list.
    let (opt, sel) = opts.iter().find_map(|o| {
        let is_model = matches!(o.category, Some(acp::SessionConfigOptionCategory::Model))
            || o.id.0.as_ref() == "model";
        if !is_model {
            return None;
        }
        match &o.kind {
            acp::SessionConfigKind::Select(sel) => Some((o, sel)),
            _ => None,
        }
    })?;

    let flat: Vec<&acp::SessionConfigSelectOption> = match &sel.options {
        acp::SessionConfigSelectOptions::Ungrouped(v) => v.iter().collect(),
        acp::SessionConfigSelectOptions::Grouped(groups) => {
            groups.iter().flat_map(|g| g.options.iter()).collect()
        }
        _ => return None,
    };

    let models = flat
        .iter()
        .map(|o| AcpModelInfo {
            id: o.value.0.to_string(),
            name: o.name.clone(),
            description: o.description.clone(),
            kind: crate::agent::ModelKind::Cloud,
        })
        .collect();

    Some((
        opt.id.0.to_string(),
        models,
        Some(sel.current_value.0.to_string()),
    ))
}

/// Switch the model on a live session, routing to `session/set_model` or
/// `session/set_config_option` depending on the channel recorded by
/// [`models_from_new_session`].
pub(crate) async fn apply_session_model(
    conn: &acp::ClientSideConnection,
    session_id: acp::SessionId,
    model_id: String,
) -> acp::Result<()> {
    // Snapshot under the read lock and release it before the await — the lock
    // guard isn't Send and must not be held across the suspension point.
    let channel = MODEL_SWITCH.read().unwrap().clone();
    match channel {
        ModelSwitchChannel::Config { config_id } => conn
            .set_session_config_option(acp::SetSessionConfigOptionRequest::new(
                session_id, config_id, model_id,
            ))
            .await
            .map(|_| ()),
        ModelSwitchChannel::Legacy => conn
            .set_session_model(acp::SetSessionModelRequest::new(session_id, model_id))
            .await
            .map(|_| ()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real `session/new` wire shape from @agentclientprotocol/claude-agent-acp
    // (v0.44): no legacy `models` field — the model selector lives in
    // `configOptions` as a Select with category=model. Captured from
    // wta-acp-debug while validating issue #257.
    const CLAUDE_AGENT_ACP_NEW_SESSION: &str = r#"{
        "sessionId": "dac14599-682e-4a94-b48d-828101d22c05",
        "configOptions": [
            {
                "id": "mode", "name": "Mode", "category": "mode", "type": "select",
                "currentValue": "auto",
                "options": [{"value": "auto", "name": "Auto"}]
            },
            {
                "id": "model", "name": "Model", "description": "AI model to use",
                "category": "model", "type": "select", "currentValue": "default",
                "options": [
                    {"value": "default", "name": "Default (recommended)", "description": "currently Opus"},
                    {"value": "sonnet", "name": "Sonnet"},
                    {"value": "haiku", "name": "Haiku"}
                ]
            }
        ]
    }"#;

    // Legacy shape used by Copilot/Gemini and the deprecated
    // @zed-industries/claude-code-acp adapter.
    const LEGACY_NEW_SESSION: &str = r#"{
        "sessionId": "legacy-1",
        "models": {
            "availableModels": [
                {"modelId": "gpt-5.5", "name": "GPT-5.5"},
                {"modelId": "gpt-5.4", "name": "GPT-5.4"}
            ],
            "currentModelId": "gpt-5.5"
        }
    }"#;

    #[test]
    fn model_extraction_across_channels() {
        // Run sequentially in one test: the recorded switch channel is a
        // process-global, so splitting these into parallel #[test]s would race.

        // 1. New claude-agent-acp: models come from configOptions[category=model]
        //    and the switch channel flips to config-option.
        let resp: acp::NewSessionResponse =
            serde_json::from_str(CLAUDE_AGENT_ACP_NEW_SESSION).expect("valid new_session");
        let (models, current) = models_from_new_session(&resp);
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["default", "sonnet", "haiku"]);
        assert_eq!(current.as_deref(), Some("default"));
        // The model selector — not the "mode" selector — must win.
        assert_eq!(models[0].name, "Default (recommended)");
        assert_eq!(
            *MODEL_SWITCH.read().unwrap(),
            ModelSwitchChannel::Config {
                config_id: "model".to_string()
            }
        );

        // 2. Legacy `models` field wins when present, channel flips back.
        let resp: acp::NewSessionResponse =
            serde_json::from_str(LEGACY_NEW_SESSION).expect("valid new_session");
        let (models, current) = models_from_new_session(&resp);
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["gpt-5.5", "gpt-5.4"]);
        assert_eq!(current.as_deref(), Some("gpt-5.5"));
        assert_eq!(*MODEL_SWITCH.read().unwrap(), ModelSwitchChannel::Legacy);

        // 3. Neither channel present → empty list, no current model.
        let resp: acp::NewSessionResponse =
            serde_json::from_str(r#"{"sessionId": "bare"}"#).expect("valid new_session");
        let (models, current) = models_from_new_session(&resp);
        assert!(models.is_empty());
        assert_eq!(current, None);

        // 4. Model selector identified by category alone (id != "model") is
        //    still found, and a preceding non-model Select is skipped — proves
        //    the find_map matches on the model predicate, not just position.
        let by_category = r#"{
            "sessionId": "cat-1",
            "configOptions": [
                {
                    "id": "mode", "name": "Mode", "category": "mode", "type": "select",
                    "currentValue": "auto", "options": [{"value": "auto", "name": "Auto"}]
                },
                {
                    "id": "llm", "name": "LLM", "category": "model", "type": "select",
                    "currentValue": "haiku",
                    "options": [{"value": "haiku", "name": "Haiku"}]
                }
            ]
        }"#;
        let resp: acp::NewSessionResponse =
            serde_json::from_str(by_category).expect("valid new_session");
        let (models, current) = models_from_new_session(&resp);
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["haiku"]);
        assert_eq!(current.as_deref(), Some("haiku"));
        // The channel id now UPDATES to this session's selector id ("llm"),
        // proving it is no longer frozen at the first-seen "model" id — the
        // exact regression the OnceLock version had.
        assert_eq!(
            *MODEL_SWITCH.read().unwrap(),
            ModelSwitchChannel::Config {
                config_id: "llm".to_string()
            }
        );
    }
}
