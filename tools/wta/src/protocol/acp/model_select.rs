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

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;

use agent_client_protocol as acp;
use agent_client_protocol::Agent as _;

use crate::app::AcpModelInfo;

const SWITCH_VIA_LEGACY: u8 = 0;
const SWITCH_VIA_CONFIG: u8 = 1;

/// How this process's agent expects model switches to be delivered. Defaults
/// to the legacy `session/set_model` path until a `config_options`-style model
/// selector is observed.
static MODEL_SWITCH_VIA: AtomicU8 = AtomicU8::new(SWITCH_VIA_LEGACY);

/// The `config_options` id of the model selector (typically `"model"`), kept
/// so the hot-swap path forwards the exact id the agent advertised.
static MODEL_CONFIG_ID: OnceLock<String> = OnceLock::new();

fn record_channel_legacy() {
    MODEL_SWITCH_VIA.store(SWITCH_VIA_LEGACY, Ordering::Relaxed);
}

fn record_channel_config(config_id: &str) {
    MODEL_SWITCH_VIA.store(SWITCH_VIA_CONFIG, Ordering::Relaxed);
    // First writer wins; the channel is uniform per process so a later session
    // would carry the same id anyway.
    let _ = MODEL_CONFIG_ID.set(config_id.to_string());
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
    let opt = opts.iter().find(|o| {
        matches!(o.category, Some(acp::SessionConfigOptionCategory::Model))
            || o.id.0.as_ref() == "model"
    })?;

    let sel = match &opt.kind {
        acp::SessionConfigKind::Select(sel) => sel,
        _ => return None,
    };

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
    if MODEL_SWITCH_VIA.load(Ordering::Relaxed) == SWITCH_VIA_CONFIG {
        let config_id = MODEL_CONFIG_ID
            .get()
            .map(String::as_str)
            .unwrap_or("model");
        conn.set_session_config_option(acp::SetSessionConfigOptionRequest::new(
            session_id, config_id, model_id,
        ))
        .await
        .map(|_| ())
    } else {
        conn.set_session_model(acp::SetSessionModelRequest::new(session_id, model_id))
            .await
            .map(|_| ())
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
        assert_eq!(MODEL_SWITCH_VIA.load(Ordering::Relaxed), SWITCH_VIA_CONFIG);
        assert_eq!(MODEL_CONFIG_ID.get().map(String::as_str), Some("model"));

        // 2. Legacy `models` field wins when present, channel flips back.
        let resp: acp::NewSessionResponse =
            serde_json::from_str(LEGACY_NEW_SESSION).expect("valid new_session");
        let (models, current) = models_from_new_session(&resp);
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["gpt-5.5", "gpt-5.4"]);
        assert_eq!(current.as_deref(), Some("gpt-5.5"));
        assert_eq!(MODEL_SWITCH_VIA.load(Ordering::Relaxed), SWITCH_VIA_LEGACY);

        // 3. Neither channel present → empty list, no current model.
        let resp: acp::NewSessionResponse =
            serde_json::from_str(r#"{"sessionId": "bare"}"#).expect("valid new_session");
        let (models, current) = models_from_new_session(&resp);
        assert!(models.is_empty());
        assert_eq!(current, None);
    }
}
